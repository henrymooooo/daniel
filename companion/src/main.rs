mod audio;
mod format;
mod input;
mod lyrics;
mod media;
mod oled_preview;
mod protocol;
mod simulator;
mod transport;

use std::collections::VecDeque;
use std::env;
use std::io::{self, BufWriter, Write};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use audio::{AppAudioController, AppVolumeState, DEFAULT_VOLUME_STEP};
use format::{format_duration, friendly_app_name, render_progress_bar, truncate_chars};
use input::{ConsoleInput, InputAction};
use lyrics::{
    LyricsCache, LyricsContent, LyricsRequest, LyricsSource, LyricsState, LyricsWorker,
    active_lyric,
};
use media::{MediaCommand, MediaSnapshot, PlaybackStatus, SmtcReader, TrackId};
use oled_preview::{OledLyricStatus, OledVolumeInfo, render_oled_box};
use protocol::{
    DeviceButton, DeviceEvent, HostPacket, LyricDisplayMode, PowerMode, ProtocolSyncState,
};
use simulator::{DEFAULT_SIMULATOR_PORT, SimulatorTransport};
use transport::{DeviceTransport, NoopTransport, TransportStats};

const INPUT_TICK: Duration = Duration::from_millis(50);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const VOLUME_OVERLAY_TTL: Duration = Duration::from_millis(1500);
const PROGRESS_BAR_WIDTH: usize = 28;
const VOLUME_BAR_WIDTH: usize = 20;
const MAX_RECENT_TRACKS: usize = 3;

struct TrackEvent {
    elapsed: Duration,
    app: String,
    title: String,
    artist: String,
}

struct CliOptions {
    once: bool,
    no_cache: bool,
    offset_ms: i64,
    simulate: bool,
    port: u16,
    help: bool,
}

fn parse_args<I>(args: I) -> Result<CliOptions, String>
where
    I: IntoIterator<Item = String>,
{
    let mut opts = CliOptions {
        once: false,
        no_cache: false,
        offset_ms: 0,
        simulate: false,
        port: DEFAULT_SIMULATOR_PORT,
        help: false,
    };

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => opts.help = true,
            "--once" => opts.once = true,
            "--no-cache" => opts.no_cache = true,
            "--simulate" => opts.simulate = true,
            "--offset-ms" => {
                let val = iter.next().ok_or_else(|| {
                    "--offset-ms requires an integer value (e.g. --offset-ms 250)".to_string()
                })?;
                opts.offset_ms = val.parse::<i64>().map_err(|_| {
                    format!("invalid --offset-ms value '{val}' (expected integer ms)")
                })?;
            }
            other if other.starts_with("--offset-ms=") => {
                let val = &other["--offset-ms=".len()..];
                opts.offset_ms = val.parse::<i64>().map_err(|_| {
                    format!("invalid --offset-ms value '{val}' (expected integer ms)")
                })?;
            }
            "--port" => {
                let val = iter.next().ok_or_else(|| {
                    "--port requires a port number (e.g. --port 47291)".to_string()
                })?;
                opts.port = parse_port(&val)?;
            }
            other if other.starts_with("--port=") => {
                let val = &other["--port=".len()..];
                opts.port = parse_port(val)?;
            }
            other => {
                return Err(format!(
                    "unknown argument '{other}'. Run with --help for usage."
                ));
            }
        }
    }

    Ok(opts)
}

fn parse_port(val: &str) -> Result<u16, String> {
    let p = val
        .parse::<u16>()
        .map_err(|_| format!("invalid --port value '{val}' (expected 1..=65535)"))?;
    if p == 0 {
        return Err("invalid --port value '0' (expected 1..=65535)".to_string());
    }
    Ok(p)
}

fn print_usage() {
    println!("Desk Media Companion — Phase 0 (Milestone 0.4: BLE Protocol & Digital Sim)");
    println!("Usage: desk-media-companion [OPTIONS]");
    println!("  --simulate           Launch the 128x64 pixel web simulator on 127.0.0.1.");
    println!(
        "  --port <PORT>        Simulator HTTP port (default: {}).",
        DEFAULT_SIMULATOR_PORT
    );
    println!("  --once               Poll Windows SMTC once, print snapshot, and exit.");
    println!("  --offset-ms <MS>     Shift lyric timing in ms (positive = show lyrics earlier).");
    println!("  --no-cache           Disable local disk cache and always query lrclib.net.");
    println!("  -h, --help           Show this help message.");
    println!();
    println!("Interactive controls in dashboard mode:");
    println!("  Left / P / J         Previous track");
    println!("  Space / K            Play / Pause");
    println!("  Right / N / L        Next track");
    println!("  Up / Down / + / -    Active media app volume +/-5%");
    println!("  M                    Toggle active media app mute");
    println!("  Q / Esc              Quit");
}

