use crate::mqtt_log;
use crate::state::{self, PowerLevel, TargetState};
use core::cell::RefCell;
use core::ops::DerefMut;
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::channel::Receiver;
use embassy_time::{Duration, Instant, Ticker};
use heapless::String;
use minimq::Publication;

mod interop {
    /// Various helpers to ensure interoperability between Embassy's async interfaces and minimq's
    /// sync interaces.
    use core::{cell::RefCell, cmp::min};
    use embassy_net::tcp;
    use embassy_time::Instant;
    use embedded_nal::{nb::Error::WouldBlock, SocketAddr, SocketAddrV4};
    use minimq::{broker::IpBroker, Broker};

    #[derive(Debug)]
    #[allow(dead_code)] // Rust doesn't consider derived Debug as field access.
    pub(super) enum SocketError {
        UnexpectedAddr {
            expected: SocketAddr,
            got: SocketAddr,
        },
        UnexpectedSocketId {
            expected: Option<SocketId>,
            got: Option<SocketId>,
        },
        ConnectionReset,
    }

    impl embedded_nal::TcpError for SocketError {
        fn kind(&self) -> embedded_nal::TcpErrorKind {
            match *self {
                SocketError::ConnectionReset => embedded_nal::TcpErrorKind::PipeClosed,
                _ => embedded_nal::TcpErrorKind::Other,
            }
        }
    }

    // Wraps a single embassy_net::tcp::Socket to appear as sync embedded_nal::TcpClientStack.
    // The stack supports only one concurrent connection. The socket connection must be established
    // outside of the BlockingSocketStack, using ensure_connected().
    pub(super) struct BlockingSocketStack<'sock, 'buf> {
        // The wrapped socket.
        socket: &'sock RefCell<tcp::TcpSocket<'buf>>,
        // Remote endpoint the socket corresponds to.
        endpoint: SocketAddr,
        // Id of the socket that the stack currently emulates. Used to track that there is only one
        // active socket.
        current_socket_id: Option<SocketId>,
        // Id of the previously created socket. Incremented every time a new socket is requested, so
        // that sockets are distinguishable.
        last_socket_id: u32,
    }

    // Socket type for an embedded_nal::TcpClientStack wrapper. Contains only an ID which is used
    // for tracking that there is only one open socket.
    #[derive(Debug, Eq, PartialEq, Copy, Clone)]
    pub(super) struct SocketId(u32);

    impl<'sock, 'buf> BlockingSocketStack<'sock, 'buf> {
        pub(super) fn new(
            socket: &'sock RefCell<tcp::TcpSocket<'buf>>,
            endpoint: SocketAddr,
        ) -> BlockingSocketStack<'sock, 'buf> {
            BlockingSocketStack {
                socket,
                endpoint,
                current_socket_id: None,
                last_socket_id: 0,
            }
        }

        // Checks that the passed socket is the socket that the stack currently emulates.
        fn check_socket(&self, got: Option<SocketId>) -> Result<(), SocketError> {
            if got != self.current_socket_id {
                Err(SocketError::UnexpectedSocketId {
                    expected: self.current_socket_id,
                    got,
                })
            } else {
                Ok(())
            }
        }
    }

    // Ensures that the socket is connected to the given endpoint.
    pub(super) async fn ensure_connected(
        socket: &mut tcp::TcpSocket<'_>,
        endpoint: &(embassy_net::IpAddress, u16),
    ) {
        match socket.state() {
            tcp::State::Established => (),
            state => {
                log::info!("Reopening socket; current state: {}", state);
                // Need to reopen.
                socket.abort();
                if let Err(e) = socket.flush().await {
                    log::error!("cannot flush: {:?}", e);
                }
                if let Err(e) = socket.connect(*endpoint).await {
                    log::error!("cannot connect: {:?}", e);
                }
            }
        }
    }

    impl<'sock, 'buf> embedded_nal::TcpClientStack for BlockingSocketStack<'sock, 'buf> {
        type Error = SocketError;
        type TcpSocket = SocketId;

        // Returns a new socket. Because the system emulates only one socket, trying to get a new
        // socket before the previous one was closed returns an error.
        fn socket(&mut self) -> Result<Self::TcpSocket, Self::Error> {
            self.check_socket(None)?;
            self.last_socket_id += 1;
            let new_id = SocketId(self.last_socket_id);
            self.current_socket_id = Some(new_id);
            Ok(new_id)
        }

        // Emulates a socket connection. Because the connection is happening asynchonously outside
        // of the TcpClientStack implementation, this function only checks that the passed endpoint
        // is the expected one and returns WouldBlock if the connection is not established.
        fn connect(
            &mut self,
            socket: &mut Self::TcpSocket,
            remote: SocketAddr,
        ) -> embedded_nal::nb::Result<(), Self::Error> {
            self.check_socket(Some(*socket))?;

            if self.endpoint != remote {
                return Err(embedded_nal::nb::Error::Other(
                    SocketError::UnexpectedAddr {
                        expected: self.endpoint,
                        got: remote,
                    },
                ));
            }

            match self.socket.borrow().state() {
                tcp::State::Established => Ok(()),
                _ => Err(embedded_nal::nb::Error::WouldBlock),
            }
        }

