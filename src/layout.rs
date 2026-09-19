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
    let stale = guard.as_ref().map(|c| c.at.elapsed() > TTL).unwrap_or(true);
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

#[cfg(all(target_os = "linux", not(test)))]
fn probe_layout() -> HashMap<char, char> {
    linux::probe()
}

#[cfg(all(not(any(target_os = "macos", target_os = "linux")), not(test)))]
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
                if let Some(Ok(ch)) = char::decode_utf16(buf[..len as usize].iter().copied()).next()
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

#[cfg(all(target_os = "linux", not(test)))]
mod linux {
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    use xkbcommon_dl::{xkb_keymap_compile_flags, xkb_rule_names, xkbcommon_option, XkbCommon};

    fn current_layout() -> String {
        if let Ok(l) = std::env::var("XKB_DEFAULT_LAYOUT") {
            let first = l.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
        if let Ok(raw) = std::fs::read_to_string("/etc/default/keyboard") {
            for line in raw.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("XKBLAYOUT=") {
                    let v = rest.trim().trim_matches('"').trim_matches('\'');
                    let first = v.split(',').next().unwrap_or("").trim();
                    if !first.is_empty() {
                        return first.to_string();
                    }
                }
            }
        }
        String::new()
    }

    fn utf32(xkb: &XkbCommon, state: *mut xkbcommon_dl::xkb_state, kc: u32) -> Option<char> {
        let n = unsafe { (xkb.xkb_state_key_get_utf32)(state, kc) };
        char::from_u32(n).filter(|c| !c.is_ascii_control() && *c != '\0')
    }

    pub fn probe() -> HashMap<char, char> {
        let mut out = HashMap::new();
        let Some(xkb) = xkbcommon_option() else {
            return out;
        };
        let layout = current_layout();
        if layout.is_empty() || layout == "us" {
            return out;
        }
        let Ok(layout_c) = CString::new(layout) else {
            return out;
        };
        let us_c = CString::new("us").expect("us");
        unsafe {
            let ctx = (xkb.xkb_context_new)(xkbcommon_dl::xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if ctx.is_null() {
                return out;
            }
            let cur_names = xkb_rule_names {
                rules: ptr::null(),
                model: ptr::null(),
                layout: layout_c.as_ptr(),
                variant: ptr::null(),
                options: ptr::null(),
            };
            let us_names = xkb_rule_names {
                rules: ptr::null(),
                model: ptr::null(),
                layout: us_c.as_ptr(),
                variant: ptr::null(),
                options: ptr::null(),
            };
            let flags = xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS;
            let cur_map = (xkb.xkb_keymap_new_from_names)(ctx, &cur_names, flags);
            let us_map = (xkb.xkb_keymap_new_from_names)(ctx, &us_names, flags);
            if cur_map.is_null() || us_map.is_null() {
                if !cur_map.is_null() {
                    (xkb.xkb_keymap_unref)(cur_map);
                }
                if !us_map.is_null() {
                    (xkb.xkb_keymap_unref)(us_map);
                }
                (xkb.xkb_context_unref)(ctx);
                return out;
            }
            let cur_state = (xkb.xkb_state_new)(cur_map);
            let us_state = (xkb.xkb_state_new)(us_map);
            if !cur_state.is_null() && !us_state.is_null() {
                let min = (xkb.xkb_keymap_min_keycode)(cur_map);
                let max = (xkb.xkb_keymap_max_keycode)(cur_map);
                for kc in min..=max {
                    let Some(us) = utf32(xkb, us_state, kc) else {
                        continue;
                    };
                    if !us.is_ascii_lowercase() {
                        continue;
                    }
                    let Some(cur) = utf32(xkb, cur_state, kc) else {
                        continue;
                    };
                    if cur != us {
                        out.insert(cur, us);
                    }
                }
            }
            if !cur_state.is_null() {
                (xkb.xkb_state_unref)(cur_state);
            }
            if !us_state.is_null() {
                (xkb.xkb_state_unref)(us_state);
            }
            (xkb.xkb_keymap_unref)(cur_map);
            (xkb.xkb_keymap_unref)(us_map);
            (xkb.xkb_context_unref)(ctx);
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
