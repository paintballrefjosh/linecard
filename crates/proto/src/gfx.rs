//! Graphical sessions (VNC, RDP): what they report and what they accept.
//!
//! The remote screen arrives as rectangles of RGBA pixels; input goes the
//! other way as pointer, wheel and key events. Keys are given the way a
//! browser reports them (`KeyboardEvent.code` and `.key`) and each protocol
//! maps them: X11 keysyms for VNC, scancodes for RDP.

use std::io;

use crate::{AuthPrompt, HostKeyPrompt, PromptReply};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GfxEvent {
    /// Logged in; the remote screen is `width`×`height`.
    Connected {
        width: u16,
        height: u16,
        name: String,
    },
    /// The remote screen changed size.
    Resized {
        width: u16,
        height: u16,
    },
    /// New pixels: `rgba` is `width * height * 4` bytes, row by row.
    Frame {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        rgba: Vec<u8>,
    },
    /// A password or other credentials are needed; answer with `reply`.
    Auth(AuthPrompt),
    /// The server's certificate or key needs the user's decision.
    HostKey(HostKeyPrompt),
    Bell,
    /// Text the remote side copied.
    Clipboard(String),
    /// Ended; `None` for a clean close.
    Disconnected {
        reason: Option<String>,
    },
}

pub type GfxSink = Box<dyn FnMut(GfxEvent) + Send>;

/// Mouse buttons held, as a bit mask (left 1, middle 2, right 4).
pub type Buttons = u8;

pub trait GfxSession: Send {
    fn pointer(&mut self, x: u16, y: u16, buttons: Buttons) -> io::Result<()>;
    /// `dy` > 0 scrolls down; `dx` > 0 scrolls right (in notches).
    fn wheel(&mut self, x: u16, y: u16, dx: i16, dy: i16) -> io::Result<()>;
    fn key(&mut self, code: &str, key: &str, down: bool) -> io::Result<()>;
    /// Type text (paste).
    fn text(&mut self, text: &str) -> io::Result<()>;
    fn reply(&mut self, reply: PromptReply) -> io::Result<()>;
    /// The window showing the session changed size (RDP can follow it).
    fn resize(&mut self, width: u16, height: u16) -> io::Result<()>;
    fn close(&mut self) -> io::Result<()>;
}

/// X11 keysym for a key, as VNC wants it. `key` is the character typed
/// (when there is one); `code` the physical key.
pub fn keysym(code: &str, key: &str) -> Option<u32> {
    let named = match key {
        "Backspace" => 0xff08,
        "Tab" => 0xff09,
        "Enter" => 0xff0d,
        "Escape" => 0xff1b,
        "Delete" => 0xffff,
        "Home" => 0xff50,
        "End" => 0xff57,
        "PageUp" => 0xff55,
        "PageDown" => 0xff56,
        "ArrowLeft" => 0xff51,
        "ArrowUp" => 0xff52,
        "ArrowRight" => 0xff53,
        "ArrowDown" => 0xff54,
        "Insert" => 0xff63,
        "CapsLock" => 0xffe5,
        "NumLock" => 0xff7f,
        "ScrollLock" => 0xff14,
        "Pause" => 0xff13,
        "PrintScreen" => 0xff61,
        "ContextMenu" => 0xff67,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    let modifier = match code {
        "ShiftLeft" => 0xffe1,
        "ShiftRight" => 0xffe2,
        "ControlLeft" => 0xffe3,
        "ControlRight" => 0xffe4,
        "AltLeft" => 0xffe9,
        "AltRight" => 0xffea,
        "MetaLeft" => 0xffeb,
        "MetaRight" => 0xffec,
        _ => 0,
    };
    if modifier != 0 {
        return Some(modifier);
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0xffbd + n);
        }
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => {
            let cp = c as u32;
            Some(if (0x20..=0x7e).contains(&cp) || (0xa0..=0xff).contains(&cp) {
                cp
            } else {
                0x0100_0000 + cp
            })
        }
        _ => None,
    }
}