        // Buffers the data into the socket.
        fn send(
            &mut self,
            socket: &mut Self::TcpSocket,
            buffer: &[u8],
        ) -> embedded_nal::nb::Result<usize, Self::Error> {
            self.check_socket(Some(*socket))?;
            let mut socket = self.socket.borrow_mut();
            let send_window = socket.send_capacity() - socket.send_queue();
            if send_window == 0 {
                return Err(embedded_nal::nb::Error::WouldBlock);
            }

            let send_size = min(send_window, buffer.len());
            if send_size == 0 {
                return Ok(0);
            }

            // block_on is fine: the socket has enough space in the buffer, so the future should be
            // ready immediately.
            match embassy_futures::block_on(socket.write(&buffer[..send_size])) {
                Ok(size) => Ok(size),
                Err(tcp::Error::ConnectionReset) => {
                    Err(embedded_nal::nb::Error::Other(SocketError::ConnectionReset))
                }
            }
        }

        // Returns the buffered data from the socket.
        fn receive(
            &mut self,
            socket: &mut Self::TcpSocket,
            buffer: &mut [u8],
        ) -> embedded_nal::nb::Result<usize, Self::Error> {
            self.check_socket(Some(*socket))?;
            let mut socket = self.socket.borrow_mut();
            if !socket.may_recv() {
                // If the server closed the socket (or the connection was closed for other reasons),
                // report it immediately.
                return Err(embedded_nal::nb::Error::Other(SocketError::ConnectionReset));
            }
            if !socket.can_recv() {
                // No data in the buffer.
                return Err(WouldBlock);
            }
            // block_on is fine: there is something in the buffer, so the future should be ready
            // immediately.
            match embassy_futures::block_on(socket.read(buffer)) {
                Ok(size) => Ok(size),
                Err(tcp::Error::ConnectionReset) => {
                    Err(embedded_nal::nb::Error::Other(SocketError::ConnectionReset))
                }
            }
        }

        // Marks the passed socket as closed, and marks the connection is closed. flush() on the
        // socket must be called elsewhere to really close the connection.
        fn close(&mut self, socket: Self::TcpSocket) -> Result<(), Self::Error> {
            self.check_socket(Some(socket))?;
            self.socket.borrow_mut().close();
            self.current_socket_id = None;
            Ok(())
        }
    }

    // Wraps embassy_time to appear as sync embedded_time::Clock.
    pub(super) struct Clock;

    impl embedded_time::Clock for Clock {
        type T = u64;
        const SCALING_FACTOR: embedded_time::rate::Fraction =
            embedded_time::rate::Fraction::new(1, 1000);

        fn try_now(&self) -> Result<embedded_time::Instant<Self>, embedded_time::clock::Error> {
            Ok(embedded_time::Instant::<Self>::new(
                Instant::now().as_millis(),
            ))
        }

        // minimq never uses this function.
        fn new_timer<Dur: embedded_time::duration::Duration>(
            &self,
            _duration: Dur,
        ) -> embedded_time::Timer<
            embedded_time::timer::param::OneShot,
            embedded_time::timer::param::Armed,
            Self,
            Dur,
        >
        where
            Dur: embedded_time::fixed_point::FixedPoint,
        {
            unreachable!()
        }
    }

    // Parses endpoint (4 octets of an IP address and a port) to all library-specific types that
    // the parent module has to work with.
    // Returns (embassy_net endpoint, embedded_nal endpoint, minimq endpoint).
    pub(super) fn parse_endpoint(
        (ip, port): ((u8, u8, u8, u8), u16),
    ) -> ((embassy_net::IpAddress, u16), SocketAddr, IpBroker) {
        let embedded_nal_ip = embedded_nal::Ipv4Addr::new(ip.0, ip.1, ip.2, ip.3);

        (
            (embassy_net::IpAddress::v4(ip.0, ip.1, ip.2, ip.3), port),
            SocketAddr::V4(SocketAddrV4::new(embedded_nal_ip, port)),
            {
                let mut broker = IpBroker::new(embedded_nal::IpAddr::V4(embedded_nal_ip));
                broker.set_port(port);
                broker
            },
        )
    }
}

// A command that the device can receive over MQTT.
#[derive(Debug)]
enum MqttCommand {
    Unknown,
    Set(TargetState),
}