fn main() {
    let opts = match parse_args(env::args().skip(1)) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("Error: {msg}");
            process::exit(2);
        }
    };

    if opts.help {
        print_usage();
        return;
    }

    let mut reader = match SmtcReader::new() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Failed to initialize Windows SMTC reader: {err}");
            process::exit(1);
        }
    };

    if opts.once {
        match reader.poll() {
            Ok(Some(snap)) => print!("{}", render_once_snapshot(&snap)),
            Ok(None) => println!("No active Windows media session detected."),
            Err(err) => {
                eprintln!("Failed to query Windows SMTC session: {err}");
                process::exit(1);
            }
        }
        return;
    }

    run_live_dashboard(&mut reader, &opts);
}

struct DashboardState {
    started: Instant,
    last_track: Option<TrackId>,
    last_snap: Option<MediaSnapshot>,
    last_poll_err: Option<String>,
    recent_tracks: VecDeque<TrackEvent>,
    current_lyrics: LyricsState,
    current_volume: Option<AppVolumeState>,
    volume_overlay_until: Option<Instant>,
    had_overlay: bool,
    last_action_msg: Option<(Duration, String)>,
    force_refresh: bool,
    needs_redraw: bool,
}

impl DashboardState {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            last_track: None,
            last_snap: None,
            last_poll_err: None,
            recent_tracks: VecDeque::with_capacity(MAX_RECENT_TRACKS),
            current_lyrics: LyricsState::Idle,
            current_volume: None,
            volume_overlay_until: None,
            had_overlay: false,
            last_action_msg: None,
            force_refresh: true,
            needs_redraw: true,
        }
    }

    fn apply_action(
        &mut self,
        action: InputAction,
        reader: &SmtcReader,
        audio: &AppAudioController,
        source_tag: &str,
    ) -> bool {
        match action {
            InputAction::Quit => return true,
            InputAction::Prev => {
                let msg = send_media_action(reader, MediaCommand::Prev, "Prev");
                self.last_action_msg = Some((self.started.elapsed(), format!("{source_tag}{msg}")));
                self.force_refresh = true;
                self.needs_redraw = true;
            }
            InputAction::PlayPause => {
                let msg = send_media_action(reader, MediaCommand::PlayPause, "Play/Pause");
                self.last_action_msg = Some((self.started.elapsed(), format!("{source_tag}{msg}")));
                self.force_refresh = true;
                self.needs_redraw = true;
            }
            InputAction::Next => {
                let msg = send_media_action(reader, MediaCommand::Next, "Next");
                self.last_action_msg = Some((self.started.elapsed(), format!("{source_tag}{msg}")));
                self.force_refresh = true;
                self.needs_redraw = true;
            }
            InputAction::VolumeDelta(steps) => {
                let msg = if let Some(snap) = &self.last_snap {
                    let app_label = friendly_app_name(&snap.app_id);
                    match audio.adjust_app_volume(&snap.app_id, steps, DEFAULT_VOLUME_STEP) {
                        Ok(Some(vol)) => {
                            self.current_volume = Some(vol);
                            self.volume_overlay_until =
                                Some(Instant::now() + VOLUME_OVERLAY_TTL);
                            format!(
                                "Vol {} {} ({}%{})",
                                app_label,
                                if steps >= 0 { "+" } else { "-" },
                                vol.percent,
                                if vol.muted { ", MUTED" } else { "" }
                            )
                        }
                        Ok(None) => format!("No WASAPI audio session found for {app_label}"),
                        Err(err) => format!("Volume error for {app_label}: {err}"),
                    }
                } else {
                    "Volume ignored: no active media session".to_string()
                };
                self.last_action_msg = Some((self.started.elapsed(), format!("{source_tag}{msg}")));
                self.needs_redraw = true;
            }
            InputAction::ToggleMute => {
                let msg = if let Some(snap) = &self.last_snap {
                    let app_label = friendly_app_name(&snap.app_id);
                    match audio.toggle_app_mute(&snap.app_id) {
                        Ok(Some(vol)) => {
                            self.current_volume = Some(vol);
                            self.volume_overlay_until =
                                Some(Instant::now() + VOLUME_OVERLAY_TTL);
                            format!(
                                "{} {} ({}%)",
                                app_label,
                                if vol.muted { "MUTED" } else { "UNMUTED" },
                                vol.percent
                            )
                        }
                        Ok(None) => format!("No WASAPI audio session found for {app_label}"),
                        Err(err) => format!("Mute error for {app_label}: {err}"),
                    }
                } else {
                    "Mute ignored: no active media session".to_string()
                };
                self.last_action_msg = Some((self.started.elapsed(), format!("{source_tag}{msg}")));
                self.needs_redraw = true;
            }
        }
        false
    }
}

