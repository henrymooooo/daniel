use std::time::Duration;

/// Formats a `Duration` as `MM:SS` when under one hour, or `H:MM:SS` otherwise.
pub fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

/// Renders a fixed-width ASCII progress bar such as `[========>-----------]`.
pub fn render_progress_bar(position: Duration, duration: Duration, width: usize) -> String {
    if width == 0 {
        return "[]".to_string();
    }

    if duration.is_zero() || position.is_zero() {
        return format!("[{}]", "-".repeat(width));
    }

    let ratio = (position.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0);
    if ratio >= 1.0 {
        return format!("[{}]", "=".repeat(width));
    }

    if width == 1 {
        return "[>]".to_string();
    }

    let head = ((ratio * (width - 1) as f64).floor() as usize).min(width - 1);
    let rest = width - head - 1;
    format!("[{}>{}]", "=".repeat(head), "-".repeat(rest))
}

/// Truncates a string to at most `max_chars` Unicode scalar values, appending `…` when clipped.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }

    if max_chars == 1 {
        return "…".to_string();
    }

    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}

/// Maps a Windows `SourceAppUserModelId` (AUMID) to a concise human-readable app label.
pub fn friendly_app_name(aumid: &str) -> String {
    let trimmed = aumid.trim();
    if trimmed.is_empty() {
        return "Unknown App".to_string();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("spotify") {
        return "Spotify".to_string();
    }
    if lower.contains("chrome") {
        return "Chrome".to_string();
    }
    if lower.contains("msedge") || lower.contains("edge") {
        return "Edge".to_string();
    }
    if lower.contains("firefox") {
        return "Firefox".to_string();
    }
    if lower.contains("brave") {
        return "Brave".to_string();
    }
    if lower.contains("vlc") {
        return "VLC".to_string();
    }
    if lower.contains("apple") || lower.contains("itunes") {
        return "Apple Music".to_string();
    }
    if lower.contains("zunemusic") {
        return "Media Player".to_string();
    }

    // Fall back to cleaning up raw AUMIDs like `Vendor.AppName_hash!App` or `C:\...\player.exe`.
    let candidate = if let Some((pkg, app)) = trimmed.split_once('!') {
        let app_lower = app.to_ascii_lowercase();
        if app.is_empty() || app_lower == "app" || app_lower == "application" {
            pkg.split('_').next().unwrap_or(pkg)
        } else {
            app
        }
    } else {
        trimmed
    };

    let basename = candidate
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(candidate);

    let without_ext = if basename.to_ascii_lowercase().ends_with(".exe") {
        &basename[..basename.len() - 4]
    } else {
        basename
    };

    if without_ext.is_empty() {
        "Unknown App".to_string()
    } else {
        without_ext.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sub_hour_and_hour_durations() {
        assert_eq!(format_duration(Duration::from_secs(0)), "00:00");
        assert_eq!(format_duration(Duration::from_secs(5)), "00:05");
        assert_eq!(format_duration(Duration::from_secs(222)), "03:42");
        assert_eq!(format_duration(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(format_duration(Duration::from_secs(3909)), "1:05:09");
    }

    #[test]
    fn renders_progress_bar_edge_cases() {
        assert_eq!(
            render_progress_bar(Duration::from_secs(10), Duration::from_secs(100), 0),
            "[]"
        );
        assert_eq!(
            render_progress_bar(Duration::from_secs(10), Duration::ZERO, 10),
            "[----------]"
        );
        assert_eq!(
            render_progress_bar(Duration::ZERO, Duration::from_secs(100), 10),
            "[----------]"
        );
        assert_eq!(
            render_progress_bar(Duration::from_secs(50), Duration::from_secs(100), 10),
            "[====>-----]"
        );
        assert_eq!(
            render_progress_bar(Duration::from_secs(100), Duration::from_secs(100), 10),
            "[==========]"
        );
        assert_eq!(
            render_progress_bar(Duration::from_secs(150), Duration::from_secs(100), 10),
            "[==========]"
        );
    }

    #[test]
    fn truncates_unicode_safely() {
        assert_eq!(truncate_chars("Hello", 0), "");
        assert_eq!(truncate_chars("Hello", 1), "…");
        assert_eq!(truncate_chars("Hello", 5), "Hello");
        assert_eq!(truncate_chars("Hello World", 6), "Hello…");
        assert_eq!(truncate_chars("夜に駆ける - YOASOBI", 5), "夜に駆け…");
        assert_eq!(truncate_chars("🎵✨🎧", 2), "🎵…");
    }

    #[test]
    fn maps_friendly_app_names() {
        assert_eq!(
            friendly_app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"),
            "Spotify"
        );
        assert_eq!(friendly_app_name("Spotify.exe"), "Spotify");
        assert_eq!(friendly_app_name("MSEdge"), "Edge");
        assert_eq!(friendly_app_name("chrome.exe"), "Chrome");
        assert_eq!(friendly_app_name("AppleInc.AppleMusicWin_nzyj5cx40ttqa!App"), "Apple Music");
        assert_eq!(friendly_app_name("Vendor.MyPlayer_12345!App"), "Vendor.MyPlayer");
        assert_eq!(friendly_app_name(r"C:\Apps\Foobar2000.exe"), "Foobar2000");
        assert_eq!(friendly_app_name("   "), "Unknown App");
    }
}
