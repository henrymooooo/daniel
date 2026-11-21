# ESP32-C3 Firmware (Phase 1+)

This directory will hold the Arduino C++ firmware for the **Seeed Studio XIAO ESP32-C3** once hardware arrives in Phase 1.

## Wiring Reference (XIAO ESP32-C3)

| XIAO Pin | ESP32-C3 GPIO | Signal | Mode / Notes |
|---|---|---|---|
| `D4` | `GPIO6` | OLED `SDA` | I2C Data (1.3" SH1106) |
| `D5` | `GPIO7` | OLED `SCL` | I2C Clock (1.3" SH1106) |
| `D6` | `GPIO21` | EC11 `CLK` (A) | Rotary encoder A |
| `D7` | `GPIO20` | EC11 `DT` (B) | Rotary encoder B |
| `D10` | `GPIO10` | EC11 `SW` | Push-button (`INPUT_PULLUP`) |
| `D1` | `GPIO3` | **Prev** switch | `INPUT_PULLUP`, RTC wake capable |
| `D2` | `GPIO4` | **Play/Pause** switch | `INPUT_PULLUP`, RTC wake capable |
| `D3` | `GPIO5` | **Next** switch | `INPUT_PULLUP`, RTC wake capable |
| `3V3` / `GND` | Power | OLED `VCC`/`GND`, EC11 `+`/`GND` | Strict 3.3V power rail |
| `BAT+` / `BAT-` | Underside pads | 3.7V 2000mAh 1S LiPo | Phase 3 |

## Safety Checklist Before Powering On

1. **EC11 `+` goes to `3V3`, never `5V`.** ESP32-C3 GPIOs are not 5V tolerant.
2. **Snap on the u.FL antenna** before enabling BLE or Wi-Fi in firmware.
3. **Avoid strapping pins (`D0`/`GPIO2`, `D8`/`GPIO8`, `D9`/`GPIO9`)** so button presses never accidentally trigger bootloader mode on reset.
4. **Verify LiPo polarity** (`+` red to `BAT+`, `-` black to `BAT-`) before soldering in Phase 3 — the underside pads have no reverse-polarity protection.