fn run_live_dashboard(reader: &mut SmtcReader, opts: &CliOptions) {
    let mut transport: Box<dyn DeviceTransport> = if opts.simulate {
        match SimulatorTransport::start(opts.port) {
            Ok(sim) => Box::new(sim),
            Err(err) => {
                eprintln!("{err}");
                process::exit(1);
            }
        }
    } else {
        Box::new(NoopTransport)
    };

    let cache = LyricsCache::new(LyricsCache::default_dir(), !opts.no_cache);
    let worker = LyricsWorker::spawn(cache);
    let audio = AppAudioController;
    let console_input = ConsoleInput::new();

    let mut state = DashboardState::new();
    let mut sync_state = ProtocolSyncState::default();
    let mut stats = TransportStats::default();
    let mut was_connected = false;

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = out.flush();

    let mut last_poll = Instant::now();

    loop {
        state.needs_redraw = false;

        for action in console_input.poll_actions() {
            if state.apply_action(action, reader, &audio, "") {
                return;
            }
        }

        for raw in transport.poll_events() {
            stats.rx_packets += 1;
            stats.rx_bytes += raw.len() as u64;
            match DeviceEvent::decode(&raw) {
                Ok(ev) => {
                    stats.last_rx_summary = Some(ev.summary());
                    match ev {
                        DeviceEvent::Button(DeviceButton::Prev) => {
                            let _ = state.apply_action(InputAction::Prev, reader, &audio, "[BLE] ");
                        }
                        DeviceEvent::Button(DeviceButton::PlayPause) => {
                            let _ = state.apply_action(
                                InputAction::PlayPause,
                                reader,
                                &audio,
                                "[BLE] ",
                            );
                        }
                        DeviceEvent::Button(DeviceButton::Next) => {
                            let _ = state.apply_action(InputAction::Next, reader, &audio, "[BLE] ");
                        }
                        DeviceEvent::Button(DeviceButton::EncoderClick) => {
                            let _ = state.apply_action(
                                InputAction::ToggleMute,
                                reader,
                                &audio,
                                "[BLE] ",
                            );
                        }
                        DeviceEvent::EncoderDelta(delta) => {
                            let _ = state.apply_action(
                                InputAction::VolumeDelta(delta as i32),
                                reader,
                                &audio,
                                "[BLE] ",
                            );
                        }
                        DeviceEvent::Hello { .. } => {
                            sync_state.reset();
                            state.last_action_msg = Some((
                                state.started.elapsed(),
                                format!("[BLE] {} -> full resync", ev.summary()),
                            ));
                            state.needs_redraw = true;
                        }
                    }
                }
                Err(err) => {
                    stats.last_rx_summary = Some(format!("decode error: {err}"));
                    state.needs_redraw = true;
                }
            }
        }

        if state.force_refresh || last_poll.elapsed() >= POLL_INTERVAL {
            state.force_refresh = false;
            last_poll = Instant::now();
            state.needs_redraw = true;

            match reader.poll() {
                Ok(Some(snap)) => {
                    state.last_poll_err = None;
                    let track_id = snap.track_id();
                    if state.last_track.as_ref() != Some(&track_id)
                        && (!snap.title.trim().is_empty() || !snap.artist.trim().is_empty())
                    {
                        if state.recent_tracks.len() == MAX_RECENT_TRACKS {
                            state.recent_tracks.pop_back();
                        }
                        state.recent_tracks.push_front(TrackEvent {
                            elapsed: state.started.elapsed(),
                            app: friendly_app_name(&snap.app_id),
                            title: display_or(&snap.title, "<untitled>"),
                            artist: display_or(&snap.artist, "<unknown artist>"),
                        });
                        state.last_track = Some(track_id.clone());

                        if !snap.title.trim().is_empty() {
                            state.current_lyrics = LyricsState::Loading;
                            let duration_secs = if snap.duration.is_zero() {
                                None
                            } else {
                                Some(snap.duration.as_secs())
                            };
                            worker.request(LyricsRequest {
                                track_id,
                                title: snap.title.clone(),
                                artist: snap.artist.clone(),
                                album: snap.album.clone(),
                                duration_secs,
                            });
                        } else {
                            state.current_lyrics = LyricsState::Idle;
                        }
                    }

                    if let Ok(vol_opt) = audio.get_app_volume(&snap.app_id) {
                        state.current_volume = vol_opt;
                    }
                    state.last_snap = Some(snap);
                }
                Ok(None) => {
                    state.last_poll_err = None;
                    state.last_snap = None;
                    state.current_volume = None;
                    while worker.try_recv().is_some() {}
                }
                Err(err) => {
                    state.last_poll_err = Some(err.to_string());
                }
            }
        }

        while let Some(resp) = worker.try_recv() {
            if state.last_track.as_ref() == Some(&resp.track_id) {
                state.current_lyrics = resp.state;
                state.needs_redraw = true;
            }
        }

        let show_overlay = state
            .volume_overlay_until
            .is_some_and(|until| Instant::now() < until);
        if state.had_overlay && !show_overlay {
            state.volume_overlay_until = None;
            state.needs_redraw = true;
        }
        state.had_overlay = show_overlay;

        let connected = transport.is_connected();
        if connected && !was_connected {
            sync_state.reset();
            state.needs_redraw = true;
        } else if !connected && was_connected {
            state.needs_redraw = true;
        }
        was_connected = connected;

        if connected {
            let (track_pkt, lyric_pkt, playback_pkt) = build_protocol_packets(
                state.last_snap.as_ref(),
                &state.current_lyrics,
                state.current_volume,
                show_overlay,
                opts.offset_ms,
            );
            let mut outbound = sync_state.diff_packets(track_pkt, lyric_pkt, playback_pkt);
            if let Some(power_pkt) = sync_state.diff_power(PowerMode::Awake) {
                outbound.push(power_pkt);
            }
            for pkt in outbound {
                let bytes = pkt.encode();
                if transport.send_packet(&bytes).is_ok() {
                    stats.tx_packets += 1;
                    stats.tx_bytes += bytes.len() as u64;
                    stats.last_tx_summary = Some(pkt.summary());
                }
            }
        }

        if state.needs_redraw {
            let frame = render_dashboard_frame(
                &state,
                opts,
                transport.as_ref(),
                connected,
                &stats,
                show_overlay,
            );
            let _ = out.write_all(frame.as_bytes());
            let _ = out.flush();
        }

        thread::sleep(INPUT_TICK);
    }
}

