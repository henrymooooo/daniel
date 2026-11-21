/// Advertised BLE GAP device name for the XIAO ESP32-C3 desk controller.
#[allow(dead_code)]
pub const BLE_DEVICE_NAME: &str = "DeskMediaCtrl";

/// Custom BLE GATT service UUID shared between the Windows companion and ESP32-C3 firmware.
#[allow(dead_code)]
pub const BLE_SERVICE_UUID: &str = "8d720001-4c9a-4f3b-9e61-2c8a71b0f4e1";

/// GATT RX characteristic UUID (PC -> ESP32 Write / Write Without Response).
#[allow(dead_code)]
pub const BLE_RX_CHAR_UUID: &str = "8d720002-4c9a-4f3b-9e61-2c8a71b0f4e1";

/// GATT TX characteristic UUID (ESP32 -> PC Notify).
#[allow(dead_code)]
pub const BLE_TX_CHAR_UUID: &str = "8d720003-4c9a-4f3b-9e61-2c8a71b0f4e1";

pub const PROTO_VERSION: u8 = 1;
#[allow(dead_code)]
pub const MAX_BLE_PACKET_BYTES: usize = 128;

const OP_TRACK_META: u8 = 0x01;
const OP_LYRIC_LINE: u8 = 0x02;
const OP_PLAYBACK_STATE: u8 = 0x03;
const OP_POWER_HINT: u8 = 0x04;

const OP_DEVICE_BUTTON: u8 = 0x10;
const OP_DEVICE_ENCODER: u8 = 0x11;
const OP_DEVICE_HELLO: u8 = 0x12;

const MAX_TITLE_BYTES: usize = 48;
const MAX_ARTIST_BYTES: usize = 40;
const MAX_APP_BYTES: usize = 20;
const MAX_LYRIC_BYTES: usize = 96;

/// Display mode for the lyric region on the SH1106 OLED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LyricDisplayMode {
    SyncedText = 0,
    Loading = 1,
    Instrumental = 2,
    Intro = 3,
    PlainOnly = 4,
    NotFound = 5,
    Break = 6,
    Error = 7,
}

impl LyricDisplayMode {
    #[allow(dead_code)]
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::SyncedText),
            1 => Some(Self::Loading),
            2 => Some(Self::Instrumental),
            3 => Some(Self::Intro),
            4 => Some(Self::PlainOnly),
            5 => Some(Self::NotFound),
            6 => Some(Self::Break),
            7 => Some(Self::Error),
            _ => None,
        }
    }
}

/// Power state hint sent to the ESP32-C3 so it can dim or sleep when idle.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PowerMode {
    Awake = 0,
    Dim = 1,
    ScreenOff = 2,
    Sleep = 3,
}

impl PowerMode {
    #[allow(dead_code)]
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::Awake),
            1 => Some(Self::Dim),
            2 => Some(Self::ScreenOff),
            3 => Some(Self::Sleep),
            _ => None,
        }
    }
}

/// Packets sent from the Windows companion app to the desk controller (PC -> ESP32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPacket {
    TrackMeta {
        duration_sec: u16,
        title: String,
        artist: String,
        app: String,
    },
    LyricLine {
        mode: LyricDisplayMode,
        text: String,
    },
    PlaybackState {
        is_playing: bool,
        is_muted: bool,
        show_volume_overlay: bool,
        has_session: bool,
        has_volume: bool,
        position_sec: u16,
        duration_sec: u16,
        volume_percent: u8,
    },
    PowerHint {
        mode: PowerMode,
    },
}

