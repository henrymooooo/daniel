# Wireless desk media controller

A battery-powered wireless desk controller for Spotify and other Windows 11 media apps. It has three mechanical switches, a rotary volume knob, and a 1.3" OLED screen for the current track and synced lyrics.

## Hardware BOM

| Component | Part | Role |
|---|---|---|
| MCU | Seeed Studio XIAO ESP32-C3 | BLE 5.0 and Wi-Fi, USB-C, built-in LiPo charger (`BAT+`/`BAT-` pads) |
| Display | 1.3" 4-pin I2C OLED (`128×64`, SH1106, White) | Track title, artist, progress bar, synced lyrics |
| Knob | EC11 5-pin rotary encoder module + cap | Twist adjusts active app volume, click toggles mute/pause |
| Switches | 3x Outemu Brown tactile switches + 1U XDA PBT caps | Prev, Play/Pause, Next |
| Battery | 3.7V 2000mAh 1S LiPo (`103450`, protected) | Wireless desk power (targets 1 to 2+ weeks per charge with auto-sleep) |
| Prototyping | 400-pin MB102 breadboard, Dupont wires, 60W soldering iron | Bench assembly and wiring |

## XIAO ESP32-C3 pinout

All 8 signal lines use the XIAO ESP32-C3's 8 non-strapping GPIOs, leaving the boot strapping pins (`GPIO2`, `GPIO8`, `GPIO9`) unused.

| XIAO Pin | ESP32-C3 GPIO | Connected to | Notes |
|---|---|---|---|
| `D4` | `GPIO6` | OLED `SDA` | Default XIAO I2C SDA |
| `D5` | `GPIO7` | OLED `SCL` | Default XIAO I2C SCL |
| `D6` | `GPIO21` | EC11 `CLK` (A) | Quadrature phase A |
| `D7` | `GPIO20` | EC11 `DT` (B) | Quadrature phase B |
| `D10` | `GPIO10` | EC11 `SW` | Encoder push-button (`INPUT_PULLUP`) |
| `D1` | `GPIO3` | Prev switch | Switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `D2` | `GPIO4` | Play/Pause switch | Switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `D3` | `GPIO5` | Next switch | Switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `3V3` / `GND` | Power | OLED `VCC`/`GND`, EC11 `+`/`GND` | 3.3V logic only |
| `BAT+` / `BAT-` | Underside pads | 2000mAh LiPo (Phase 3) | Verify polarity before soldering |

### Unused pins and wiring warnings

- `D0` (`GPIO2`), `D8` (`GPIO8`), and `D9` (`GPIO9`) are ESP32-C3 boot strapping pins, so leave them unconnected. Keeping `D0` free also leaves an ADC pin open if you add a battery voltage divider later.
- Power both the OLED and the EC11 module from `3V3`, never `5V`. ESP32-C3 GPIOs are not 5V tolerant.
- The underside `BAT+` and `BAT-` pads have no reverse-polarity protection. Check with a multimeter or visual inspection that red (`+`) goes to `BAT+` and black (`-`) goes to `BAT-` before soldering the battery in Phase 3.
- Snap the included external u.FL antenna onto the XIAO ESP32-C3 before turning on BLE or Wi-Fi. Running the radio without an antenna cuts range and can stress the RF front end.

## Roadmap

### Phase 0: Windows companion app (Rust, built on PC while parts ship)

- Milestone 0.1 (current): Windows SMTC (`Windows.Media.Control`) media reader, playback timestamp interpolation, and live terminal dashboard.
- Milestone 0.2: LRCLIB (`lrclib.net`) `.lrc` synced lyrics fetcher, parser, local cache, and `128×64` OLED layout preview.
- Milestone 0.3: Media controls (Prev, Play/Pause, Next) and per-app WASAPI volume control for the active media app.
- Milestone 0.4: BLE GATT service protocol and mock transport layer for the ESP32-C3.

### Phase 1: USB breadboard test

Solder the XIAO headers, wire the SH1106 OLED and EC11 on the MB102 breadboard, and test the display and knob over USB.

### Phase 2: Wireless lyrics over BLE

Connect the XIAO ESP32-C3 to the Windows companion app over BLE to stream track metadata and synced lyrics, and send button and knob inputs back.

### Phase 3: Fully wireless build

Solder the 3 Outemu Brown switches and the 2000mAh LiPo (`BAT+`/`BAT-`), then turn on OLED timeout and ESP32 light/deep sleep.

## Quick start (Milestone 0.1)

Start Spotify, YouTube, or another media app on Windows 11, then run:

```powershell
cargo run --manifest-path companion/Cargo.toml
```

For a single snapshot check:

```powershell
cargo run --manifest-path companion/Cargo.toml -- --once
```