fn build_protocol_packets(
    snap_opt: Option<&MediaSnapshot>,
    lyrics: &LyricsState,
    volume: Option<AppVolumeState>,
    show_overlay: bool,
    offset_ms: i64,
) -> (Option<HostPacket>, Option<HostPacket>, HostPacket) {
    let Some(snap) = snap_opt else {
        return (
            None,
            None,
            HostPacket::PlaybackState {
                is_playing: false,
                is_muted: false,
                show_volume_overlay: false,
                has_session: false,
                has_volume: false,
                position_sec: 0,
                duration_sec: 0,
                volume_percent: 0,
            },
        );
    };

    let dur_sec = snap.duration.as_secs().min(u16::MAX as u64) as u16;
    let pos_sec = snap.position.as_secs().min(u16::MAX as u64) as u16;
    let track = HostPacket::TrackMeta {
        duration_sec: dur_sec,
        title: display_or(&snap.title, "<untitled>"),
        artist: display_or(&snap.artist, "<unknown artist>"),
        app: friendly_app_name(&snap.app_id),
    };

    let raw_ms = snap.position.as_millis().min(i64::MAX as u128) as i64;
    let effective_pos_ms = raw_ms.saturating_add(offset_ms).max(0) as u64;

    let lyric = match lyrics {
        LyricsState::Idle => HostPacket::LyricLine {
            mode: LyricDisplayMode::NotFound,
            text: String::new(),
        },
        LyricsState::Loading => HostPacket::LyricLine {
            mode: LyricDisplayMode::Loading,
            text: String::new(),
        },
        LyricsState::Error(_) => HostPacket::LyricLine {
            mode: LyricDisplayMode::Error,
            text: String::new(),
        },
        LyricsState::Ready { content, .. } => match content {
            LyricsContent::Synced(lines) => {
                let active = active_lyric(lines, effective_pos_ms);
                match active.current {
                    None => HostPacket::LyricLine {
                        mode: LyricDisplayMode::Intro,
                        text: active
                            .next
                            .map(|l| l.text.trim().to_string())
                            .unwrap_or_default(),
                    },
                    Some(line) if line.text.trim().is_empty() => HostPacket::LyricLine {
                        mode: LyricDisplayMode::Break,
                        text: String::new(),
                    },
                    Some(line) => HostPacket::LyricLine {
                        mode: LyricDisplayMode::SyncedText,
                        text: line.text.trim().to_string(),
                    },
                }
            }
            LyricsContent::Instrumental => HostPacket::LyricLine {
                mode: LyricDisplayMode::Instrumental,
                text: String::new(),
            },
            LyricsContent::PlainOnly(_) => HostPacket::LyricLine {
                mode: LyricDisplayMode::PlainOnly,
                text: String::new(),
            },
            LyricsContent::NotFound => HostPacket::LyricLine {
                mode: LyricDisplayMode::NotFound,
                text: String::new(),
            },
        },
    };

    let playback = HostPacket::PlaybackState {
        is_playing: snap.status.is_playing(),
        is_muted: volume.is_some_and(|v| v.muted),
        show_volume_overlay: show_overlay && volume.is_some(),
        has_session: true,
        has_volume: volume.is_some(),
        position_sec: pos_sec,
        duration_sec: dur_sec,
        volume_percent: volume.map(|v| v.percent).unwrap_or(0),
    };

    (Some(track), Some(lyric), playback)
}