impl HostPacket {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::TrackMeta {
                duration_sec,
                title,
                artist,
                app,
            } => {
                let title_t = truncate_utf8_bytes(title, MAX_TITLE_BYTES);
                let artist_t = truncate_utf8_bytes(artist, MAX_ARTIST_BYTES);
                let app_t = truncate_utf8_bytes(app, MAX_APP_BYTES);

                let mut buf = Vec::with_capacity(
                    6 + title_t.len() + artist_t.len() + app_t.len(),
                );
                buf.push(OP_TRACK_META);
                buf.extend_from_slice(&duration_sec.to_be_bytes());
                push_lp_str(&mut buf, &title_t);
                push_lp_str(&mut buf, &artist_t);
                push_lp_str(&mut buf, &app_t);
                buf
            }
            Self::LyricLine { mode, text } => {
                let text_t = truncate_utf8_bytes(text, MAX_LYRIC_BYTES);
                let mut buf = Vec::with_capacity(3 + text_t.len());
                buf.push(OP_LYRIC_LINE);
                buf.push(*mode as u8);
                push_lp_str(&mut buf, &text_t);
                buf
            }
            Self::PlaybackState {
                is_playing,
                is_muted,
                show_volume_overlay,
                has_session,
                has_volume,
                position_sec,
                duration_sec,
                volume_percent,
            } => {
                let mut flags = 0u8;
                if *is_playing {
                    flags |= 1 << 0;
                }
                if *is_muted {
                    flags |= 1 << 1;
                }
                if *show_volume_overlay {
                    flags |= 1 << 2;
                }
                if *has_session {
                    flags |= 1 << 3;
                }
                if *has_volume {
                    flags |= 1 << 4;
                }

                let mut buf = Vec::with_capacity(7);
                buf.push(OP_PLAYBACK_STATE);
                buf.push(flags);
                buf.extend_from_slice(&position_sec.to_be_bytes());
                buf.extend_from_slice(&duration_sec.to_be_bytes());
                buf.push((*volume_percent).min(100));
                buf
            }
            Self::PowerHint { mode } => vec![OP_POWER_HINT, *mode as u8],
        }
    }

    #[allow(dead_code)]
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let Some(&opcode) = bytes.first() else {
            return Err("empty host packet".to_string());
        };

        match opcode {
            OP_TRACK_META => {
                if bytes.len() < 4 {
                    return Err("truncated TrackMeta header".to_string());
                }
                let duration_sec = u16::from_be_bytes([bytes[1], bytes[2]]);
                let mut cursor = 3usize;
                let title = read_lp_str(bytes, &mut cursor, "title")?;
                let artist = read_lp_str(bytes, &mut cursor, "artist")?;
                let app = read_lp_str(bytes, &mut cursor, "app")?;
                if cursor != bytes.len() {
                    return Err("trailing bytes in TrackMeta".to_string());
                }
                Ok(Self::TrackMeta {
                    duration_sec,
                    title,
                    artist,
                    app,
                })
            }
            OP_LYRIC_LINE => {
                if bytes.len() < 3 {
                    return Err("truncated LyricLine header".to_string());
                }
                let mode = LyricDisplayMode::from_u8(bytes[1])
                    .ok_or_else(|| format!("unknown LyricDisplayMode {}", bytes[1]))?;
                let mut cursor = 2usize;
                let text = read_lp_str(bytes, &mut cursor, "lyric text")?;
                if cursor != bytes.len() {
                    return Err("trailing bytes in LyricLine".to_string());
                }
                Ok(Self::LyricLine { mode, text })
            }
            OP_PLAYBACK_STATE => {
                if bytes.len() != 7 {
                    return Err(format!(
                        "PlaybackState must be 7 bytes, got {}",
                        bytes.len()
                    ));
                }
                let flags = bytes[1];
                Ok(Self::PlaybackState {
                    is_playing: (flags & (1 << 0)) != 0,
                    is_muted: (flags & (1 << 1)) != 0,
                    show_volume_overlay: (flags & (1 << 2)) != 0,
                    has_session: (flags & (1 << 3)) != 0,
                    has_volume: (flags & (1 << 4)) != 0,
                    position_sec: u16::from_be_bytes([bytes[2], bytes[3]]),
                    duration_sec: u16::from_be_bytes([bytes[4], bytes[5]]),
                    volume_percent: bytes[6].min(100),
                })
            }
            OP_POWER_HINT => {
                if bytes.len() != 2 {
                    return Err(format!("PowerHint must be 2 bytes, got {}", bytes.len()));
                }
                let mode = PowerMode::from_u8(bytes[1])
                    .ok_or_else(|| format!("unknown PowerMode {}", bytes[1]))?;
                Ok(Self::PowerHint { mode })
            }
            other => Err(format!("unknown host packet opcode 0x{other:02x}")),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::TrackMeta {
                title,
                artist,
                duration_sec,
                ..
            } => format!("TrackMeta(\"{title}\" - \"{artist}\", {duration_sec}s)"),
            Self::LyricLine { mode, text } => format!("LyricLine({mode:?}, \"{}\")", text),
            Self::PlaybackState {
                is_playing,
                is_muted,
                position_sec,
                volume_percent,
                ..
            } => format!(
                "PlaybackState(play={is_playing}, pos={position_sec}s, vol={volume_percent}%{})",
                if *is_muted { " MUTE" } else { "" }
            ),
            Self::PowerHint { mode } => format!("PowerHint({mode:?})"),
        }
    }
}

