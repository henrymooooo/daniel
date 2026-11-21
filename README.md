# Wireless Tactile Desk Media Controller

A wireless, battery-powered desk controller for Spotify and any Windows 11 media app, featuring mechanical switches, a rotary volume/mute knob, and a 1.3" OLED display showing the active track and live synced lyrics.

## Hardware BOM

| Component | Exact Part | Role |
|---|---|---|
| **MCU** | Seeed Studio **XIAO ESP32-C3** | BLE 5.0 + Wi-Fi, USB-C, built-in LiPo charger (`BAT+`/`BAT-` pads) |
| **Display** | 1.3" 4-pin I2C OLED (`128×64`, **SH1106**, White) | Track title, artist, progress bar, synced lyrics |
| **Knob** | **EC11** 5-pin rotary encoder module + cap | Twist = active app volume, click = mute/pause |
| **Switches** | 3× **Outemu Brown** tactile switches + 1U XDA PBT caps | **Prev**, **Play/Pause**, **Next** |
| **Battery** | 3.7V **2000mAh** 1S LiPo (`103450`, protected) | Wireless desk power (1–2+ weeks target with auto-sleep) |
| **Prototyping** | 400-pin MB102 breadboard, Dupont wires, 60W soldering iron | Bench assembly and wiring |

## XIAO ESP32-C3 Pinout Plan

All 8 external signal lines map cleanly onto the XIAO ESP32-C3's **8 non-strapping GPIOs**, avoiding boot-mode strapping pins (`GPIO2`, `GPIO8`, `GPIO9`).

| XIAO Pin | ESP32-C3 GPIO | Connected To | Notes |
|---|---|---|---|
| `D4` | `GPIO6` | OLED `SDA` | I2C Data (default XIAO I2C SDA) |
| `D5` | `GPIO7` | OLED `SCL` | I2C Clock (default XIAO I2C SCL) |
| `D6` | `GPIO21` | EC11 `CLK` (A) | Quadrature phase A |
| `D7` | `GPIO20` | EC11 `DT` (B) | Quadrature phase B |
| `D10` | `GPIO10` | EC11 `SW` | Encoder push-button (`INPUT_PULLUP`) |
| `D1` | `GPIO3` | **Prev** switch | Mechanical switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `D2` | `GPIO4` | **Play/Pause** switch | Mechanical switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `D3` | `GPIO5` | **Next** switch | Mechanical switch to `GND` (`INPUT_PULLUP`, RTC wake capable) |
| `3V3` / `GND` | Power | OLED `VCC`/`GND`, EC11 `+`/`GND` | **3.3V logic only** |
| `BAT+` / `BAT-` | Underside pads | 2000mAh LiPo (Phase 3) | Verify polarity before soldering |

### Reserved Unused Pins & Hardware Safety Warnings

- **Reserved unused pins:** `D0` (`GPIO2`), `D8` (`GPIO8`), `D9` (`GPIO9`) are ESP32-C3 boot strapping pins and are intentionally left unconnected (`D0` also remains available if an optional battery voltage divider is added later).
- **Never connect EC11 `+` to `5V`:** ESP32-C3 GPIOs are **3.3V only** (not 5V tolerant). Power both the OLED and EC11 module strictly from `3V3`.
- **Battery polarity (`BAT+` / `BAT-`):** The XIAO ESP32-C3 underside battery pads have **no reverse-polarity protection**. Always double-check red (`+`) -> `BAT+` and black (`-`) -> `BAT-` with a multimeter or visual check before soldering in Phase 3.
- **External antenna:** Always snap the included external **u.FL antenna** onto the XIAO ESP32-C3 before testing BLE or Wi-Fi (running the radio without an antenna degrades range and can stress the RF front end).

## Roadmap

### Phase 0 — Windows Companion App (Rust, on PC while parts ship)
- **Milestone 0.1 (Current):** Windows SMTC (`Windows.Media.Control`) live media reader + playback timestamp interpolation + terminal dashboard.
- **Milestone 0.2:** LRCLIB (`lrclib.net`) `.lrc` synced lyrics fetcher, parser, local cache, and `128×64` OLED layout preview.
- **Milestone 0.3:** Media transport control (Play/Pause/Next/Prev) + per-app WASAPI session volume control for the active media app.
- **Milestone 0.4:** BLE GATT service protocol & mock transport layer ready for the ESP32-C3.

### Phase 1 — USB Breadboard Test
- Solder XIAO headers, wire SH1106 OLED + EC11 on the MB102 breadboard, and verify display + rotary encoder over USB.

### Phase 2 — Wireless Lyrics over BLE
- Connect the XIAO ESP32-C3 to the Windows companion app over BLE to stream live track metadata + synced lyrics and send media/volume inputs back.

### Phase 3 — Fully Wireless Build
- Solder the 3× Outemu Brown switches and 2000mAh LiPo (`BAT+`/`BAT-`), then enable OLED timeout + ESP32 light/deep sleep.

## Quick Start (Milestone 0.1)

Start Spotify, YouTube, or another media app on Windows 11, then run:

```powershell
cargo run --manifest-path companion/Cargo.toml
```

For a single snapshot check:

```powershell
cargo run --manifest-path companion/Cargo.toml -- --once
```
