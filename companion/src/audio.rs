use std::ptr;

use windows::Win32::Foundation::{CloseHandle, HANDLE, S_OK};
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{Interface, PWSTR, Result as WinResult};

/// 5% volume step per knob detent (20 steps across 0..=100%).
pub const DEFAULT_VOLUME_STEP: i32 = 5;

/// Live volume and mute status for the active media application's WASAPI session(s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppVolumeState {
    pub percent: u8,
    pub muted: bool,
    pub matched_sessions: usize,
}

/// Steps a percentage volume cleanly within `0..=100`.
pub fn step_volume_percent(current: u8, delta_steps: i32, step_size: i32) -> u8 {
    let next = (current as i32) + delta_steps.saturating_mul(step_size);
    next.clamp(0, 100) as u8
}

/// Checks whether a WASAPI audio session (`exe_name` + `session_id`) belongs to an SMTC `aumid`.
pub fn matches_app_session(aumid: &str, exe_name: &str, session_id: &str) -> bool {
    let aumid_trimmed = aumid.trim();
    if aumid_trimmed.is_empty() {
        return false;
    }

    let exe_stem = extract_exe_stem(exe_name);
    let session_lower = session_id.to_ascii_lowercase();
    let fallback_stem = if exe_stem.is_empty() {
        extract_exe_stem_from_session_id(&session_lower)
    } else {
        String::new()
    };
    let stem = if !exe_stem.is_empty() {
        exe_stem.as_str()
    } else {
        fallback_stem.as_str()
    };

    let aumid_lower = aumid_trimmed.to_ascii_lowercase();

    // Exact executable name check first so `spotifast` and `spotify` never cross-match.
    if !stem.is_empty() && aumid_stem_matches_exact(&aumid_lower, stem) {
        return true;
    }

    if aumid_lower.contains("spotifast") {
        return stem == "spotifast" || session_lower.contains("spotifast.exe");
    }
    if aumid_lower.contains("spotify") {
        return stem == "spotify"
            || session_lower.contains("spotify.exe")
            || session_lower.contains("spotifyab.spotifymusic");
    }
    if aumid_lower.contains("msedge") || aumid_lower.contains("microsoftedge") || aumid_lower == "edge" {
        return stem == "msedge" || session_lower.contains("msedge.exe");
    }
    if aumid_lower.contains("chrome") {
        return stem == "chrome" || session_lower.contains("chrome.exe");
    }
    if aumid_lower.contains("firefox") {
        return stem == "firefox" || session_lower.contains("firefox.exe");
    }
    if aumid_lower.contains("brave") {
        return stem == "brave" || session_lower.contains("brave.exe");
    }
    if aumid_lower.contains("opera") {
        return stem == "opera" || session_lower.contains("opera.exe");
    }
    if aumid_lower.contains("vivaldi") {
        return stem == "vivaldi" || session_lower.contains("vivaldi.exe");
    }
    if aumid_lower.contains("vlc") {
        return stem == "vlc" || session_lower.contains("vlc.exe");
    }
    if aumid_lower.contains("applemusic") || aumid_lower.contains("itunes") {
        return stem == "applemusic"
            || stem == "itunes"
            || session_lower.contains("applemusic")
            || session_lower.contains("itunes.exe");
    }
    if aumid_lower.contains("zunemusic") {
        return stem == "microsoft.media.player"
            || stem == "music.ui"
            || session_lower.contains("zunemusic");
    }

    let generic = extract_generic_aumid_stem(&aumid_lower);
    if generic.len() >= 3 {
        if stem == generic || session_lower.contains(&format!("{generic}.exe")) {
            return true;
        }
    }

    false
}

fn aumid_stem_matches_exact(aumid_lower: &str, stem: &str) -> bool {
    let base = aumid_lower
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(aumid_lower);
    let stripped = base.strip_suffix(".exe").unwrap_or(base);
    stripped == stem
}

