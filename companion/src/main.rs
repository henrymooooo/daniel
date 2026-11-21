mod format;
mod lyrics;
mod media;
mod oled_preview;

use std::collections::VecDeque;
use std::env;
use std::io::{self, BufWriter, Write};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

use format::{format_duration, friendly_app_name, render_progress_bar, truncate_chars};
use lyrics::{
    LyricsCache, LyricsContent, LyricsRequest, LyricsSource, LyricsState, LyricsWorker,
    active_lyric,
};
use media::{MediaSnapshot, PlaybackStatus, SmtcReader, TrackId};
use oled_preview::{OledLyricStatus, render_oled_box};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const PROGRESS_BAR_WIDTH: usize = 28;
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
        help: false,
    };

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => opts.help = true,
            "--once" => opts.once = true,
            "--no-cache" => opts.no_cache = true,
            "--offset-ms" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--offset-ms requires an integer value (e.g. --offset-ms 250)".to_string())?;
                opts.offset_ms = val
                    .parse::<i64>()
                    .map_err(|_| format!("invalid --offset-ms value '{val}' (expected integer ms)"))?;
            }
            other if other.starts_with("--offset-ms=") => {
                let val = &other["--offset-ms=".len()..];
                opts.offset_ms = val
                    .parse::<i64>()
                    .map_err(|_| format!("invalid --offset-ms value '{val}' (expected integer ms)"))?;
            }
            other => {
                return Err(format!("unknown argument '{other}'. Run with --help for usage."));
            }
        }
    }

    Ok(opts)
}

fn print_usage() {
    println!("Desk Media Companion — Phase 0 (Milestone 0.2: SMTC + LRCLIB Synced Lyrics)");
    println!("Usage: desk-media-companion [OPTIONS]");
    println!("  --once               Poll Windows SMTC once, print snapshot, and exit.");
    println!("  --offset-ms <MS>     Shift lyric timing in ms (positive = show lyrics earlier).");
    println!("  --no-cache           Disable local disk cache and always query lrclib.net.");
    println!("  -h, --help           Show this help message.");
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

fn run_live_dashboard(reader: &mut SmtcReader, opts: &CliOptions) {
    let started = Instant::now();
    let mut last_track: Option<TrackId> = None;
    let mut recent_tracks: VecDeque<TrackEvent> = VecDeque::with_capacity(MAX_RECENT_TRACKS);
    let mut current_lyrics = LyricsState::Idle;

    let cache = LyricsCache::new(LyricsCache::default_dir(), !opts.no_cache);
    let worker = LyricsWorker::spawn(cache);

    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = out.flush();

    loop {
        let mut frame = String::with_capacity(2048);
        frame.push_str("\x1b[H\x1b[J");
        frame.push_str("========================================================================\n");
        frame.push_str("  Desk Media Companion  |  Phase 0, Milestone 0.2: SMTC + LRCLIB Lyrics   \n");
        frame.push_str("========================================================================\n\n");

        match reader.poll() {
            Ok(Some(snap)) => {
                let track_id = snap.track_id();
                if last_track.as_ref() != Some(&track_id)
                    && (!snap.title.trim().is_empty() || !snap.artist.trim().is_empty())
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
                    last_track = Some(track_id.clone());

                    if !snap.title.trim().is_empty() {
                        current_lyrics = LyricsState::Loading;
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
                        current_lyrics = LyricsState::Idle;
                    }
                }

                while let Some(resp) = worker.try_recv() {
                    if last_track.as_ref() == Some(&resp.track_id) {
                        current_lyrics = resp.state;
                    }
                }

                frame.push_str(&render_live_snapshot(&snap));
                frame.push('\n');
                frame.push_str(&render_lyrics_and_oled(&snap, &current_lyrics, opts.offset_ms));
            }
            Ok(None) => {
                while worker.try_recv().is_some() {}
                frame.push_str("  Status: [IDLE]\n\n");
                frame.push_str("  Waiting for an active media session...\n");
                frame.push_str("  Start Spotify, spotifast, YouTube, or any Windows media player.\n");
            }
            Err(err) => {
                frame.push_str(&format!("  SMTC poll warning (retrying): {err}\n"));
            }
        }

        frame.push_str("\n------------------------------------------------------------------------\n");
        frame.push_str("  Recent Track Changes:\n");
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

    let lead_secs = snap.position.saturating_sub(snap.raw_position).as_secs_f64();

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
        lead_secs
    )
}

fn render_lyrics_and_oled(snap: &MediaSnapshot, state: &LyricsState, offset_ms: i64) -> String {
    let mut out = String::from("------------------------------------------------------------------------\n");

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
                        Some(line) if line.text.trim().is_empty() => "~ ~ ~ (instrumental break)".to_string(),
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
    for row in render_oled_box(snap, &oled_status) {
        out.push_str("  ");
        out.push_str(&row);
        out.push('\n');
    }

    out
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