fn render_dashboard_frame(
    state: &DashboardState,
    opts: &CliOptions,
    transport: &dyn DeviceTransport,
    connected: bool,
    stats: &TransportStats,
    show_overlay: bool,
) -> String {
    let mut frame = String::with_capacity(2800);
    frame.push_str("\x1b[H\x1b[J");
    frame.push_str("========================================================================\n");
    frame.push_str("  Desk Media Companion  |  Phase 0, Milestone 0.4: BLE Protocol & Sim   \n");
    frame.push_str("========================================================================\n\n");

    if let Some(err) = &state.last_poll_err {
        frame.push_str(&format!("  SMTC poll warning (retrying): {err}\n"));
    } else if let Some(snap) = &state.last_snap {
        let app_label = friendly_app_name(&snap.app_id);
        frame.push_str(&render_live_snapshot(snap, state.current_volume));
        frame.push('\n');

        let vol_info = state.current_volume.map(|v| OledVolumeInfo {
            app_label: &app_label,
            percent: v.percent,
            muted: v.muted,
            show_overlay,
        });
        frame.push_str(&render_lyrics_and_oled(
            snap,
            &state.current_lyrics,
            opts.offset_ms,
            vol_info.as_ref(),
        ));
    } else {
        frame.push_str("  Status: [IDLE]\n\n");
        frame.push_str("  Waiting for an active media session...\n");
        frame.push_str("  Start Spotify, YouTube, or any Windows media player.\n");
    }

    frame.push_str("\n------------------------------------------------------------------------\n");
    if opts.simulate {
        let link_badge = if connected {
            "[CONNECTED]"
        } else {
            "[OPEN IN BROWSER]"
        };
        frame.push_str(&format!(
            "  Link:     {}  {}  |  TX: {} pkts ({} B)  RX: {} pkts\n",
            transport.label(),
            link_badge,
            stats.tx_packets,
            stats.tx_bytes,
            stats.rx_packets
        ));
    } else {
        frame.push_str(
            "  Link:     Local terminal only (run with --simulate for 128x64 pixel UI)\n",
        );
    }

    if let Some((at, msg)) = &state.last_action_msg {
        frame.push_str(&format!(
            "  Last Cmd: [+{}] {}\n",
            format_duration(*at),
            msg
        ));
    }
    frame.push_str("  Recent Track Changes:\n");
    if state.recent_tracks.is_empty() {
        frame.push_str("    (none detected yet)\n");
    } else {
        for ev in &state.recent_tracks {
            let title = truncate_chars(&ev.title, 28);
            let artist = truncate_chars(&ev.artist, 22);
            frame.push_str(&format!(
                "    [+{}] {:<10} {} - {}\n",
                format_duration(ev.elapsed),
                ev.app,
                title,
                artist
            ));
        }
    }

    frame.push_str("\n  Controls: [Left/P] Prev  [Space] Play/Pause  [Right/N] Next\n");
    frame.push_str("            [Up/Down] Vol +/-5%  [M] Mute  [Q] Quit\n");
    frame
}

