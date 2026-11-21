use std::fmt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::Result as WinResult;

/// 100-nanosecond intervals between 1601-01-01 (Windows FILETIME epoch) and 1970-01-01 (Unix epoch).
pub const WINDOWS_EPOCH_OFFSET_100NS: i64 = 116_444_736_000_000_000;

const RPC_E_CHANGED_MODE_HR: i32 = 0x8001_0106_u32 as i32;

/// Normalized playback state reported by Windows SMTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Closed,
    Opened,
    Changing,
    Stopped,
    Playing,
    Paused,
    Unknown,
}

impl PlaybackStatus {
    pub fn is_playing(&self) -> bool {
        matches!(self, Self::Playing)
    }

    fn from_winrt(status: GlobalSystemMediaTransportControlsSessionPlaybackStatus) -> Self {
        match status {
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Closed => Self::Closed,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Opened => Self::Opened,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Changing => Self::Changing,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Stopped => Self::Stopped,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing => Self::Playing,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused => Self::Paused,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for PlaybackStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Closed => "Closed",
            Self::Opened => "Opened",
            Self::Changing => "Changing",
            Self::Stopped => "Stopped",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Unknown => "Unknown",
        };
        f.write_str(label)
    }
}

/// Stable track identifier used to detect track changes cleanly across polling ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackId {
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
}

/// Snapshot of the active Windows media session at a single point in time.
#[derive(Debug, Clone)]
pub struct MediaSnapshot {
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub status: PlaybackStatus,
    pub position: Duration,
    pub raw_position: Duration,
    pub duration: Duration,
    pub playback_rate: f64,
    pub session_count: usize,
}

impl MediaSnapshot {
    pub fn track_id(&self) -> TrackId {
        TrackId {
            app_id: self.app_id.clone(),
            title: self.title.clone(),
            artist: self.artist.clone(),
            duration_ms: self.duration.as_millis().min(u64::MAX as u128) as u64,
        }
    }
}

/// Converts a `SystemTime` into Windows FILETIME 100-nanosecond ticks.
pub fn system_time_to_filetime_100ns(now: SystemTime) -> i64 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let ticks = (d.as_nanos() / 100).min(i64::MAX as u128) as i64;
            WINDOWS_EPOCH_OFFSET_100NS.saturating_add(ticks)
        }
        Err(e) => {
            let ticks = (e.duration().as_nanos() / 100).min(i64::MAX as u128) as i64;
            WINDOWS_EPOCH_OFFSET_100NS.saturating_sub(ticks)
        }
    }
}

fn ticks_100ns_to_duration(ticks_100ns: i64) -> Duration {
    if ticks_100ns <= 0 {
        return Duration::ZERO;
    }
    let nanos = (ticks_100ns as u64).saturating_mul(100);
    Duration::from_nanos(nanos)
}

/// Interpolates the live track position from SMTC timeline fields and elapsed wall-clock time.
///
/// Windows SMTC updates `Position` only intermittently (on play, pause, seek, or track change),
/// so live lyric sync requires advancing from `raw_pos_100ns` using `now_filetime_100ns - last_updated_100ns`
/// (or `fallback_elapsed` for players that omit `LastUpdatedTime`).
#[allow(clippy::too_many_arguments)]
pub fn compute_live_position(
    raw_pos_100ns: i64,
    start_100ns: i64,
    end_100ns: i64,
    last_updated_100ns: i64,
    now_filetime_100ns: i64,
    playback_rate: f64,
    is_playing: bool,
    fallback_elapsed: Duration,
) -> (Duration, Duration) {
    let raw_rel_100ns = raw_pos_100ns.saturating_sub(start_100ns).max(0);
    let duration_100ns = end_100ns.saturating_sub(start_100ns).max(0);
    let raw_rel = ticks_100ns_to_duration(raw_rel_100ns);
    let duration = ticks_100ns_to_duration(duration_100ns);

    if !is_playing {
        return (raw_rel, duration);
    }

    let rate = if playback_rate.is_finite() && playback_rate > 0.0 {
        playback_rate.clamp(0.1, 4.0)
    } else {
        1.0
    };

    let base_delta_100ns = if last_updated_100ns > 0 {
        now_filetime_100ns.saturating_sub(last_updated_100ns).max(0)
    } else {
        (fallback_elapsed.as_nanos() / 100).min(i64::MAX as u128) as i64
    };

    let scaled_delta_100ns = ((base_delta_100ns as f64) * rate).round();
    let delta_100ns = if scaled_delta_100ns.is_finite() && scaled_delta_100ns > 0.0 {
        scaled_delta_100ns.min(i64::MAX as f64) as i64
    } else {
        0
    };

    let mut live_pos = ticks_100ns_to_duration(raw_rel_100ns.saturating_add(delta_100ns));
    if !duration.is_zero() && live_pos > duration {
        live_pos = duration;
    }

    (live_pos, duration)
}

