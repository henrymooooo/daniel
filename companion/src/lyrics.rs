use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::media::TrackId;

const LRCLIB_GET_URL: &str = "https://lrclib.net/api/get";
const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";
const USER_AGENT: &str = "DeskMediaCompanion/0.2.0 (https://github.com/local/desk-media-companion)";
const NOT_FOUND_TTL_SECS: u64 = 86_400; // 24 hours

/// A single timestamped lyric line (empty `text` marks an instrumental break).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    pub time_ms: u64,
    pub text: String,
}

/// Parsed lyrics payload returned from cache or LRCLIB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsContent {
    Synced(Vec<LyricLine>),
    Instrumental,
    PlainOnly(Vec<String>),
    NotFound,
}

/// Where a resolved lyrics entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsSource {
    Cache,
    Network,
}

/// Current lyrics state for the active track.
#[derive(Debug, Clone)]
pub enum LyricsState {
    Idle,
    Loading,
    Ready {
        content: LyricsContent,
        source: LyricsSource,
    },
    Error(String),
}

/// Active + upcoming synced lyric lines at the current playback timestamp.
#[derive(Debug, Clone)]
pub struct ActiveLyric<'a> {
    pub current: Option<&'a LyricLine>,
    pub next: Option<&'a LyricLine>,
    pub index: Option<usize>,
    pub total: usize,
}

/// Parses an `.lrc` document into time-sorted `LyricLine` entries.
pub fn parse_lrc(lrc: &str) -> Vec<LyricLine> {
    let mut offset_ms: i64 = 0;
    let mut raw_entries: Vec<(u64, String)> = Vec::new();

    for raw_line in lrc.lines() {
        let mut rest = raw_line.trim_start();
        if !rest.starts_with('[') {
            continue;
        }

        let mut timestamps: Vec<u64> = Vec::new();
        let mut metadata_only = false;

        while rest.starts_with('[') {
            let Some(close_idx) = rest.find(']') else {
                break;
            };
            let tag = &rest[1..close_idx];

            if let Some(ms) = parse_timestamp_tag(tag) {
                timestamps.push(ms);
                rest = &rest[close_idx + 1..];
            } else if timestamps.is_empty() {
                if let Some(parsed_offset) = parse_offset_tag(tag) {
                    offset_ms = parsed_offset;
                }
                metadata_only = true;
                break;
            } else {
                break;
            }
        }

        if metadata_only || timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for raw_ms in timestamps {
            raw_entries.push((raw_ms, text.clone()));
        }
    }

    let mut lines: Vec<LyricLine> = raw_entries
        .into_iter()
        .map(|(raw_ms, text)| LyricLine {
            time_ms: apply_offset_ms(raw_ms, offset_ms),
            text,
        })
        .collect();

    lines.sort_by_key(|line| line.time_ms);
    lines
}

fn parse_timestamp_tag(tag: &str) -> Option<u64> {
    let (mm_str, ss_part) = tag.trim().split_once(':')?;
    if mm_str.is_empty() || !mm_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let minutes: u64 = mm_str.parse().ok()?;

    let (ss_str, frac_ms) = if let Some((sec, frac)) = ss_part.split_once('.') {
        if sec.is_empty() || !sec.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if frac.is_empty() || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let millis = parse_fraction_ms(frac)?;
        (sec, millis)
    } else {
        if ss_part.is_empty() || !ss_part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        (ss_part, 0)
    };

    let seconds: u64 = ss_str.parse().ok()?;
    Some(
        minutes
            .saturating_mul(60_000)
            .saturating_add(seconds.saturating_mul(1_000))
            .saturating_add(frac_ms),
    )
}

fn parse_fraction_ms(frac: &str) -> Option<u64> {
    let digits: String = frac.chars().take(3).collect();
    let val: u64 = digits.parse().ok()?;
    let ms = match digits.len() {
        1 => val * 100,
        2 => val * 10,
        3 => val,
        _ => return None,
    };
    Some(ms)
}