fn send_media_action(reader: &SmtcReader, cmd: MediaCommand, label: &str) -> String {
    match reader.send_command(cmd) {
        Ok(true) => format!("{label} -> sent"),
        Ok(false) => format!("{label} -> skipped (no active session)"),
        Err(err) => format!("{label} -> error: {err}"),
    }
}

fn render_live_snapshot(snap: &MediaSnapshot, volume: Option<AppVolumeState>) -> String {
    let app_label = friendly_app_name(&snap.app_id);
    let badge = status_badge(snap.status);
    let title = truncate_chars(&display_or(&snap.title, "<untitled>"), 56);
    let artist = truncate_chars(&display_or(&snap.artist, "<unknown artist>"), 56);
    let album = truncate_chars(&display_or(&snap.album, "-"), 56);

    let pos_str = format_duration(snap.position);
    let dur_str = if snap.duration.is_zero() {
        "--:--".to_string()
    } else {
        format_duration(snap.duration)
    };
    let bar = render_progress_bar(snap.position, snap.duration, PROGRESS_BAR_WIDTH);

    let rate_str = if (snap.playback_rate - 1.0).abs() > 0.01 {
        format!("  ({:.2}x)", snap.playback_rate)
    } else {
        String::new()
    };

    let vol_line = match volume {
        Some(v) => {
            let pct = if v.muted { 0 } else { v.percent as u64 };
            let vbar = render_progress_bar(
                Duration::from_secs(pct),
                Duration::from_secs(100),
                VOLUME_BAR_WIDTH,
            );
            let mute_tag = if v.muted { " [MUTED]" } else { "" };
            let sess_tag = if v.matched_sessions > 1 {
                format!(" ({} sessions)", v.matched_sessions)
            } else {
                String::new()
            };
            format!("{} {:>3}%{}{}", vbar, v.percent, mute_tag, sess_tag)
        }
        None => format!("(no WASAPI audio session for {app_label} yet)"),
    };

    let lead_secs = snap.position.saturating_sub(snap.raw_position).as_secs_f64();

    format!(
        "  App:      {}  ({})\n  State:    {}  |  Sessions: {}\n  Volume:   {}\n\n  Title:    {}\n  Artist:   {}\n  Album:    {}\n\n  Timeline: {} {} {}{}\n  SMTC Raw: {}  |  Interpolated: {} (+{:.1}s)\n",
        app_label,
        truncate_chars(&snap.app_id, 44),
        badge,
        snap.session_count,
        vol_line,
        title,
        artist,
        album,
        pos_str,
        bar,
        dur_str,
        rate_str,
        format_duration(snap.raw_position),
        pos_str,
        lead_secs
    )
}