fn extract_exe_stem(exe_name: &str) -> String {
    let trimmed = exe_name.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let base = trimmed
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base).to_string()
}

fn extract_exe_stem_from_session_id(session_lower: &str) -> String {
    let Some(exe_pos) = session_lower.find(".exe") else {
        return String::new();
    };
    let before = &session_lower[..exe_pos];
    before
        .rsplit(['\\', '/', '|'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn extract_generic_aumid_stem(aumid_lower: &str) -> String {
    let candidate = if let Some((pkg, app)) = aumid_lower.split_once('!') {
        if app.is_empty() || app == "app" || app == "application" {
            pkg.split('_').next().unwrap_or(pkg)
        } else {
            app
        }
    } else {
        aumid_lower
    };
    extract_exe_stem(candidate)
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

struct CoTaskStr(PWSTR);

impl CoTaskStr {
    fn into_string(self) -> String {
        if self.0.is_null() {
            return String::new();
        }
        unsafe { self.0.to_string().unwrap_or_default() }
    }
}

impl Drop for CoTaskStr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CoTaskMemFree(Some(self.0.0 as *const _));
            }
        }
    }
}

struct MatchedSession {
    volume: ISimpleAudioVolume,
    is_active: bool,
}

/// Reads and controls per-application volume/mute via Windows WASAPI (`ISimpleAudioVolume`).
#[derive(Default)]
pub struct AppAudioController;

impl AppAudioController {
    /// Reads the current volume `%` and mute state of the WASAPI audio session(s) matching `aumid`.
    pub fn get_app_volume(&self, aumid: &str) -> WinResult<Option<AppVolumeState>> {
        let sessions = collect_matching_sessions(aumid)?;
        if sessions.is_empty() {
            return Ok(None);
        }
        let primary = pick_primary_session(&sessions);
        let (percent, muted) = read_session_volume(&primary.volume)?;
        Ok(Some(AppVolumeState {
            percent,
            muted,
            matched_sessions: sessions.len(),
        }))
    }

    /// Adjusts volume by `delta_steps * step_size` across all WASAPI sessions matching `aumid`.
    pub fn adjust_app_volume(
        &self,
        aumid: &str,
        delta_steps: i32,
        step_size: i32,
    ) -> WinResult<Option<AppVolumeState>> {
        let sessions = collect_matching_sessions(aumid)?;
        if sessions.is_empty() {
            return Ok(None);
        }

        let primary = pick_primary_session(&sessions);
        let (current_percent, current_muted) = read_session_volume(&primary.volume)?;
        let new_percent = step_volume_percent(current_percent, delta_steps, step_size);
        let new_level = (new_percent as f32) / 100.0;
        let should_unmute = delta_steps > 0 && current_muted;

        for s in &sessions {
            unsafe {
                s.volume.SetMasterVolume(new_level, ptr::null())?;
                if should_unmute {
                    s.volume.SetMute(false, ptr::null())?;
                }
            }
        }

        Ok(Some(AppVolumeState {
            percent: new_percent,
            muted: if should_unmute { false } else { current_muted },
            matched_sessions: sessions.len(),
        }))
    }

    /// Toggles mute across all WASAPI sessions matching `aumid`.
    pub fn toggle_app_mute(&self, aumid: &str) -> WinResult<Option<AppVolumeState>> {
        let sessions = collect_matching_sessions(aumid)?;
        if sessions.is_empty() {
            return Ok(None);
        }

        let primary = pick_primary_session(&sessions);
        let (percent, current_muted) = read_session_volume(&primary.volume)?;
        let new_muted = !current_muted;

        for s in &sessions {
            unsafe {
                s.volume.SetMute(new_muted, ptr::null())?;
            }
        }

        Ok(Some(AppVolumeState {
            percent,
            muted: new_muted,
            matched_sessions: sessions.len(),
        }))
    }
}