fn parse_offset_tag(tag: &str) -> Option<i64> {
    let (key, val) = tag.split_once(':')?;
    if !key.trim().eq_ignore_ascii_case("offset") {
        return None;
    }
    val.trim().parse::<i64>().ok()
}

fn apply_offset_ms(raw_ms: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        raw_ms.saturating_sub(offset_ms as u64)
    } else {
        raw_ms.saturating_add(offset_ms.unsigned_abs())
    }
}

/// Finds the active and upcoming lyric lines for `position_ms` (assuming `lines` is sorted by `time_ms`).
pub fn active_lyric<'a>(lines: &'a [LyricLine], position_ms: u64) -> ActiveLyric<'a> {
    let idx = lines.partition_point(|line| line.time_ms <= position_ms);
    if idx == 0 {
        ActiveLyric {
            current: None,
            next: lines.first(),
            index: None,
            total: lines.len(),
        }
    } else {
        ActiveLyric {
            current: Some(&lines[idx - 1]),
            next: lines.get(idx),
            index: Some(idx - 1),
            total: lines.len(),
        }
    }
}

/// Strips noisy streaming suffixes such as `(Official Video)` or `- 2019 Remaster` for search fallback.
pub fn clean_track_title(title: &str) -> String {
    let original = title.trim();
    if original.is_empty() {
        return String::new();
    }

    let mut work = original.to_string();
    loop {
        let stripped_brackets = strip_matching_bracket_suffixes(&work);
        if stripped_brackets != work {
            work = stripped_brackets;
            continue;
        }

        if let Some((prefix, tail)) = work.rsplit_once(" - ") {
            let prefix_trimmed = prefix.trim();
            if !prefix_trimmed.is_empty() && is_dash_noise_qualifier(tail) {
                work = prefix_trimmed.to_string();
                continue;
            }
        }
        break;
    }

    let normalized: String = work.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        original.to_string()
    } else {
        normalized
    }
}

fn strip_matching_bracket_suffixes(input: &str) -> String {
    let mut s = input.trim().to_string();
    loop {
        let trimmed = s.trim_end();
        let last_char = trimmed.chars().last();
        let (open_ch, close_ch) = match last_char {
            Some(')') => ('(', ')'),
            Some(']') => ('[', ']'),
            _ => break,
        };

        let Some(open_idx) = trimmed.rfind(open_ch) else {
            break;
        };
        let inside = &trimmed[open_idx + 1..trimmed.len() - close_ch.len_utf8()];
        if is_noise_qualifier(inside) {
            let prefix = trimmed[..open_idx].trim();
            if prefix.is_empty() {
                break;
            }
            s = prefix.to_string();
        } else {
            break;
        }
    }
    s
}

fn is_dash_noise_qualifier(tail: &str) -> bool {
    let lower = tail.trim().to_ascii_lowercase();
    is_noise_qualifier(&lower) || lower == "live" || lower.starts_with("live ")
}

