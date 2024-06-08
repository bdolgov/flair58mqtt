/// Constructs configuration that will be built into the firmware from environment variables.
///
/// Supported variables:
///
/// * `$F58_WIFI_NETWORK`: SSID of the WiFi network.
/// * `$F58_WIFI_PASSWORD`: WPA2 passphrase of the network.
/// * `$F58_MQTT_ENDPOINT`: IPv4 address and port of the MQTT broker (in `a.b.c.d:p` form).
/// * `$F58_MQTT_PREFIX`: Prefix for all MQTT topics used by the firmware. Defaults to `f58`.
pub(crate) struct WifiConfig {
    pub wifi_network: &'static str,
    pub wifi_password: &'static str,
}

// Full topic names.
pub(crate) struct MqttTopics {
    pub cmd: &'static str,
    pub log: &'static str,
    pub set: &'static str,
    pub state: &'static str,
}

pub(crate) struct Config {
    pub wifi_config: WifiConfig,
    pub mqtt_topics: MqttTopics,
    pub mqtt_endpoint: ((u8, u8, u8, u8), u16),
}

const MQTT_PREFIX: &str = if let Some(mqtt_prefix) = option_env!("F58_MQTT_PREFIX") {
    mqtt_prefix
} else {
    "f58"
};

pub const CONFIG: Config = Config {
    wifi_config: WifiConfig {
        wifi_network: env!(
            "F58_WIFI_NETWORK",
            "Set $F58_WIFI_NETWORK to the network name"
        ),
        wifi_password: env!(
            "F58_WIFI_PASSWORD",
            "Set $F58_WIFI_PASSWORD to the network name"
        ),
    },
    mqtt_topics: MqttTopics {
        cmd: const_format::concatcp!(MQTT_PREFIX, "/cmd"),
        log: const_format::concatcp!(MQTT_PREFIX, "/log"),
        set: const_format::concatcp!(MQTT_PREFIX, "/set"),
        state: const_format::concatcp!(MQTT_PREFIX, "/state"),
    },
    mqtt_endpoint: parse_endpoint(env!(
        "F58_MQTT_ENDPOINT",
        "Set $F58_MQTT_ENDPOINT to ipv4addr:port of the MQTT broker"
    )),
};

// Parses IPv4 endpoint in a form of `a.b.c.d:port` in compile time.
const fn parse_endpoint(endpoint: &str) -> ((u8, u8, u8, u8), u16) {
    let bytes = endpoint.as_bytes();
    let mut parts = [0u64; 5];

    let mut i = 0;
    let mut part_idx = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' || bytes[i] == b':' {
            part_idx += 1;
            assert!(part_idx <= 4);
        } else if bytes[i].is_ascii_digit() {
            parts[part_idx] = parts[part_idx] * 10 + (bytes[i] - b'0') as u64;
        } else {
            panic!("unexpected character in $F58_MQTT_ENDPOINT");
        }
        i += 1;
    }

    assert!(
        parts[0] < 256 && parts[1] < 256 && parts[2] < 256 && parts[3] < 256 && parts[4] < 65536
    );
    (
        (
            parts[0] as u8,
            parts[1] as u8,
            parts[2] as u8,
            parts[3] as u8,
        ),
        parts[4] as u16,
    )
}