/// Polls Windows Global System Media Transport Controls (SMTC) for the active media session.
pub struct SmtcReader {
    manager: GlobalSystemMediaTransportControlsSessionManager,
    fallback_anchor: Option<(TrackId, i64, Instant)>,
}

impl SmtcReader {
    /// Initializes WinRT/COM on the current thread and acquires the Windows SMTC session manager.
    pub fn new() -> WinResult<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_err() && hr.0 != RPC_E_CHANGED_MODE_HR {
            return Err(windows::core::Error::from(hr));
        }

        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
        Ok(Self {
            manager,
            fallback_anchor: None,
        })
    }

    /// Reads the best active SMTC session (preferring a currently playing session) and returns a live snapshot.
    pub fn poll(&mut self) -> WinResult<Option<MediaSnapshot>> {
        let sessions = self.manager.GetSessions()?;
        let session_count_u32 = sessions.Size()?;
        let session_count = session_count_u32 as usize;
        let current = self.manager.GetCurrentSession().ok();
        let sessions_vec: Vec<GlobalSystemMediaTransportControlsSession> = (0..session_count_u32)
            .filter_map(|idx| sessions.GetAt(idx).ok())
            .collect();

        let selected = pick_preferred_session(current, &sessions_vec);
        let Some(session) = selected else {
            self.fallback_anchor = None;
            return Ok(None);
        };

        let app_id = session
            .SourceAppUserModelId()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default();

        let playback_info = session.GetPlaybackInfo()?;
        let status = playback_info
            .PlaybackStatus()
            .map(PlaybackStatus::from_winrt)
            .unwrap_or(PlaybackStatus::Unknown);

        let playback_rate = playback_info
            .PlaybackRate()
            .and_then(|r| r.Value())
            .ok()
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(1.0);

        // Media properties can fail transiently while a player switches tracks.
        let (title, artist, album) = match session.TryGetMediaPropertiesAsync() {
            Ok(op) => match op.get() {
                Ok(props) => (
                    props.Title().map(|s| s.to_string_lossy()).unwrap_or_default(),
                    props.Artist().map(|s| s.to_string_lossy()).unwrap_or_default(),
                    props
                        .AlbumTitle()
                        .map(|s| s.to_string_lossy())
                        .unwrap_or_default(),
                ),
                Err(_) => (String::new(), String::new(), String::new()),
            },
            Err(_) => (String::new(), String::new(), String::new()),
        };

        let timeline = session.GetTimelineProperties()?;
        let raw_pos_100ns = timeline.Position()?.Duration;
        let start_100ns = timeline.StartTime()?.Duration;
        let end_100ns = timeline.EndTime()?.Duration;
        let last_updated_100ns = timeline.LastUpdatedTime()?.UniversalTime;

        let duration_100ns = end_100ns.saturating_sub(start_100ns).max(0);
        let duration_ms = ticks_100ns_to_duration(duration_100ns)
            .as_millis()
            .min(u64::MAX as u128) as u64;

        let track_id = TrackId {
            app_id: app_id.clone(),
            title: title.clone(),
            artist: artist.clone(),
            duration_ms,
        };

        let fallback_elapsed = self.update_fallback_anchor(
            &track_id,
            raw_pos_100ns,
            status.is_playing() && last_updated_100ns <= 0,
        );

        let now_filetime_100ns = system_time_to_filetime_100ns(SystemTime::now());
        let (position, duration) = compute_live_position(
            raw_pos_100ns,
            start_100ns,
            end_100ns,
            last_updated_100ns,
            now_filetime_100ns,
            playback_rate,
            status.is_playing(),
            fallback_elapsed,
        );
        let raw_position = ticks_100ns_to_duration(raw_pos_100ns.saturating_sub(start_100ns).max(0));

        Ok(Some(MediaSnapshot {
            app_id,
            title,
            artist,
            album,
            status,
            position,
            raw_position,
            duration,
            playback_rate,
            session_count,
        }))
    }

    fn update_fallback_anchor(
        &mut self,
        track_id: &TrackId,
        raw_pos_100ns: i64,
        needs_fallback: bool,
    ) -> Duration {
        if !needs_fallback {
            self.fallback_anchor = None;
            return Duration::ZERO;
        }

        let now = Instant::now();
        match &self.fallback_anchor {
            Some((prev_id, prev_raw, started)) if prev_id == track_id && *prev_raw == raw_pos_100ns => {
                now.saturating_duration_since(*started)
            }
            _ => {
                self.fallback_anchor = Some((track_id.clone(), raw_pos_100ns, now));
                Duration::ZERO
            }
        }
    }
}

