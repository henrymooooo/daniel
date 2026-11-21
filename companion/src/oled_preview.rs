use crate::format::{format_duration, render_progress_bar, truncate_chars};
use crate::media::{MediaSnapshot, PlaybackStatus};

/// 21 monospace 6px-wide glyphs fit comfortably across a 128px-wide SH1106 OLED (21 * 6 = 126px).
pub const OLED_COLS: usize = 21;

/// Lyric area state passed into the `128×64` OLED layout preview.
pub enum OledLyricStatus<'a> {
    Loading,
    SyncedLine(&'a str),
    Intro { next: Option<&'a str> },
    Instrumental,
    PlainOnly,
    NotFound,
    Error,
}

/// Word-wraps `text` into at most `max_lines` lines of width `max_cols`, splitting overlong words
/// and appending `…` on the final line if remaining text overflows.
pub fn wrap_words(text: &str, max_cols: usize, max_lines: usize) -> Vec<String> {
    if max_cols == 0 || max_lines == 0 {
        return Vec::new();
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut overflowed = false;

    for word in words {
        let word_len = word.chars().count();

        if word_len <= max_cols {
            let needed = if current.is_empty() {
                word_len
            } else {
                current_len + 1 + word_len
            };

            if needed <= max_cols {
                if !current.is_empty() {
                    current.push(' ');
                    current_len += 1;
                }
                current.push_str(word);
                current_len += word_len;
            } else {
                if lines.len() + 1 >= max_lines {
                    lines.push(current);
                    overflowed = true;
                    current = String::new();
                    break;
                }
                lines.push(current);
                current = word.to_string();
                current_len = word_len;
            }
        } else {
            if !current.is_empty() {
                if lines.len() + 1 >= max_lines {
                    lines.push(current);
                    overflowed = true;
                    current = String::new();
                    break;
                }
                lines.push(current);
                current = String::new();
                current_len = 0;
            }

            let mut chars = word.chars().peekable();
            while chars.peek().is_some() {
                let chunk: String = chars.by_ref().take(max_cols).collect();
                let chunk_len = chunk.chars().count();
                if chars.peek().is_some() {
                    if lines.len() + 1 >= max_lines {
                        lines.push(chunk);
                        overflowed = true;
                        break;
                    }
                    lines.push(chunk);
                } else {
                    current = chunk;
                    current_len = chunk_len;
                }
            }

            if overflowed {
                break;
            }
        }
    }

    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    }

    if overflowed && let Some(last) = lines.last_mut() {
        *last = force_ellipsis(last, max_cols);
    }

    lines
}

fn force_ellipsis(line: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "…".to_string();
    }
    let count = line.chars().count();
    if count >= max_cols {
        let mut out: String = line.chars().take(max_cols - 1).collect();
        out.push('…');
        out
    } else if line.ends_with('…') {
        line.to_string()
    } else {
        format!("{line}…")
    }
}

fn pad_to_cols(s: &str, cols: usize) -> String {
    let truncated = truncate_chars(s, cols);
    let len = truncated.chars().count();
    if len < cols {
        format!("{truncated}{}", " ".repeat(cols - len))
    } else {
        truncated
    }
}