fn pick_primary_session(sessions: &[MatchedSession]) -> &MatchedSession {
    sessions
        .iter()
        .find(|s| s.is_active)
        .unwrap_or(&sessions[0])
}

fn read_session_volume(volume: &ISimpleAudioVolume) -> WinResult<(u8, bool)> {
    let level = unsafe { volume.GetMasterVolume()? };
    let muted = unsafe { volume.GetMute()?.as_bool() };
    let percent = (level * 100.0).round().clamp(0.0, 100.0) as u8;
    Ok((percent, muted))
}

fn collect_matching_sessions(aumid: &str) -> WinResult<Vec<MatchedSession>> {
    if aumid.trim().is_empty() {
        return Ok(Vec::new());
    }

    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)? };
    let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let session_enum = unsafe { manager.GetSessionEnumerator()? };
    let count = unsafe { session_enum.GetCount()? };

    let mut matched = Vec::new();
    for idx in 0..count {
        let Ok(control) = (unsafe { session_enum.GetSession(idx) }) else {
            continue;
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };

        if unsafe { control2.IsSystemSoundsSession() } == S_OK {
            continue;
        }

        let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
            continue;
        };
        if pid == 0 {
            continue;
        }

        let exe_name = process_exe_name(pid).unwrap_or_default();
        let session_id = unsafe { control2.GetSessionIdentifier() }
            .map(|pw| CoTaskStr(pw).into_string())
            .unwrap_or_default();

        if !matches_app_session(aumid, &exe_name, &session_id) {
            continue;
        }

        let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
            continue;
        };
        let is_active = unsafe { control2.GetState() }
            .map(|st| st == AudioSessionStateActive)
            .unwrap_or(false);

        matched.push(MatchedSession { volume, is_active });
    }

    Ok(matched)
}

fn process_exe_name(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let owned = OwnedHandle(handle);

    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            owned.0,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }

    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_spotifast_without_matching_spotify() {
        assert!(matches_app_session(
            "spotifast.exe",
            r"C:\Users\Henry\Apps\spotifast.exe",
            ""
        ));
        assert!(!matches_app_session(
            "spotifast.exe",
            r"C:\Users\Henry\AppData\Roaming\Spotify\Spotify.exe",
            ""
        ));
        assert!(matches_app_session(
            "Spotify.exe",
            r"C:\Users\Henry\AppData\Roaming\Spotify\Spotify.exe",
            ""
        ));
        assert!(!matches_app_session(
            "Spotify.exe",
            r"C:\Users\Henry\Apps\spotifast.exe",
            ""
        ));
    }

    #[test]
    fn matches_browsers_and_uwp_aumids() {
        assert!(matches_app_session("MSEdge", "msedge.exe", ""));
        assert!(matches_app_session(
            "Chrome",
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ""
        ));
        assert!(matches_app_session(
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
            "Spotify.exe",
            ""
        ));
        assert!(matches_app_session(
            "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App",
            "AppleMusic.exe",
            ""
        ));
        assert!(!matches_app_session("MSEdge", "Discord.exe", ""));
    }

    #[test]
    fn matches_via_session_id_fallback_when_exe_unavailable() {
        assert!(matches_app_session(
            "spotifast.exe",
            "",
            r"{0.0.0.00000000}.{abc}|\Device\HarddiskVolume3\Apps\spotifast.exe%b{00000000}"
        ));
        assert!(matches_app_session(
            "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
            "",
            r"{0.0.0.00000000}.{abc}|#%b{00000000}|SpotifyAB.SpotifyMusic"
        ));
    }

    #[test]
    fn steps_and_clamps_volume_percent() {
        assert_eq!(step_volume_percent(50, 1, 5), 55);
        assert_eq!(step_volume_percent(50, -3, 5), 35);
        assert_eq!(step_volume_percent(98, 2, 5), 100);
        assert_eq!(step_volume_percent(3, -2, 5), 0);
    }
}