fn is_noise_qualifier(inside: &str) -> bool {
    let lower = inside.to_ascii_lowercase();
    const KEYWORDS: &[&str] = &[
        "official video",
        "official music video",
        "official audio",
        "official lyric video",
        "official visualizer",
        "lyric video",
        "lyrics",
        "music video",
        "visualizer",
        "audio",
        "remaster",
        "remastered",
        "hd",
        "4k",
        "mv",
    ];
    KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Cleans YouTube Music auto-generated `" - Topic"` channel suffixes from artist names.
pub fn clean_artist_name(artist: &str) -> String {
    let trimmed = artist.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let suffix = " - topic";
    let lower = trimmed.to_ascii_lowercase();
    if lower.ends_with(suffix) && trimmed.len() > suffix.len() {
        trimmed[..trimmed.len() - suffix.len()].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct LrcLibRecord {
    #[serde(rename = "trackName", default)]
    #[allow(dead_code)]
    track_name: String,
    #[serde(rename = "artistName", default)]
    #[allow(dead_code)]
    artist_name: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(rename = "syncedLyrics")]
    synced_lyrics: Option<String>,
    #[serde(rename = "plainLyrics")]
    plain_lyrics: Option<String>,
}

fn record_to_content(rec: &LrcLibRecord) -> LyricsContent {
    if rec.instrumental {
        return LyricsContent::Instrumental;
    }

    if let Some(raw_synced) = &rec.synced_lyrics {
        let parsed = parse_lrc(raw_synced);
        if !parsed.is_empty() {
            return LyricsContent::Synced(parsed);
        }
    }

    if let Some(raw_plain) = &rec.plain_lyrics {
        let lines: Vec<String> = raw_plain.lines().map(|l| l.trim().to_string()).collect();
        if lines.iter().any(|l| !l.is_empty()) {
            return LyricsContent::PlainOnly(lines);
        }
    }

    LyricsContent::NotFound
}

fn pick_best_search_match<'a>(
    records: &'a [LrcLibRecord],
    target_duration_secs: Option<u64>,
) -> Option<&'a LrcLibRecord> {
    let mut best: Option<&'a LrcLibRecord> = None;
    let mut best_score = i64::MIN;

    for rec in records {
        let content_score: i64 = match record_to_content(rec) {
            LyricsContent::Synced(_) => 1_000,
            LyricsContent::Instrumental => 400,
            LyricsContent::PlainOnly(_) => 200,
            LyricsContent::NotFound => 0,
        };

        let duration_score: i64 = match (target_duration_secs, rec.duration) {
            (Some(target), Some(rec_dur)) if rec_dur.is_finite() && rec_dur > 0.0 => {
                let rec_secs = rec_dur.round() as u64;
                let diff = target.abs_diff(rec_secs) as i64;
                let penalty = (diff * 5).min(300);
                let bonus = if diff <= 3 { 100 } else { 0 };
                bonus - penalty
            }
            _ => 0,
        };

        let score = content_score + duration_score;
        if score > best_score {
            best_score = score;
            best = Some(rec);
        }
    }

    best
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEntry {
    artist: String,
    title: String,
    duration_secs: Option<u64>,
    cached_at_secs: u64,
    kind: String,
    raw_text: Option<String>,
}

/// On-disk JSON cache for fetched `.lrc` / plain / instrumental / not-found results.
#[derive(Debug, Clone)]
pub struct LyricsCache {
    dir: PathBuf,
    enabled: bool,
}

impl LyricsCache {
    pub fn new(dir: PathBuf, enabled: bool) -> Self {
        Self { dir, enabled }
    }

    pub fn default_dir() -> PathBuf {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            PathBuf::from(local_app_data)
                .join("desk-media-companion")
                .join("lyrics_cache")
        } else {
            PathBuf::from(".lyrics_cache")
        }
    }

    pub fn load(
        &self,
        artist: &str,
        title: &str,
        duration_secs: Option<u64>,
    ) -> Option<LyricsContent> {
        self.load_at(artist, title, duration_secs, now_unix_secs())
    }

    fn load_at(
        &self,
        artist: &str,
        title: &str,
        duration_secs: Option<u64>,
        now_secs: u64,
    ) -> Option<LyricsContent> {
        if !self.enabled {
            return None;
        }

        let path = self.entry_path(artist, title, duration_secs);
        let raw = fs::read_to_string(path).ok()?;
        let entry: CachedEntry = serde_json::from_str(&raw).ok()?;

        match entry.kind.as_str() {
            "synced" => {
                let raw_lrc = entry.raw_text?;
                let parsed = parse_lrc(&raw_lrc);
                if parsed.is_empty() {
                    None
                } else {
                    Some(LyricsContent::Synced(parsed))
                }
            }
            "instrumental" => Some(LyricsContent::Instrumental),
            "plain" => {
                let raw_plain = entry.raw_text?;
                let lines: Vec<String> = raw_plain.lines().map(|l| l.trim().to_string()).collect();
                if lines.iter().any(|l| !l.is_empty()) {
                    Some(LyricsContent::PlainOnly(lines))
                } else {
                    None
                }
            }
            "not_found" => {
                if now_secs.saturating_sub(entry.cached_at_secs) >= NOT_FOUND_TTL_SECS {
                    None
                } else {
                    Some(LyricsContent::NotFound)
                }
            }
            _ => None,
        }
    }

    pub fn save(
        &self,
        artist: &str,
        title: &str,
        duration_secs: Option<u64>,
        content: &LyricsContent,
        raw_lrc: Option<&str>,
    ) {
        self.save_at(artist, title, duration_secs, content, raw_lrc, now_unix_secs());
    }

    fn save_at(
        &self,
        artist: &str,
        title: &str,
        duration_secs: Option<u64>,
        content: &LyricsContent,
        raw_lrc: Option<&str>,
        now_secs: u64,
    ) {
        if !self.enabled {
            return;
        }

        if fs::create_dir_all(&self.dir).is_err() {
            return;
        }

        let (kind, raw_text) = match content {
            LyricsContent::Synced(lines) => {
                let text = raw_lrc
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| encode_lrc(lines));
                ("synced".to_string(), Some(text))
            }
            LyricsContent::Instrumental => ("instrumental".to_string(), None),
            LyricsContent::PlainOnly(lines) => ("plain".to_string(), Some(lines.join("\n"))),
            LyricsContent::NotFound => ("not_found".to_string(), None),
        };

        let entry = CachedEntry {
            artist: artist.to_string(),
            title: title.to_string(),
            duration_secs,
            cached_at_secs: now_secs,
            kind,
            raw_text,
        };

        if let Ok(json) = serde_json::to_string_pretty(&entry) {
            let path = self.entry_path(artist, title, duration_secs);
            let _ = fs::write(path, json);
        }
    }

    fn entry_path(&self, artist: &str, title: &str, duration_secs: Option<u64>) -> PathBuf {
        let key = cache_key(artist, title, duration_secs);
        self.dir.join(format!("{key}.json"))
    }
}