fn render_lyrics_and_oled(
    snap: &MediaSnapshot,
    state: &LyricsState,
    offset_ms: i64,
    volume: Option<&OledVolumeInfo<'_>>,
) -> String {
    let mut out =
        String::from("------------------------------------------------------------------------\n");

    let raw_ms = snap.position.as_millis().min(i64::MAX as u128) as i64;
    let effective_pos_ms = raw_ms.saturating_add(offset_ms).max(0) as u64;
    let offset_note = if offset_ms != 0 {
        format!("  (offset {offset_ms:+}ms)")
    } else {
        String::new()
    };

    let oled_status = match state {
        LyricsState::Idle => {
            out.push_str("  Lyrics:   [IDLE]\n");
            OledLyricStatus::NotFound
        }
        LyricsState::Loading => {
            out.push_str("  Lyrics:   [LOADING from lrclib.net...]\n");
            OledLyricStatus::Loading
        }
        LyricsState::Error(msg) => {
            out.push_str(&format!(
                "  Lyrics:   [ERROR] {}\n",
                truncate_chars(msg, 56)
            ));
            OledLyricStatus::Error
        }
        LyricsState::Ready { content, source } => {
            let src = match source {
                LyricsSource::Cache => "cache",
                LyricsSource::Network => "lrclib.net",
            };
            match content {
                LyricsContent::Synced(lines) => {
                    let active = active_lyric(lines, effective_pos_ms);
                    let line_counter = match active.index {
                        Some(i) => format!("Line {}/{}", i + 1, active.total),
                        None => format!("Intro (0/{})", active.total),
                    };
                    out.push_str(&format!(
                        "  Lyrics:   [SYNCED - {src}]  {line_counter}{offset_note}\n"
                    ));

                    let now_text = match active.current {
                        None => "(intro — waiting for first lyric)".to_string(),
                        Some(line) if line.text.trim().is_empty() => {
                            "~ ~ ~ (instrumental break)".to_string()
                        }
                        Some(line) => truncate_chars(&line.text, 60),
                    };
                    let next_text = match active.next {
                        Some(line) if line.text.trim().is_empty() => "~ ~ ~".to_string(),
                        Some(line) => truncate_chars(&line.text, 60),
                        None => "(end of track)".to_string(),
                    };

                    out.push_str(&format!("  Now:    > {now_text}\n"));
                    out.push_str(&format!("  Next:     {next_text}\n"));

                    match active.current {
                        None => OledLyricStatus::Intro {
                            next: active.next.map(|l| l.text.as_str()),
                        },
                        Some(line) => OledLyricStatus::SyncedLine(line.text.as_str()),
                    }
                }
                LyricsContent::Instrumental => {
                    out.push_str(&format!("  Lyrics:   [INSTRUMENTAL - {src}]\n"));
                    OledLyricStatus::Instrumental
                }
                LyricsContent::PlainOnly(lines) => {
                    out.push_str(&format!(
                        "  Lyrics:   [PLAIN ONLY - {src}] ({} lines, not time-synced)\n",
                        lines.len()
                    ));
                    if let Some(first) = lines.iter().find(|l| !l.trim().is_empty()) {
                        out.push_str(&format!("  Sample:   {}\n", truncate_chars(first, 60)));
                    }
                    OledLyricStatus::PlainOnly
                }
                LyricsContent::NotFound => {
                    out.push_str(&format!("  Lyrics:   [NOT FOUND - {src}]\n"));
                    OledLyricStatus::NotFound
                }
            }
        }
    };

    out.push_str("\n  128x64 OLED Layout Preview (21 cols):\n");
    for row in render_oled_box(snap, &oled_status, volume) {
        out.push_str("  ");
        out.push_str(&row);
        out.push('\n');
    }

    out
}

fn render_once_snapshot(snap: &MediaSnapshot) -> String {
    let mut out = String::from("Desk Media Companion — Windows SMTC Snapshot\n");
    out.push_str(&render_live_snapshot(snap, None));
    out
}

fn status_badge(status: PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Playing => "[PLAYING]",
        PlaybackStatus::Paused => "[PAUSED]",
        PlaybackStatus::Stopped => "[STOPPED]",
        PlaybackStatus::Changing => "[CHANGING]",
        PlaybackStatus::Opened => "[OPENED]",
        PlaybackStatus::Closed => "[CLOSED]",
        PlaybackStatus::Unknown => "[UNKNOWN]",
    }
}

fn display_or<'a>(value: &'a str, fallback: &'a str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simulate_and_port_flags() {
        let opts = parse_args(vec![
            "--simulate".to_string(),
            "--port".to_string(),
            "8080".to_string(),
            "--offset-ms=-250".to_string(),
            "--no-cache".to_string(),
        ])
        .unwrap();

        assert!(opts.simulate);
        assert_eq!(opts.port, 8080);
        assert_eq!(opts.offset_ms, -250);
        assert!(opts.no_cache);

        assert!(parse_args(vec!["--port=0".to_string()]).is_err());
        assert!(parse_args(vec!["--port=abc".to_string()]).is_err());
    }
}
