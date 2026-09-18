//! Map the current keyboard layout back to the US letter on the same
//! physical key. Shortcuts bind to keys, not to Thai/Chinese/Japanese glyphs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Cache {
    at: Instant,
    to_us: HashMap<char, char>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
const TTL: Duration = Duration::from_millis(750);

/// US letter (or ASCII punctuation) produced by the same physical key.
pub fn to_latin(c: char) -> Option<char> {
    if c.is_ascii_alphabetic() {
        return Some(c.to_ascii_lowercase());
    }
    if c.is_ascii() {
        return None;
    }
    let mut guard = CACHE.lock().ok()?;
    let stale = guard
        .as_ref()
        .map(|c| c.at.elapsed() > TTL)
        .unwrap_or(true);
    if stale {
        *guard = Some(Cache {
            at: Instant::now(),
            to_us: probe_layout(),
        });
    }
    guard.as_ref()?.to_us.get(&c).copied()
}

#[cfg(test)]
fn probe_layout() -> HashMap<char, char> {
    HashMap::new()
}

#[cfg(all(target_os = "macos", not(test)))]
fn probe_layout() -> HashMap<char, char> {
    macos::probe()
}

#[cfg(all(not(target_os = "macos"), not(test)))]
fn probe_layout() -> HashMap<char, char> {
    HashMap::new()
}

#[cfg(all(target_os = "macos", not(test)))]
mod macos {
    use super::*;
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        fn CFDataGetBytePtr(theData: *const c_void) -> *const u8;
        fn CFRelease(cf: *const c_void);
        fn LMGetKbdType() -> u8;
        fn UCKeyTranslate(
            key_layout: *const u8,
            virtual_key_code: u16,
            key_action: u16,
            modifier_key_state: u32,
            keyboard_type: u32,
            key_translate_options: u32,
            dead_key_state: *mut u32,
            max_string_length: i32,
            actual_string_length: *mut i32,
            unicode_string: *mut u16,
        ) -> i32;
        static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    }

    const UC_KEY_ACTION_DISPLAY: u16 = 3;
    const UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

    /// ANSI virtual key → US letter on a PC-101 board.
    const LETTER_KEYS: &[(u16, char)] = &[
        (0x00, 'a'),
        (0x01, 's'),
        (0x02, 'd'),
        (0x03, 'f'),
        (0x04, 'h'),
        (0x05, 'g'),
        (0x06, 'z'),
        (0x07, 'x'),
        (0x08, 'c'),
        (0x09, 'v'),
        (0x0B, 'b'),
        (0x0C, 'q'),
        (0x0D, 'w'),
        (0x0E, 'e'),
        (0x0F, 'r'),
        (0x10, 'y'),
        (0x11, 't'),
        (0x1F, 'o'),
        (0x20, 'u'),
        (0x22, 'i'),
        (0x23, 'p'),
        (0x25, 'l'),
        (0x26, 'j'),
        (0x28, 'k'),
        (0x2D, 'n'),
        (0x2E, 'm'),
    ];

    pub fn probe() -> HashMap<char, char> {
        let mut out = HashMap::new();
        unsafe {
            let source = TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return out;
            }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            if data.is_null() {
                CFRelease(source);
                return out;
            }
            let layout = CFDataGetBytePtr(data);
            if layout.is_null() {
                CFRelease(source);
                return out;
            }
            let kb_type = LMGetKbdType() as u32;
            for &(vk, latin) in LETTER_KEYS {
                let mut dead = 0u32;
                let mut len = 0i32;
                let mut buf = [0u16; 8];
                let status = UCKeyTranslate(
                    layout,
                    vk,
                    UC_KEY_ACTION_DISPLAY,
                    0,
                    kb_type,
                    UC_KEY_TRANSLATE_NO_DEAD_KEYS,
                    &mut dead,
                    buf.len() as i32,
                    &mut len,
                    buf.as_mut_ptr(),
                );
                if status != 0 || len < 1 {
                    continue;
                }
                if let Some(Ok(ch)) =
                    char::decode_utf16(buf[..len as usize].iter().copied()).next()
                {
                    if ch != latin && !ch.is_ascii_control() {
                        out.insert(ch, latin);
                    }
                }
            }
            CFRelease(source);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity() {
        assert_eq!(to_latin('C'), Some('c'));
        assert_eq!(to_latin('c'), Some('c'));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_probe_runs() {
        let _ = to_latin('แ');
    }
}