fn cache_key(artist: &str, title: &str, duration_secs: Option<u64>) -> String {
    let norm_artist = normalize_key_part(artist);
    let norm_title = normalize_key_part(title);
    let duration_bucket = duration_secs.map(|d| d / 5).unwrap_or(0);
    let raw = format!("{norm_artist}|{norm_title}|{duration_bucket}");
    format!("{:016x}", fnv1a_64(raw.as_bytes()))
}

fn normalize_key_part(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn encode_lrc(lines: &[LyricLine]) -> String {
    let mut out = String::new();
    for line in lines {
        let mins = line.time_ms / 60_000;
        let secs = (line.time_ms % 60_000) / 1_000;
        let ms = line.time_ms % 1_000;
        out.push_str(&format!("[{mins:02}:{secs:02}.{ms:03}] {}\n", line.text));
    }
    out
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

struct FetchedLyrics {
    content: LyricsContent,
    raw_lrc: Option<String>,
}

fn fetch_from_lrclib(
    agent: &ureq::Agent,
    title: &str,
    artist: &str,
    album: &str,
    duration_secs: Option<u64>,
) -> Result<FetchedLyrics, String> {
    let title_trimmed = title.trim();
    let artist_trimmed = artist.trim();
    if title_trimmed.is_empty() && artist_trimmed.is_empty() {
        return Ok(FetchedLyrics {
            content: LyricsContent::NotFound,
            raw_lrc: None,
        });
    }

    let mut fallback_plain: Option<Vec<String>> = None;

    if !title_trimmed.is_empty() && !artist_trimmed.is_empty() {
        let mut req = agent
            .get(LRCLIB_GET_URL)
            .query("track_name", title_trimmed)
            .query("artist_name", artist_trimmed);

        let album_trimmed = album.trim();
        if !album_trimmed.is_empty() {
            req = req.query("album_name", album_trimmed);
        }
        if let Some(dur) = duration_secs.filter(|&d| d > 0) {
            req = req.query("duration", &dur.to_string());
        }

        match req.call() {
            Ok(resp) => {
                let rec: LrcLibRecord = serde_json::from_reader(resp.into_reader())
                    .map_err(|e| format!("Invalid LRCLIB response JSON: {e}"))?;
                let content = record_to_content(&rec);
                match content {
                    LyricsContent::Synced(_) | LyricsContent::Instrumental => {
                        return Ok(FetchedLyrics {
                            content,
                            raw_lrc: rec.synced_lyrics,
                        });
                    }
                    LyricsContent::PlainOnly(lines) => {
                        fallback_plain = Some(lines);
                    }
                    LyricsContent::NotFound => {}
                }
            }
            Err(ureq::Error::Status(404, _)) => {}
            Err(ureq::Error::Status(code, _)) => {
                return Err(format!("LRCLIB HTTP {code}"));
            }
            Err(ureq::Error::Transport(err)) => {
                return Err(format!("Network error: {err}"));
            }
        }
    }

    let clean_title = clean_track_title(title_trimmed);
    let clean_artist = clean_artist_name(artist_trimmed);

    let search_req = if !clean_title.is_empty() && !clean_artist.is_empty() {
        agent
            .get(LRCLIB_SEARCH_URL)
            .query("track_name", &clean_title)
            .query("artist_name", &clean_artist)
    } else {
        let q = if !clean_title.is_empty() {
            format!("{clean_title} {clean_artist}").trim().to_string()
        } else {
            clean_artist.clone()
        };
        agent.get(LRCLIB_SEARCH_URL).query("q", &q)
    };

    match search_req.call() {
        Ok(resp) => {
            let records: Vec<LrcLibRecord> = serde_json::from_reader(resp.into_reader())
                .map_err(|e| format!("Invalid LRCLIB search JSON: {e}"))?;
            if let Some(best) = pick_best_search_match(&records, duration_secs) {
                let content = record_to_content(best);
                if !matches!(content, LyricsContent::NotFound) {
                    return Ok(FetchedLyrics {
                        content,
                        raw_lrc: best.synced_lyrics.clone(),
                    });
                }
            }
        }
        Err(ureq::Error::Status(404, _)) => {}
        Err(ureq::Error::Status(code, _)) => {
            if let Some(lines) = fallback_plain {
                return Ok(FetchedLyrics {
                    content: LyricsContent::PlainOnly(lines),
                    raw_lrc: None,
                });
            }
            return Err(format!("LRCLIB search HTTP {code}"));
        }
        Err(ureq::Error::Transport(err)) => {
            if let Some(lines) = fallback_plain {
                return Ok(FetchedLyrics {
                    content: LyricsContent::PlainOnly(lines),
                    raw_lrc: None,
                });
            }
            return Err(format!("Network error: {err}"));
        }
    }

    if let Some(lines) = fallback_plain {
        return Ok(FetchedLyrics {
            content: LyricsContent::PlainOnly(lines),
            raw_lrc: None,
        });
    }

    Ok(FetchedLyrics {
        content: LyricsContent::NotFound,
        raw_lrc: None,
    })
}

/// Request sent to the background LRCLIB worker when the active track changes.
#[derive(Debug, Clone)]
pub struct LyricsRequest {
    pub track_id: TrackId,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: Option<u64>,
}

/// Response produced by the background LRCLIB worker.
#[derive(Debug, Clone)]
pub struct LyricsResponse {
    pub track_id: TrackId,
    pub state: LyricsState,
}

/// Non-blocking background worker that fetches and caches lyrics on track changes.
pub struct LyricsWorker {
    tx: mpsc::Sender<LyricsRequest>,
    rx: mpsc::Receiver<LyricsResponse>,
}

impl LyricsWorker {
    pub fn spawn(cache: LyricsCache) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<LyricsRequest>();
        let (resp_tx, resp_rx) = mpsc::channel::<LyricsResponse>();

        thread::Builder::new()
            .name("lrclib-worker".to_string())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(4))
                    .timeout_read(Duration::from_secs(6))
                    .user_agent(USER_AGENT)
                    .build();

                let mut mem_cache: HashMap<String, LyricsContent> = HashMap::new();

                while let Ok(mut req) = req_rx.recv() {
                    // Coalesce queued track skips so we only fetch the latest track.
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }

                    let key = cache_key(&req.artist, &req.title, req.duration_secs);

                    if let Some(cached) = mem_cache.get(&key) {
                        let _ = resp_tx.send(LyricsResponse {
                            track_id: req.track_id,
                            state: LyricsState::Ready {
                                content: cached.clone(),
                                source: LyricsSource::Cache,
                            },
                        });
                        continue;
                    }

                    if let Some(disk_cached) =
                        cache.load(&req.artist, &req.title, req.duration_secs)
                    {
                        mem_cache.insert(key, disk_cached.clone());
                        let _ = resp_tx.send(LyricsResponse {
                            track_id: req.track_id,
                            state: LyricsState::Ready {
                                content: disk_cached,
                                source: LyricsSource::Cache,
                            },
                        });
                        continue;
                    }

                    let state = match fetch_from_lrclib(
                        &agent,
                        &req.title,
                        &req.artist,
                        &req.album,
                        req.duration_secs,
                    ) {
                        Ok(fetched) => {
                            cache.save(
                                &req.artist,
                                &req.title,
                                req.duration_secs,
                                &fetched.content,
                                fetched.raw_lrc.as_deref(),
                            );
                            mem_cache.insert(key, fetched.content.clone());
                            LyricsState::Ready {
                                content: fetched.content,
                                source: LyricsSource::Network,
                            }
                        }
                        Err(err) => LyricsState::Error(err),
                    };

                    let _ = resp_tx.send(LyricsResponse {
                        track_id: req.track_id,
                        state,
                    });
                }
            })
            .expect("failed to spawn lrclib-worker thread");

        Self {
            tx: req_tx,
            rx: resp_rx,
        }
    }

    pub fn request(&self, req: LyricsRequest) {
        let _ = self.tx.send(req);
    }

    pub fn try_recv(&self) -> Option<LyricsResponse> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_centiseconds_milliseconds_and_plain_seconds() {
        let lrc = "\
[ar:Faye Webster]
[ti:In A Good Way]
[00:05.2] First
[00:12.34] Second
[01:02.345] Third
[02:00] Fourth
";
        let lines = parse_lrc(lrc);
        assert_eq!(
            lines,
            vec![
                LyricLine {
                    time_ms: 5_200,
                    text: "First".to_string()
                },
                LyricLine {
                    time_ms: 12_340,
                    text: "Second".to_string()
                },
                LyricLine {
                    time_ms: 62_345,
                    text: "Third".to_string()
                },
                LyricLine {
                    time_ms: 120_000,
                    text: "Fourth".to_string()
                },
            ]
        );
    }

    #[test]
    fn parses_multi_timestamp_lines_and_sorts() {
        let lrc = "[01:30.20][00:45.10] Chorus line\n[00:20.00] Verse one\n";
        let lines = parse_lrc(lrc);
        assert_eq!(
            lines,
            vec![
                LyricLine {
                    time_ms: 20_000,
                    text: "Verse one".to_string()
                },
                LyricLine {
                    time_ms: 45_100,
                    text: "Chorus line".to_string()
                },
                LyricLine {
                    time_ms: 90_200,
                    text: "Chorus line".to_string()
                },
            ]
        );
    }

    #[test]
    fn applies_positive_and_negative_lrc_offset() {
        let earlier = parse_lrc("[offset:500]\n[00:00.20] Early\n[00:02.00] Later\n");
        assert_eq!(earlier[0].time_ms, 0);
        assert_eq!(earlier[1].time_ms, 1_500);

        let delayed = parse_lrc("[offset:-750]\n[00:02.00] Delayed\n");
        assert_eq!(delayed[0].time_ms, 2_750);
    }

    #[test]
    fn preserves_blank_instrumental_break_lines() {
        let lrc = "[00:10.00] Singing\n[00:20.00]\n[00:35.00] Back again\n";
        let lines = parse_lrc(lrc);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "");
        assert_eq!(lines[1].time_ms, 20_000);
    }

    #[test]
    fn active_lyric_handles_intro_middle_and_end() {
        let lines = vec![
            LyricLine {
                time_ms: 10_000,
                text: "Line 1".to_string(),
            },
            LyricLine {
                time_ms: 20_000,
                text: "Line 2".to_string(),
            },
            LyricLine {
                time_ms: 30_000,
                text: "Line 3".to_string(),
            },
        ];

        let intro = active_lyric(&lines, 5_000);
        assert_eq!(intro.current, None);
        assert_eq!(intro.next.map(|l| l.text.as_str()), Some("Line 1"));
        assert_eq!(intro.index, None);

        let exact = active_lyric(&lines, 20_000);
        assert_eq!(exact.current.map(|l| l.text.as_str()), Some("Line 2"));
        assert_eq!(exact.next.map(|l| l.text.as_str()), Some("Line 3"));
        assert_eq!(exact.index, Some(1));

        let end = active_lyric(&lines, 45_000);
        assert_eq!(end.current.map(|l| l.text.as_str()), Some("Line 3"));
        assert_eq!(end.next, None);
        assert_eq!(end.index, Some(2));
    }

    #[test]
    fn cleans_youtube_and_topic_metadata() {
        assert_eq!(
            clean_track_title("In A Good Way (Official Music Video)"),
            "In A Good Way"
        );
        assert_eq!(
            clean_track_title("Ripples [Official Audio] (Lyrics)"),
            "Ripples"
        );
        assert_eq!(
            clean_track_title("Dreams - 2004 Remaster"),
            "Dreams"
        );
        assert_eq!(clean_artist_name("Faye Webster - Topic"), "Faye Webster");
    }

    #[test]
    fn search_picker_prefers_synced_and_close_duration() {
        let records = vec![
            LrcLibRecord {
                track_name: "Song".to_string(),
                artist_name: "Artist".to_string(),
                duration: Some(200.0),
                instrumental: false,
                synced_lyrics: None,
                plain_lyrics: Some("Plain only".to_string()),
            },
            LrcLibRecord {
                track_name: "Song (Long)".to_string(),
                artist_name: "Artist".to_string(),
                duration: Some(320.0),
                instrumental: false,
                synced_lyrics: Some("[00:10.00] Wrong edit".to_string()),
                plain_lyrics: None,
            },
            LrcLibRecord {
                track_name: "Song".to_string(),
                artist_name: "Artist".to_string(),
                duration: Some(201.0),
                instrumental: false,
                synced_lyrics: Some("[00:10.00] Exact match".to_string()),
                plain_lyrics: None,
            },
        ];

        let best = pick_best_search_match(&records, Some(200)).unwrap();
        assert_eq!(best.duration, Some(201.0));
        assert!(best.synced_lyrics.as_deref().unwrap().contains("Exact match"));
    }

    #[test]
    fn disk_cache_roundtrip_and_not_found_expiry() {
        let unique = format!(
            "dmc_cache_test_{}_{}",
            std::process::id(),
            now_unix_secs()
        );
        let dir = env::temp_dir().join(unique);
        let cache = LyricsCache::new(dir.clone(), true);

        let lines = vec![LyricLine {
            time_ms: 12_340,
            text: "Hello desk".to_string(),
        }];
        cache.save_at(
            "Faye Webster",
            "In A Good Way",
            Some(224),
            &LyricsContent::Synced(lines.clone()),
            Some("[00:12.34] Hello desk\n"),
            1_000,
        );
        assert_eq!(
            cache.load_at("faye webster", "  In A Good Way ", Some(223), 100_000),
            Some(LyricsContent::Synced(lines))
        );

        cache.save_at(
            "Artist",
            "Missing Song",
            Some(180),
            &LyricsContent::NotFound,
            None,
            1_000,
        );
        assert_eq!(
            cache.load_at("Artist", "Missing Song", Some(180), 1_000 + 3_600),
            Some(LyricsContent::NotFound)
        );
        assert_eq!(
            cache.load_at("Artist", "Missing Song", Some(180), 1_000 + NOT_FOUND_TTL_SECS),
            None
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