// Converts a raw incoming message into a parsed command.
fn process_incoming(
    topic: &str,
    msg: &[u8],
    mqtt_topics: &crate::config::MqttTopics,
) -> MqttCommand {
    if topic == mqtt_topics.set {
        match msg {
            b"off" => MqttCommand::Set(TargetState::Off),
            b"low" => MqttCommand::Set(TargetState::On(PowerLevel::Low)),
            b"medium" => MqttCommand::Set(TargetState::On(PowerLevel::Medium)),
            b"high" => MqttCommand::Set(TargetState::On(PowerLevel::High)),
            _ => {
                mqtt_log!("Received unknown set command: {:?}", msg);
                MqttCommand::Unknown
            }
        }
    } else if topic == mqtt_topics.cmd {
        match msg {
            [b'p', b'i', b'n', b'g', b' ', ping @ ..] => {
                // TODO: Print as a string?
                mqtt_log!("Pong: {:?}", ping);
                MqttCommand::Unknown
            }
            _ => {
                mqtt_log!("Received unknown cmd command: {:?}", msg);
                MqttCommand::Unknown
            }
        }
    } else {
        mqtt_log!("Received unknown topic: {}", topic);
        MqttCommand::Unknown
    }
}

const STATE_UPDATE_PERIOD: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub(super) async fn minimq_task(
    network_stack: &'static embassy_net::Stack<cyw43::NetDriver<'static>>,
    topics: &'static crate::config::MqttTopics,
    endpoint: ((u8, u8, u8, u8), u16),
    log_receiver: Receiver<'static, ThreadModeRawMutex, String<256>, 16>,
) {
    // This warning triggers for the ensure_connected() call, but for some reason I couldn't attach
    // the annotation to the statement where the warning is happening.
    // Holding a mutable reference across the await point is fine there, because all other uses of
    // the socket are happening in mimimq poll and publish after ensure_connected() return.
    // TODO: Find a way to attach the annotation to the statement.
    #![allow(clippy::await_holding_refcell_ref)]

    let (emb_endpoint, enal_endpoint, minimq_endpoint) = interop::parse_endpoint(endpoint);

    let mut socket_rx_buffer = [0; 4096];
    let mut socket_tx_buffer = [0; 4096];
    // RefCell is accessed mutably either in ensure_connected() or in BlockingSocketStack::* called
    // by Minimq::poll() and other Minimq functions. Because these are never called concurrently,
    // it should be safe.
    let socket = RefCell::new(TcpSocket::new(
        network_stack,
        &mut socket_rx_buffer,
        &mut socket_tx_buffer,
    ));

    let blocking_stack = interop::BlockingSocketStack::new(&socket, enal_endpoint);

    let mut minimq_buffer = [0; 8192];
    let mut minimq = minimq::Minimq::new(
        blocking_stack,
        interop::Clock,
        minimq::ConfigBuilder::new(minimq_endpoint, &mut minimq_buffer)
            .client_id("f58mqtt")
            .unwrap(),
    );

    let mut last_published_state = (Instant::now(), state::DeviceState::Unknown);

    let mut ticker = Ticker::every(Duration::from_secs(1));
    let mut need_resubscribe = true;
    loop {
        interop::ensure_connected(socket.borrow_mut().deref_mut(), &emb_endpoint).await;

        match minimq.poll(|_, topic, msg, _| process_incoming(topic, msg, topics)) {
            Ok(None) => {
                // No command.
            }
            Ok(Some(MqttCommand::Set(state))) => {
                // Received a command.
                log::info!("Received a command: Set({:?})", state);
                state::set_target_state(state).await;
            }
            Ok(Some(MqttCommand::Unknown)) => {
                // Unknown command was already logged in the process_incoming() implementation.
            }
            Err(minimq::Error::SessionReset) => {
                mqtt_log!("MQTT connection was reset!");
                need_resubscribe = true;
            }
            Err(err) => {
                // Not logging to MQTT to avoid cascading growth of publications if the poll() error
                // is caused by trying to publish logs.
                log::warn!("Error from minimq::poll(): {:?}", err)
            }
        }

        // minimq ignores publish() calls if it is not connected to the broker 🤦‍♀️. So trying to
        // publish while not connected does not make sense.
        if minimq.client().is_connected() {
            if need_resubscribe {
                match minimq
                    .client()
                    .subscribe(&[topics.set.into(), topics.cmd.into()], &[])
                {
                    Ok(()) => need_resubscribe = false,
                    Err(err) => log::warn!("Error subscribing to topics: {:?}", err),
                }
            }

            // Drain the logs channel and publish everything.
            while let Ok(log_message) = log_receiver.try_receive() {
                match minimq.client().publish(
                    Publication::new(log_message.as_bytes())
                        .topic(topics.log)
                        .finish()
                        .unwrap(),
                ) {
                    Ok(()) => {}
                    Err(err) => log::warn!("Error publishing logs: {:?}", err),
                }
            }

            // if there was no state update for some time, or the state changed since the last
            // update, publish it.
            let now = Instant::now();
            let new_state = state::get_current_state(now).await;
            if now.duration_since(last_published_state.0) > STATE_UPDATE_PERIOD
                || (last_published_state.1 != new_state && new_state != state::DeviceState::Unknown)
            {
                match minimq.client().publish(
                    Publication::new(new_state.as_bytes())
                        .topic(topics.state)
                        .retain()
                        .finish()
                        .unwrap(),
                ) {
                    Ok(()) => last_published_state = (now, new_state),
                    Err(err) => log::info!("Error publishing state: {:?}", err),
                }
            }
        }

        ticker.next().await;
    }
}