fn pick_preferred_session(
    current: Option<GlobalSystemMediaTransportControlsSession>,
    sessions: &[GlobalSystemMediaTransportControlsSession],
) -> Option<GlobalSystemMediaTransportControlsSession> {
    if let Some(ref cur) = current
        && session_is_playing(cur)
    {
        return current;
    }

    if let Some(playing) = sessions.iter().find(|s| session_is_playing(s)) {
        return Some(playing.clone());
    }

    current.or_else(|| sessions.first().cloned())
}

fn session_is_playing(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    session
        .GetPlaybackInfo()
        .and_then(|info| info.PlaybackStatus())
        .map(|status| status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC_100NS: i64 = 10_000_000;

    #[test]
    fn playing_at_normal_rate_advances_from_last_updated() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated + 12 * SEC_100NS + 5_000_000; // +12.5s

        let (pos, dur) = compute_live_position(
            30 * SEC_100NS,
            0,
            200 * SEC_100NS,
            last_updated,
            now,
            1.0,
            true,
            Duration::ZERO,
        );

        assert_eq!(dur, Duration::from_secs(200));
        assert_eq!(pos, Duration::from_millis(42_500));
    }

    #[test]
    fn playing_at_1_5x_scales_elapsed_time() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated + 10 * SEC_100NS;

        let (pos, _) = compute_live_position(
            20 * SEC_100NS,
            0,
            200 * SEC_100NS,
            last_updated,
            now,
            1.5,
            true,
            Duration::ZERO,
        );

        assert_eq!(pos, Duration::from_secs(35));
    }

    #[test]
    fn playing_clamps_to_track_duration() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated + 50 * SEC_100NS;

        let (pos, dur) = compute_live_position(
            180 * SEC_100NS,
            0,
            200 * SEC_100NS,
            last_updated,
            now,
            1.0,
            true,
            Duration::ZERO,
        );

        assert_eq!(dur, Duration::from_secs(200));
        assert_eq!(pos, Duration::from_secs(200));
    }

    #[test]
    fn negative_clock_skew_never_moves_backwards() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated - 5 * SEC_100NS;

        let (pos, _) = compute_live_position(
            45 * SEC_100NS,
            0,
            200 * SEC_100NS,
            last_updated,
            now,
            1.0,
            true,
            Duration::ZERO,
        );

        assert_eq!(pos, Duration::from_secs(45));
    }

    #[test]
    fn paused_freezes_at_reported_position() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated + 60 * SEC_100NS;

        let (pos, dur) = compute_live_position(
            45 * SEC_100NS,
            0,
            200 * SEC_100NS,
            last_updated,
            now,
            1.0,
            false,
            Duration::from_secs(20),
        );

        assert_eq!(dur, Duration::from_secs(200));
        assert_eq!(pos, Duration::from_secs(45));
    }

    #[test]
    fn uses_fallback_elapsed_when_last_updated_is_zero() {
        let (pos, dur) = compute_live_position(
            15 * SEC_100NS,
            0,
            180 * SEC_100NS,
            0,
            WINDOWS_EPOCH_OFFSET_100NS + 5_000 * SEC_100NS,
            1.0,
            true,
            Duration::from_millis(3_400),
        );

        assert_eq!(dur, Duration::from_secs(180));
        assert_eq!(pos, Duration::from_millis(18_400));
    }

    #[test]
    fn normalizes_nonzero_start_offset() {
        let last_updated = WINDOWS_EPOCH_OFFSET_100NS + 1_000 * SEC_100NS;
        let now = last_updated + 5 * SEC_100NS;

        let (pos, dur) = compute_live_position(
            120 * SEC_100NS,
            100 * SEC_100NS,
            300 * SEC_100NS,
            last_updated,
            now,
            1.0,
            true,
            Duration::ZERO,
        );

        assert_eq!(dur, Duration::from_secs(200));
        assert_eq!(pos, Duration::from_secs(25));
    }

    #[test]
    fn filetime_conversion_matches_unix_epoch() {
        assert_eq!(system_time_to_filetime_100ns(UNIX_EPOCH), WINDOWS_EPOCH_OFFSET_100NS);
        assert_eq!(
            system_time_to_filetime_100ns(UNIX_EPOCH + Duration::from_secs(2)),
            WINDOWS_EPOCH_OFFSET_100NS + 2 * SEC_100NS
        );
    }
}
