# Flair 58 MQTT Gateway

This repository contains firmware to integrate [Flair 58] [Preheat system] with a smart home
automation system using [MQTT] and [Raspberry Pi Pico W] microcontroller.

Disclaimer: controlling heating devices remotely is dangerous; wiring into electrical devices is
dangerous and voids their warranty.

## Pins

See [Pico W Pinout] for the pin names.

* Pin 3 (or any other), `Ground`: Common ground, `GND`.
* Pin 16, `GP12`: Low LED, `D2`.
* Pin 17, `GP13`: Medium LED, `D3`.
* Pin 19, `GP14`: High LED, `D4`.
* Pin 20, `GP15`: Control button, `S1`.

Preheat controller board does not provide enough 5V current for Pico W, so Pico W has to be powered
externally (for example, by USB).

## Home Assistant Config

The following `configuration.yaml` snippet adds two entities to [Home Assistant]:

* A sensor entity, which shows the current state of the preheat controller.
* A select entity, which allows to set the desired state of the preheat controller.

```yaml
mqtt:
  sensor:
    - unique_id: "f58_state"
      name: "Flair58 Current State"
      state_topic: "f58/state"
      device_class: "enum"
  select:
    - unique_id: "f58_target_state"
      name: "Flair58 Target State"
      command_topic: "f58/set"
      retain: true
      options:
        - "off"
        - "low"
        - "medium"
        - "high"
```

[Flair 58]: https://flairespresso.com/products/espresso-makers/flair-58-plus/
[Preheat system]: https://flairespresso.com/product/flair-58-electric-preheat-system/
[MQTT]: https://mqtt.org/
[Raspberry Pi Pico W]: https://www.raspberrypi.com/documentation/microcontrollers/raspberry-pi-pico.html
[Home Assistant]: https://home-assistant.io/
[Pico W Pinout]: https://picow.pinout.xyz/
