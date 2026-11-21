use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode,
    GetNumberOfConsoleInputEvents, GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
    STD_INPUT_HANDLE, SetConsoleMode,
};

const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;

/// Actions triggered interactively from the terminal (mirrors the physical desk controller inputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Prev,
    PlayPause,
    Next,
    VolumeDelta(i32),
    ToggleMute,
    Quit,
}

/// Maps a Windows virtual-key code / Unicode character pair into an `InputAction`.
pub fn translate_key(vk: u16, ch: char) -> Option<InputAction> {
    match vk {
        VK_LEFT => return Some(InputAction::Prev),
        VK_SPACE => return Some(InputAction::PlayPause),
        VK_RIGHT => return Some(InputAction::Next),
        VK_UP | VK_ADD => return Some(InputAction::VolumeDelta(1)),
        VK_DOWN | VK_SUBTRACT => return Some(InputAction::VolumeDelta(-1)),
        VK_ESCAPE => return Some(InputAction::Quit),
        _ => {}
    }

    match ch.to_ascii_lowercase() {
        'p' | 'j' => Some(InputAction::Prev),
        'k' => Some(InputAction::PlayPause),
        'n' | 'l' => Some(InputAction::Next),
        '+' | '=' => Some(InputAction::VolumeDelta(1)),
        '-' | '_' => Some(InputAction::VolumeDelta(-1)),
        'm' => Some(InputAction::ToggleMute),
        'q' => Some(InputAction::Quit),
        _ => None,
    }
}

/// Non-blocking Windows console input reader that restores original console mode on drop.
pub struct ConsoleInput {
    handle: HANDLE,
    original_mode: CONSOLE_MODE,
    enabled: bool,
}

impl ConsoleInput {
    pub fn new() -> Self {
        let Ok(handle) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
            return Self {
                handle: HANDLE::default(),
                original_mode: CONSOLE_MODE(0),
                enabled: false,
            };
        };

        if handle.is_invalid() {
            return Self {
                handle,
                original_mode: CONSOLE_MODE(0),
                enabled: false,
            };
        }

        let mut original_mode = CONSOLE_MODE(0);
        if unsafe { GetConsoleMode(handle, &mut original_mode) }.is_err() {
            return Self {
                handle,
                original_mode,
                enabled: false,
            };
        }

        // Disable line buffering and echo while preserving Ctrl+C signal processing.
        let raw_mode = CONSOLE_MODE(
            (original_mode.0 & !(ENABLE_LINE_INPUT.0 | ENABLE_ECHO_INPUT.0))
                | ENABLE_PROCESSED_INPUT.0,
        );
        let enabled = unsafe { SetConsoleMode(handle, raw_mode) }.is_ok();

        Self {
            handle,
            original_mode,
            enabled,
        }
    }

    /// Drains any pending keyboard events without blocking.
    pub fn poll_actions(&self) -> Vec<InputAction> {
        if !self.enabled {
            return Vec::new();
        }

        let mut pending = 0u32;
        if unsafe { GetNumberOfConsoleInputEvents(self.handle, &mut pending) }.is_err()
            || pending == 0
        {
            return Vec::new();
        }

        let to_read = (pending as usize).min(32);
        let mut records = vec![INPUT_RECORD::default(); to_read];
        let mut read_count = 0u32;
        if unsafe { ReadConsoleInputW(self.handle, &mut records[..to_read], &mut read_count) }
            .is_err()
        {
            return Vec::new();
        }

        let mut actions = Vec::new();
        for rec in &records[..read_count as usize] {
            if rec.EventType as u32 != KEY_EVENT {
                continue;
            }
            let key = unsafe { rec.Event.KeyEvent };
            if !key.bKeyDown.as_bool() {
                continue;
            }
            let ch = char::from_u32(unsafe { key.uChar.UnicodeChar } as u32).unwrap_or('\0');
            if let Some(action) = translate_key(key.wVirtualKeyCode, ch) {
                match action {
                    InputAction::VolumeDelta(delta) => {
                        let repeat = (key.wRepeatCount as i32).clamp(1, 5);
                        actions.push(InputAction::VolumeDelta(delta * repeat));
                    }
                    other => actions.push(other),
                }
            }
        }

        actions
    }
}

impl Drop for ConsoleInput {
    fn drop(&mut self) {
        if self.enabled {
            unsafe {
                let _ = SetConsoleMode(self.handle, self.original_mode);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_arrow_and_shortcut_keys() {
        assert_eq!(translate_key(VK_LEFT, '\0'), Some(InputAction::Prev));
        assert_eq!(translate_key(0, 'P'), Some(InputAction::Prev));
        assert_eq!(translate_key(VK_SPACE, ' '), Some(InputAction::PlayPause));
        assert_eq!(translate_key(0, 'k'), Some(InputAction::PlayPause));
        assert_eq!(translate_key(VK_RIGHT, '\0'), Some(InputAction::Next));
        assert_eq!(translate_key(0, 'n'), Some(InputAction::Next));
        assert_eq!(
            translate_key(VK_UP, '\0'),
            Some(InputAction::VolumeDelta(1))
        );
        assert_eq!(translate_key(0, '+'), Some(InputAction::VolumeDelta(1)));
        assert_eq!(
            translate_key(VK_DOWN, '\0'),
            Some(InputAction::VolumeDelta(-1))
        );
        assert_eq!(translate_key(0, '-'), Some(InputAction::VolumeDelta(-1)));
        assert_eq!(translate_key(0, 'm'), Some(InputAction::ToggleMute));
        assert_eq!(translate_key(0, 'q'), Some(InputAction::Quit));
        assert_eq!(translate_key(VK_ESCAPE, '\u{1b}'), Some(InputAction::Quit));
        assert_eq!(translate_key(0, 'x'), None);
    }
}