/// Physical buttons on the desk unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceButton {
    Prev = 1,
    PlayPause = 2,
    Next = 3,
    EncoderClick = 4,
}

impl DeviceButton {
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            1 => Some(Self::Prev),
            2 => Some(Self::PlayPause),
            3 => Some(Self::Next),
            4 => Some(Self::EncoderClick),
            _ => None,
        }
    }
}

/// Events sent from the desk unit to the Windows companion app (ESP32 -> PC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEvent {
    Button(DeviceButton),
    EncoderDelta(i8),
    Hello { proto_ver: u8, battery_pct: u8 },
}

impl DeviceEvent {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Button(btn) => vec![OP_DEVICE_BUTTON, *btn as u8],
            Self::EncoderDelta(delta) => vec![OP_DEVICE_ENCODER, *delta as u8],
            Self::Hello {
                proto_ver,
                battery_pct,
            } => vec![OP_DEVICE_HELLO, *proto_ver, *battery_pct],
        }
    }

    #[allow(dead_code)]
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let Some(&opcode) = bytes.first() else {
            return Err("empty device event".to_string());
        };

        match opcode {
            OP_DEVICE_BUTTON => {
                if bytes.len() != 2 {
                    return Err(format!("Button event must be 2 bytes, got {}", bytes.len()));
                }
                let btn = DeviceButton::from_u8(bytes[1])
                    .ok_or_else(|| format!("unknown DeviceButton {}", bytes[1]))?;
                Ok(Self::Button(btn))
            }
            OP_DEVICE_ENCODER => {
                if bytes.len() != 2 {
                    return Err(format!(
                        "EncoderDelta event must be 2 bytes, got {}",
                        bytes.len()
                    ));
                }
                Ok(Self::EncoderDelta(bytes[1] as i8))
            }
            OP_DEVICE_HELLO => {
                if bytes.len() != 3 {
                    return Err(format!("Hello event must be 3 bytes, got {}", bytes.len()));
                }
                Ok(Self::Hello {
                    proto_ver: bytes[1],
                    battery_pct: bytes[2],
                })
            }
            other => Err(format!("unknown device event opcode 0x{other:02x}")),
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::Button(btn) => format!("Button({btn:?})"),
            Self::EncoderDelta(delta) => format!("EncoderDelta({delta:+})"),
            Self::Hello {
                proto_ver,
                battery_pct,
            } => {
                let bat = if *battery_pct == 255 {
                    "USB".to_string()
                } else {
                    format!("{battery_pct}%")
                };
                format!("Hello(v{proto_ver}, bat={bat})")
            }
        }
    }
}