/// Renders an 8-row boxed text preview simulating the 1.3" `128×64` SH1106 OLED screen.
pub fn render_oled_box(snap: &MediaSnapshot, lyric: &OledLyricStatus<'_>) -> Vec<String> {
    let icon = match snap.status {
        PlaybackStatus::Playing => "> ",
        PlaybackStatus::Paused => "|| ",
        _ => "[] ",
    };

    let title_raw = if snap.title.trim().is_empty() {
        "<untitled>"
    } else {
        snap.title.trim()
    };
    let artist_raw = if snap.artist.trim().is_empty() {
        "<unknown artist>"
    } else {
        snap.artist.trim()
    };

    let row0 = pad_to_cols(&format!("{icon}{title_raw}"), OLED_COLS);
    let row1 = pad_to_cols(artist_raw, OLED_COLS);
    let row2 = "-".repeat(OLED_COLS);

    let mut lyric_rows: Vec<String> = match lyric {
        OledLyricStatus::Loading => vec![
            String::new(),
            "  Loading lyrics...".to_string(),
            String::new(),
        ],
        OledLyricStatus::SyncedLine(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                vec![String::new(), "        ~ ~ ~".to_string(), String::new()]
            } else {
                wrap_words(trimmed, OLED_COLS, 3)
            }
        }
        OledLyricStatus::Intro { next } => match next.map(str::trim).filter(|s| !s.is_empty()) {
            Some(upcoming) => {
                let mut rows = vec!["Next:".to_string()];
                rows.extend(wrap_words(upcoming, OLED_COLS, 2));
                rows
            }
            None => vec![String::new(), "       (intro)".to_string(), String::new()],
        },
        OledLyricStatus::Instrumental => vec![
            String::new(),
            "  ~ Instrumental ~".to_string(),
            String::new(),
        ],
        OledLyricStatus::PlainOnly => vec![
            String::new(),
            " (plain lyrics only)".to_string(),
            String::new(),
        ],
        OledLyricStatus::NotFound => vec![
            String::new(),
            "  (no synced lyrics)".to_string(),
            String::new(),
        ],
        OledLyricStatus::Error => vec![
            String::new(),
            " (lyrics unavailable)".to_string(),
            String::new(),
        ],
    };

    while lyric_rows.len() < 3 {
        lyric_rows.push(String::new());
    }
    lyric_rows.truncate(3);

    let pos_str = format_duration(snap.position);
    let dur_str = if snap.duration.is_zero() {
        "--:--".to_string()
    } else {
        format_duration(snap.duration)
    };
    let fixed = pos_str.chars().count() + dur_str.chars().count() + 4;
    let row6 = if fixed < OLED_COLS {
        let inner = OLED_COLS - fixed;
        let bar = render_progress_bar(snap.position, snap.duration, inner);
        pad_to_cols(&format!("{pos_str} {bar} {dur_str}"), OLED_COLS)
    } else {
        pad_to_cols(&format!("{pos_str} / {dur_str}"), OLED_COLS)
    };

    let border = format!("+{}+", "-".repeat(OLED_COLS));
    let mut out = Vec::with_capacity(9);
    out.push(border.clone());
    out.push(format!("|{row0}|"));
    out.push(format!("|{row1}|"));
    out.push(format!("|{row2}|"));
    for lr in lyric_rows {
        out.push(format!("|{}|", pad_to_cols(&lr, OLED_COLS)));
    }
    out.push(format!("|{row6}|"));
    out.push(border);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample_snap(pos_secs: u64, dur_secs: u64) -> MediaSnapshot {
        MediaSnapshot {
            app_id: "spotifast.exe".to_string(),
            title: "In A Good Way".to_string(),
            artist: "Faye Webster".to_string(),
            album: "I Know I'm Funny haha".to_string(),
            status: PlaybackStatus::Playing,
            position: Duration::from_secs(pos_secs),
            raw_position: Duration::ZERO,
            duration: Duration::from_secs(dur_secs),
            playback_rate: 1.0,
            session_count: 1,
        }
    }

    #[test]
    fn wraps_words_within_oled_width() {
        let lines = wrap_words("I didn't know I was capable of being happy", OLED_COLS, 3);
        assert_eq!(
            lines,
            vec![
                "I didn't know I was",
                "capable of being",
                "happy",
            ]
        );
        assert!(lines.iter().all(|l| l.chars().count() <= OLED_COLS));
    }

    #[test]
    fn breaks_overlong_word_and_adds_ellipsis_on_overflow() {
        let long_word = "Supercalifragilisticexpialidocious";
        let split = wrap_words(long_word, 10, 4);
        assert_eq!(split, vec!["Supercalif", "ragilistic", "expialidoc", "ious"]);

        let overflow = wrap_words(
            "One two three four five six seven eight nine ten eleven twelve",
            OLED_COLS,
            2,
        );
        assert_eq!(overflow.len(), 2);
        assert!(overflow[1].ends_with('…'));
        assert!(overflow.iter().all(|l| l.chars().count() <= OLED_COLS));
    }

    #[test]
    fn oled_box_has_consistent_border_width_for_normal_and_hour_tracks() {
        let normal = render_oled_box(
            &sample_snap(18, 224),
            &OledLyricStatus::SyncedLine("I'm just happy right now"),
        );
        assert_eq!(normal.len(), 9);
        assert!(normal.iter().all(|row| row.chars().count() == OLED_COLS + 2));

        let hour_long = render_oled_box(
            &sample_snap(3_909, 4_800),
            &OledLyricStatus::Intro {
                next: Some("Opening line coming soon"),
            },
        );
        assert_eq!(hour_long.len(), 9);
        assert!(hour_long.iter().all(|row| row.chars().count() == OLED_COLS + 2));
    }
}