/// PC/AT set-1 scancode for a key, as RDP wants it: (code, extended).
pub fn scancode(code: &str) -> Option<(u8, bool)> {
    let plain = |c: u8| Some((c, false));
    let ext = |c: u8| Some((c, true));
    match code {
        "Escape" => plain(0x01),
        "Digit1" => plain(0x02),
        "Digit2" => plain(0x03),
        "Digit3" => plain(0x04),
        "Digit4" => plain(0x05),
        "Digit5" => plain(0x06),
        "Digit6" => plain(0x07),
        "Digit7" => plain(0x08),
        "Digit8" => plain(0x09),
        "Digit9" => plain(0x0a),
        "Digit0" => plain(0x0b),
        "Minus" => plain(0x0c),
        "Equal" => plain(0x0d),
        "Backspace" => plain(0x0e),
        "Tab" => plain(0x0f),
        "KeyQ" => plain(0x10),
        "KeyW" => plain(0x11),
        "KeyE" => plain(0x12),
        "KeyR" => plain(0x13),
        "KeyT" => plain(0x14),
        "KeyY" => plain(0x15),
        "KeyU" => plain(0x16),
        "KeyI" => plain(0x17),
        "KeyO" => plain(0x18),
        "KeyP" => plain(0x19),
        "BracketLeft" => plain(0x1a),
        "BracketRight" => plain(0x1b),
        "Enter" => plain(0x1c),
        "ControlLeft" => plain(0x1d),
        "KeyA" => plain(0x1e),
        "KeyS" => plain(0x1f),
        "KeyD" => plain(0x20),
        "KeyF" => plain(0x21),
        "KeyG" => plain(0x22),
        "KeyH" => plain(0x23),
        "KeyJ" => plain(0x24),
        "KeyK" => plain(0x25),
        "KeyL" => plain(0x26),
        "Semicolon" => plain(0x27),
        "Quote" => plain(0x28),
        "Backquote" => plain(0x29),
        "ShiftLeft" => plain(0x2a),
        "Backslash" => plain(0x2b),
        "KeyZ" => plain(0x2c),
        "KeyX" => plain(0x2d),
        "KeyC" => plain(0x2e),
        "KeyV" => plain(0x2f),
        "KeyB" => plain(0x30),
        "KeyN" => plain(0x31),
        "KeyM" => plain(0x32),
        "Comma" => plain(0x33),
        "Period" => plain(0x34),
        "Slash" => plain(0x35),
        "ShiftRight" => plain(0x36),
        "NumpadMultiply" => plain(0x37),
        "AltLeft" => plain(0x38),
        "Space" => plain(0x39),
        "CapsLock" => plain(0x3a),
        "F1" => plain(0x3b),
        "F2" => plain(0x3c),
        "F3" => plain(0x3d),
        "F4" => plain(0x3e),
        "F5" => plain(0x3f),
        "F6" => plain(0x40),
        "F7" => plain(0x41),
        "F8" => plain(0x42),
        "F9" => plain(0x43),
        "F10" => plain(0x44),
        "NumLock" => plain(0x45),
        "ScrollLock" => plain(0x46),
        "Numpad7" => plain(0x47),
        "Numpad8" => plain(0x48),
        "Numpad9" => plain(0x49),
        "NumpadSubtract" => plain(0x4a),
        "Numpad4" => plain(0x4b),
        "Numpad5" => plain(0x4c),
        "Numpad6" => plain(0x4d),
        "NumpadAdd" => plain(0x4e),
        "Numpad1" => plain(0x4f),
        "Numpad2" => plain(0x50),
        "Numpad3" => plain(0x51),
        "Numpad0" => plain(0x52),
        "NumpadDecimal" => plain(0x53),
        "IntlBackslash" => plain(0x56),
        "F11" => plain(0x57),
        "F12" => plain(0x58),
        "NumpadEnter" => ext(0x1c),
        "ControlRight" => ext(0x1d),
        "NumpadDivide" => ext(0x35),
        "PrintScreen" => ext(0x37),
        "AltRight" => ext(0x38),
        "Home" => ext(0x47),
        "ArrowUp" => ext(0x48),
        "PageUp" => ext(0x49),
        "ArrowLeft" => ext(0x4b),
        "ArrowRight" => ext(0x4d),
        "End" => ext(0x4f),
        "ArrowDown" => ext(0x50),
        "PageDown" => ext(0x51),
        "Insert" => ext(0x52),
        "Delete" => ext(0x53),
        "MetaLeft" => ext(0x5b),
        "MetaRight" => ext(0x5c),
        "ContextMenu" => ext(0x5d),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms() {
        assert_eq!(keysym("KeyA", "a"), Some(0x61));
        assert_eq!(keysym("KeyA", "A"), Some(0x41));
        assert_eq!(keysym("Enter", "Enter"), Some(0xff0d));
        assert_eq!(keysym("ShiftLeft", "Shift"), Some(0xffe1));
        assert_eq!(keysym("F5", "F5"), Some(0xffc2));
        assert_eq!(keysym("Digit2", "é"), Some(0xe9), "Latin-1 maps directly");
        assert_eq!(keysym("KeyE", "€"), Some(0x0100_20ac), "others are Unicode keysyms");
        assert_eq!(keysym("Unidentified", "Dead"), None);
    }

    #[test]
    fn scancodes() {
        assert_eq!(scancode("KeyA"), Some((0x1e, false)));
        assert_eq!(scancode("ArrowUp"), Some((0x48, true)));
        assert_eq!(scancode("Nope"), None);
    }
}