/// Truncates `s` to at most `max_bytes` of valid UTF-8 without splitting a codepoint.
pub fn truncate_utf8_bytes(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

fn push_lp_str(buf: &mut Vec<u8>, s: &str) {
    let len = s.len().min(u8::MAX as usize);
    buf.push(len as u8);
    buf.extend_from_slice(&s.as_bytes()[..len]);
}

#[allow(dead_code)]
fn read_lp_str(bytes: &[u8], cursor: &mut usize, field: &str) -> Result<String, String> {
    let Some(&len_u8) = bytes.get(*cursor) else {
        return Err(format!("missing length for {field}"));
    };
    *cursor += 1;
    let len = len_u8 as usize;
    let end = cursor.saturating_add(len);
    if end > bytes.len() {
        return Err(format!("truncated {field} string"));
    }
    let slice = &bytes[*cursor..end];
    *cursor = end;
    std::str::from_utf8(slice)
        .map(|s| s.to_string())
        .map_err(|e| format!("invalid UTF-8 in {field}: {e}"))
}

/// Tracks the last-sent protocol packets so the companion only transmits deltas over BLE.
#[derive(Debug, Default)]
pub struct ProtocolSyncState {
    last_track: Option<HostPacket>,
    last_lyric: Option<HostPacket>,
    last_playback: Option<HostPacket>,
    last_power: Option<PowerMode>,
}

impl ProtocolSyncState {
    pub fn reset(&mut self) {
        self.last_track = None;
        self.last_lyric = None;
        self.last_playback = None;
        self.last_power = None;
    }

    pub fn diff_packets(
        &mut self,
        track: Option<HostPacket>,
        lyric: Option<HostPacket>,
        playback: HostPacket,
    ) -> Vec<HostPacket> {
        let mut out = Vec::new();

        if let Some(t) = track
            && self.last_track.as_ref() != Some(&t)
        {
            self.last_track = Some(t.clone());
            out.push(t);
        }

        if let Some(l) = lyric
            && self.last_lyric.as_ref() != Some(&l)
        {
            self.last_lyric = Some(l.clone());
            out.push(l);
        }

        if self.last_playback.as_ref() != Some(&playback) {
            self.last_playback = Some(playback.clone());
            out.push(playback);
        }

        out
    }

    pub fn diff_power(&mut self, mode: PowerMode) -> Option<HostPacket> {
        if self.last_power == Some(mode) {
            None
        } else {
            self.last_power = Some(mode);
            Some(HostPacket::PowerHint { mode })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_all_host_packets_within_mtu() {
        let packets = vec![
            HostPacket::TrackMeta {
                duration_sec: 224,
                title: "In A Good Way (Very Long Title That Gets Clipped Cleanly)".to_string(),
                artist: "Faye Webster".to_string(),
                app: "Spotify".to_string(),
            },
            HostPacket::LyricLine {
                mode: LyricDisplayMode::SyncedText,
                text: "I didn't know I was capable of being happy".to_string(),
            },
            HostPacket::PlaybackState {
                is_playing: true,
                is_muted: false,
                show_volume_overlay: true,
                has_session: true,
                has_volume: true,
                position_sec: 73,
                duration_sec: 224,
                volume_percent: 75,
            },
            HostPacket::PowerHint {
                mode: PowerMode::Awake,
            },
        ];

        for pkt in packets {
            let encoded = pkt.encode();
            assert!(encoded.len() <= MAX_BLE_PACKET_BYTES);
            let decoded = HostPacket::decode(&encoded).unwrap();
            match (&pkt, &decoded) {
                (
                    HostPacket::TrackMeta { duration_sec, .. },
                    HostPacket::TrackMeta {
                        duration_sec: d2,
                        title,
                        artist,
                        app,
                    },
                ) => {
                    assert_eq!(duration_sec, d2);
                    assert!(title.len() <= MAX_TITLE_BYTES);
                    assert!(artist.len() <= MAX_ARTIST_BYTES);
                    assert!(app.len() <= MAX_APP_BYTES);
                }
                _ => assert_eq!(pkt, decoded),
            }
        }
    }

    #[test]
    fn roundtrips_all_device_events_including_negative_delta() {
        let events = vec![
            DeviceEvent::Button(DeviceButton::Prev),
            DeviceEvent::Button(DeviceButton::PlayPause),
            DeviceEvent::Button(DeviceButton::Next),
            DeviceEvent::Button(DeviceButton::EncoderClick),
            DeviceEvent::EncoderDelta(1),
            DeviceEvent::EncoderDelta(-3),
            DeviceEvent::Hello {
                proto_ver: PROTO_VERSION,
                battery_pct: 92,
            },
        ];

        for ev in events {
            let encoded = ev.encode();
            let decoded = DeviceEvent::decode(&encoded).unwrap();
            assert_eq!(ev, decoded);
        }
    }

    #[test]
    fn truncates_multibyte_utf8_without_splitting_codepoints() {
        // Each Japanese char is 3 bytes. 5 bytes -> 1 char (3 bytes), never mid-codepoint.
        let clipped = truncate_utf8_bytes("夜に駆ける", 5);
        assert_eq!(clipped, "夜");
        assert!(std::str::from_utf8(clipped.as_bytes()).is_ok());

        let exact = truncate_utf8_bytes("夜に駆ける", 6);
        assert_eq!(exact, "夜に");
    }

    #[test]
    fn rejects_truncated_and_unknown_packets() {
        assert!(HostPacket::decode(&[]).is_err());
        assert!(HostPacket::decode(&[0x99]).is_err());
        assert!(HostPacket::decode(&[OP_PLAYBACK_STATE, 0, 0]).is_err());
        assert!(HostPacket::decode(&[OP_POWER_HINT, 9]).is_err());
        assert!(HostPacket::decode(&[OP_LYRIC_LINE, 0, 5, b'a', b'b']).is_err());

        assert!(DeviceEvent::decode(&[]).is_err());
        assert!(DeviceEvent::decode(&[0x55, 1]).is_err());
        assert!(DeviceEvent::decode(&[OP_DEVICE_BUTTON, 99]).is_err());
        assert!(DeviceEvent::decode(&[OP_DEVICE_HELLO, 1]).is_err());
    }

    #[test]
    fn sync_state_emits_deltas_only_and_resyncs_on_reset() {
        let mut sync = ProtocolSyncState::default();
        let track = HostPacket::TrackMeta {
            duration_sec: 200,
            title: "Song".to_string(),
            artist: "Artist".to_string(),
            app: "Spotify".to_string(),
        };
        let lyric = HostPacket::LyricLine {
            mode: LyricDisplayMode::SyncedText,
            text: "Line one".to_string(),
        };
        let playback_1 = HostPacket::PlaybackState {
            is_playing: true,
            is_muted: false,
            show_volume_overlay: false,
            has_session: true,
            has_volume: true,
            position_sec: 10,
            duration_sec: 200,
            volume_percent: 70,
        };

        let first = sync.diff_packets(Some(track.clone()), Some(lyric.clone()), playback_1.clone());
        assert_eq!(first.len(), 3);

        // Identical tick -> zero packets.
        let second = sync.diff_packets(Some(track.clone()), Some(lyric.clone()), playback_1.clone());
        assert!(second.is_empty());

        // Position advances 1s -> only PlaybackState packet.
        let playback_2 = HostPacket::PlaybackState {
            is_playing: true,
            is_muted: false,
            show_volume_overlay: false,
            has_session: true,
            has_volume: true,
            position_sec: 11,
            duration_sec: 200,
            volume_percent: 70,
        };
        let third = sync.diff_packets(Some(track.clone()), Some(lyric.clone()), playback_2.clone());
        assert_eq!(third, vec![playback_2.clone()]);

        // Reset (reconnect / Hello) -> sends full snapshot again.
        sync.reset();
        let fourth = sync.diff_packets(Some(track), Some(lyric), playback_2);
        assert_eq!(fourth.len(), 3);
    }
}
