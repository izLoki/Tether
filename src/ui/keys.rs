//! egui key events -> xterm byte sequences.

use eframe::egui::{Key, Modifiers};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(m: Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

fn cursor_key(letter: char, m: Modifiers, app_cursor: bool) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 {
        format!("\x1b[1;{p}{letter}").into_bytes()
    } else if app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn tilde_key(code: u8, m: Modifiers) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 { format!("\x1b[{code};{p}~").into_bytes() } else { format!("\x1b[{code}~").into_bytes() }
}

fn ctrl_byte(key: Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_uppercase() {
            return Some(c - b'A' + 1);
        }
    }
    match key {
        Key::Space | Key::Num2 => Some(0x00),
        Key::OpenBracket => Some(0x1b),
        Key::Backslash => Some(0x1c),
        Key::CloseBracket => Some(0x1d),
        Key::Num6 => Some(0x1e),
        Key::Minus | Key::Slash => Some(0x1f),
        _ => None,
    }
}

/// Bytes for a key press, or `None` when the key is handled as text / clipboard.
pub fn encode(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let bytes = match key {
        // Meta+Enter is understood as an inserted newline by agent TUIs. Shell
        // integration binds the same sequence to each shell's multiline editor.
        Key::Enter if m.shift && !m.ctrl && !m.alt => b"\x1b\r".to_vec(),
        Key::Enter => b"\r".to_vec(),
        Key::Backspace if m.ctrl => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Escape => vec![0x1b],
        Key::ArrowUp => cursor_key('A', m, app_cursor),
        Key::ArrowDown => cursor_key('B', m, app_cursor),
        Key::ArrowRight => cursor_key('C', m, app_cursor),
        Key::ArrowLeft => cursor_key('D', m, app_cursor),
        Key::Home => cursor_key('H', m, app_cursor),
        Key::End => cursor_key('F', m, app_cursor),
        Key::Insert => tilde_key(2, m),
        Key::Delete => tilde_key(3, m),
        Key::PageUp => tilde_key(5, m),
        Key::PageDown => tilde_key(6, m),
        Key::F1 => b"\x1bOP".to_vec(),
        Key::F2 => b"\x1bOQ".to_vec(),
        Key::F3 => b"\x1bOR".to_vec(),
        Key::F4 => b"\x1bOS".to_vec(),
        Key::F5 => tilde_key(15, m),
        Key::F6 => tilde_key(17, m),
        Key::F7 => tilde_key(18, m),
        Key::F8 => tilde_key(19, m),
        Key::F9 => tilde_key(20, m),
        Key::F10 => tilde_key(21, m),
        Key::F11 => tilde_key(23, m),
        Key::F12 => tilde_key(24, m),
        // Ctrl+C/V/X arrive as Copy/Paste/Cut events and are handled there.
        Key::C | Key::V | Key::X if m.ctrl && !m.alt => return None,
        // Ctrl+Alt is deliberately unmapped: Windows reports AltGr as Ctrl+Alt, and
        // AltGr combos (e.g. `@` on many layouts) must arrive only as Text.
        _ if m.ctrl && !m.alt => vec![ctrl_byte(key)?],
        _ => return None,
    };
    Some(bytes)
}

/// Text typed while Alt is held is sent with an ESC prefix (meta key).
pub fn encode_text(text: &str, alt: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    if alt {
        out.push(0x1b);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

/// Pasted text is normalized to CR line endings and stripped of control characters,
/// so a clipboard containing `ESC[201~` can't end bracketed paste early and run the
/// rest as typed commands.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized: String = text.replace("\r\n", "\r").replace('\n', "\r").chars().filter(|&c| c == '\r' || c == '\t' || !c.is_control()).collect();
    if normalized.is_empty() {
        return Vec::new();
    }
    if bracketed { format!("\x1b[200~{normalized}\x1b[201~").into_bytes() } else { normalized.into_bytes() }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;
    fn ctrl() -> Modifiers {
        Modifiers { ctrl: true, command: true, ..Modifiers::NONE }
    }

    #[test]
    fn basic_keys() {
        assert_eq!(encode(Key::Enter, NONE, false).unwrap(), b"\r");
        assert_eq!(encode(Key::Backspace, NONE, false).unwrap(), [0x7f]);
        assert_eq!(encode(Key::Delete, NONE, false).unwrap(), b"\x1b[3~");
        assert_eq!(encode(Key::F5, NONE, false).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn shift_enter_does_not_submit() {
        assert_eq!(encode(Key::Enter, Modifiers::SHIFT, false).unwrap(), b"\x1b\r");
        assert_eq!(encode(Key::Enter, NONE, false).unwrap(), b"\r");
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        assert_eq!(encode(Key::ArrowUp, NONE, false).unwrap(), b"\x1b[A");
        assert_eq!(encode(Key::ArrowUp, NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(encode(Key::ArrowLeft, ctrl(), true).unwrap(), b"\x1b[1;5D");
    }

    #[test]
    fn ctrl_letters_become_control_codes() {
        assert_eq!(encode(Key::D, ctrl(), false).unwrap(), [0x04]);
        assert_eq!(encode(Key::L, ctrl(), false).unwrap(), [0x0c]);
        assert_eq!(encode(Key::OpenBracket, ctrl(), false).unwrap(), [0x1b]);
        assert!(encode(Key::C, ctrl(), false).is_none(), "Ctrl+C goes through the Copy event");
        assert!(encode(Key::A, NONE, false).is_none(), "plain letters arrive as Text");
    }

    #[test]
    fn paste_normalizes_newlines_and_brackets() {
        assert!(encode_paste("", true).is_empty(), "an image-only drop must not trigger the CLI's native clipboard paste");
        assert!(encode_paste("", false).is_empty());
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(encode_paste("x", true), b"\x1b[200~x\x1b[201~");
        assert_eq!(encode_paste("a\x1b[201~rm -rf ~\tb\x07", true), b"\x1b[200~a[201~rm -rf ~\tb\x1b[201~");
    }

    #[test]
    fn altgr_is_left_to_text_events() {
        let altgr = Modifiers { ctrl: true, alt: true, ..Modifiers::NONE };
        assert!(encode(Key::Q, altgr, false).is_none());
    }

    #[test]
    fn alt_text_prefixes_escape() {
        assert_eq!(encode_text("b", true), b"\x1bb");
        assert_eq!(encode_text("b", false), b"b");
    }
}
