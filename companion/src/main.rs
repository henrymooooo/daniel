mod format;
mod media;

use std::collections::VecDeque;
use std::env;
use std::io::{self, BufWriter, Write};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use format::{format_duration, friendly_app_name, render_progress_bar, truncate_chars};
use media::{MediaSnapshot, PlaybackStatus, SmtcReader, TrackId};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const PROGRESS_BAR_WIDTH: usize = 28;
const MAX_RECENT_TRACKS: usize = 3;

struct TrackEvent {
    elapsed: Duration,
    app: String,
    title: String,
    artist: String,
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Desk Media Companion — Phase 0 (Milestone 0.1: Windows SMTC)");
        println!("Usage: desk-media-companion [--once]");
        println!("  --once   Poll Windows SMTC once, print snapshot, and exit.");
        return;
    }

    let once = args.iter().any(|a| a == "--once");

    let mut reader = match SmtcReader::new() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Failed to initialize Windows SMTC reader: {err}");
            process::exit(1);
        }
    };

    if once {
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

    run_live_dashboard(&mut reader);
}

fn run_live_dashboard(reader: &mut SmtcReader) {
    let started = Instant::now();
    let mut last_track: Option<TrackId> = None;
    let mut recent_tracks: VecDeque<TrackEvent> = VecDeque::with_capacity(MAX_RECENT_TRACKS);

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = out.flush();

    loop {
        let mut frame = String::with_capacity(1024);
        frame.push_str("\x1b[H\x1b[J");
        frame.push_str("========================================================================\n");
        frame.push_str("  Desk Media Companion  |  Phase 0, Milestone 0.1: Windows SMTC Reader  \n");
        frame.push_str("========================================================================\n\n");

        match reader.poll() {
            Ok(Some(snap)) => {
                let track_id = snap.track_id();
                if last_track.as_ref() != Some(&track_id)
                    && (!snap.title.is_empty() || !snap.artist.is_empty())
                {
                    if recent_tracks.len() == MAX_RECENT_TRACKS {
                        recent_tracks.pop_back();
                    }
                    recent_tracks.push_front(TrackEvent {
                        elapsed: started.elapsed(),
                        app: friendly_app_name(&snap.app_id),
                        title: display_or(&snap.title, "<untitled>"),
                        artist: display_or(&snap.artist, "<unknown artist>"),
                    });
                    last_track = Some(track_id);
                }

                frame.push_str(&render_live_snapshot(&snap));
            }
            Ok(None) => {
                frame.push_str("  Status: [IDLE]\n\n");
                frame.push_str("  Waiting for an active media session...\n");
                frame.push_str("  Start Spotify, YouTube, or any Windows media player.\n");
            }
            Err(err) => {
                frame.push_str(&format!("  SMTC poll warning (retrying): {err}\n"));
            }
        }

        frame.push_str("\n------------------------------------------------------------------------\n");
        frame.push_str("  Recent Track Changes (for Milestone 0.2 LRCLIB cache trigger):\n");
        if recent_tracks.is_empty() {
            frame.push_str("    (none detected yet)\n");
        } else {
            for ev in &recent_tracks {
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

        frame.push_str("\n  Press Ctrl+C to exit.\n");

        let _ = out.write_all(frame.as_bytes());
        let _ = out.flush();

        thread::sleep(POLL_INTERVAL);
    }
}

fn render_live_snapshot(snap: &MediaSnapshot) -> String {
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

    let lead_ms = snap.position.saturating_sub(snap.raw_position).as_secs_f64();

    format!(
        "  App:      {}  ({})\n  State:    {}  |  Sessions: {}\n\n  Title:    {}\n  Artist:   {}\n  Album:    {}\n\n  Timeline: {} {} {}{}\n  SMTC Raw: {}  |  Interpolated: {} (+{:.1}s)\n",
        app_label,
        truncate_chars(&snap.app_id, 44),
        badge,
        snap.session_count,
        title,
        artist,
        album,
        pos_str,
        bar,
        dur_str,
        rate_str,
        format_duration(snap.raw_position),
        pos_str,
        lead_ms
    )
}

fn render_once_snapshot(snap: &MediaSnapshot) -> String {
    let mut out = String::from("Desk Media Companion — Windows SMTC Snapshot\n");
    out.push_str(&render_live_snapshot(snap));
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
