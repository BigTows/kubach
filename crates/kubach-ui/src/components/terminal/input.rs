//! Keyboard and paste encoding for a VT/xterm-compatible remote shell.

use gpui_kit::{Keystroke, Modifiers};

/// Terminal modes that change what keys send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputMode {
    /// DECCKM: arrows send `ESC O x` instead of `ESC [ x`.
    pub app_cursor: bool,
    /// Pastes are wrapped in `ESC[200~ … ESC[201~`.
    pub bracketed_paste: bool,
}

/// Bytes a keystroke sends, or `None` for keys left to the app (anything
/// with the platform modifier, unknown keys).
pub fn encode_keystroke(keystroke: &Keystroke, mode: InputMode) -> Option<Vec<u8>> {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform {
        return None;
    }
    let key = keystroke.key.as_str();
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let param = 1 + modifiers.shift as u8 + 2 * modifiers.alt as u8 + 4 * modifiers.control as u8;
    let alt_prefix = |bytes: &[u8]| -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if modifiers.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };

    let cursor = |letter: u8| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[1;{param}{}", letter as char).into_bytes()
        } else if mode.app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[{code};{param}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };

    let named = match key {
        "enter" => Some(alt_prefix(b"\r")),
        "tab" if modifiers.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(alt_prefix(b"\t")),
        "escape" => Some(alt_prefix(b"\x1b")),
        "backspace" if modifiers.control => Some(alt_prefix(b"\x08")),
        "backspace" => Some(alt_prefix(b"\x7f")),
        "space" if modifiers.control => Some(alt_prefix(b"\0")),
        "up" => Some(cursor(b'A')),
        "down" => Some(cursor(b'B')),
        "right" => Some(cursor(b'C')),
        "left" => Some(cursor(b'D')),
        "home" => Some(cursor(b'H')),
        "end" => Some(cursor(b'F')),
        "insert" => Some(tilde(2)),
        "delete" => Some(tilde(3)),
        "pageup" => Some(tilde(5)),
        "pagedown" => Some(tilde(6)),
        "f1" | "f2" | "f3" | "f4" => {
            let letter = b'P' + (key.as_bytes()[1] - b'1');
            Some(if param > 1 {
                format!("\x1b[1;{param}{}", letter as char).into_bytes()
            } else {
                vec![0x1b, b'O', letter]
            })
        }
        "f5" => Some(tilde(15)),
        "f6" => Some(tilde(17)),
        "f7" => Some(tilde(18)),
        "f8" => Some(tilde(19)),
        "f9" => Some(tilde(20)),
        "f10" => Some(tilde(21)),
        "f11" => Some(tilde(23)),
        "f12" => Some(tilde(24)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    if modifiers.control
        && let Some(byte) = control_byte(key)
    {
        return Some(alt_prefix(&[byte]));
    }

    let typed = keystroke.key_char.as_deref();
    if modifiers.alt && key.chars().count() == 1 && typed.is_none_or(|typed| typed == key) {
        // Alt as Meta when it does not produce a different character.
        return Some(alt_prefix(key.as_bytes()));
    }
    typed
        .filter(|typed| !typed.is_empty())
        .map(|typed| typed.replace('\n', "\r").into_bytes())
}

/// What the terminal does with a key-down event.
///
/// Text reaches the terminal in one of two ways, never both: a key-down the
/// view handles itself ([`KeyAction::Send`], propagation stopped), or text the
/// platform delivers through the view's input handler afterwards
/// ([`KeyAction::Text`], propagation left alone). Every platform skips text
/// input for a key-down that was handled: macOS does not pass it to
/// `interpretKeyEvents`, Windows skips `TranslateMessage` (no `WM_CHAR`), and
/// Linux does not call `replace_text_in_range`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Write these bytes and stop the event: control keys, `ctrl-`/`alt-`
    /// combinations, named keys.
    Send(Vec<u8>),
    /// Plain typed text: leave the event alone so the platform hands the
    /// text (or an IME composition, or a dead-key result) to the input handler.
    Text,
    /// Not the terminal's key (platform shortcuts, unknown keys) or an IME
    /// composition is in progress and owns the keyboard.
    Ignore,
}

/// Routes a key-down: see [`KeyAction`]. `prefer_character_input` is the
/// platform's hint that modifiers produce a character (AltGr and dead keys on
/// Windows); `composing` is whether IME preedit text is shown.
pub fn route_keystroke(
    keystroke: &Keystroke,
    prefer_character_input: bool,
    composing: bool,
    mode: InputMode,
) -> KeyAction {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform || composing {
        return KeyAction::Ignore;
    }
    let typed = keystroke
        .key_char
        .as_deref()
        .is_some_and(|typed| !typed.is_empty() && !typed.chars().any(char::is_control));
    if typed && prefer_character_input {
        return KeyAction::Text;
    }
    let plain = modifiers.is_subset_of(&Modifiers::shift());
    // Linux only inserts text for keys without modifiers other than shift,
    // so anything else keeps going out as bytes from here.
    if typed && plain {
        return KeyAction::Text;
    }
    match encode_keystroke(keystroke, mode) {
        Some(bytes) => KeyAction::Send(bytes),
        None => KeyAction::Ignore,
    }
}

/// Bytes for text the user typed through an IME or a dead key: newlines
/// become carriage returns and control characters are dropped, as in a
/// paste, but the text is never bracketed: it is typing, not a paste.
pub fn encode_text(text: &str) -> Option<Vec<u8>> {
    let text = encode_paste(text, InputMode::default());
    (!text.is_empty()).then_some(text)
}

/// Control character for `ctrl-<key>`.
fn control_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() || !c.is_ascii() {
        return None;
    }
    let byte = match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' | ' ' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '7' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    };
    Some(byte)
}

/// Bytes for pasting `text`: newlines become carriage returns, control
/// characters other than tab and newlines are dropped, and the text is
/// bracketed when the program asked for it.
///
/// Dropping every ESC and C1 control (not only the `ESC[201~` marker) keeps
/// pasted text from ending a bracketed paste early or smuggling escape
/// sequences, and dropping the other C0 controls keeps it from acting as
/// keys (`ctrl-c`, `ctrl-d`, backspace) when the paste is not bracketed.
pub fn encode_paste(text: &str, mode: InputMode) -> Vec<u8> {
    let text: String = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .chars()
        .filter(|&c| is_pasteable(c))
        .collect();
    if mode.bracketed_paste {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// Whether `c` may reach the program in a paste: anything but C0 controls
/// (tab and carriage return excepted), DEL and C1 controls.
fn is_pasteable(c: char) -> bool {
    matches!(c, '\t' | '\r') || !(c.is_ascii_control() || ('\u{80}'..='\u{9f}').contains(&c))
}

#[cfg(test)]
mod tests {
    use gpui_kit::Modifiers;

    use super::*;

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn plain(name: &str, typed: Option<&str>) -> Option<Vec<u8>> {
        encode_keystroke(
            &key(name, typed, Modifiers::default()),
            InputMode::default(),
        )
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Default::default()
        }
    }

    #[test]
    fn printable_and_named_keys() {
        assert_eq!(plain("a", Some("a")).unwrap(), b"a");
        assert_eq!(plain("enter", Some("\n")).unwrap(), b"\r");
        assert_eq!(plain("backspace", None).unwrap(), b"\x7f");
        assert_eq!(plain("tab", Some("\t")).unwrap(), b"\t");
        assert_eq!(plain("escape", None).unwrap(), b"\x1b");
        assert_eq!(plain("delete", None).unwrap(), b"\x1b[3~");
        assert_eq!(plain("f1", None).unwrap(), b"\x1bOP");
        assert_eq!(plain("f5", None).unwrap(), b"\x1b[15~");
        assert_eq!(plain("я", Some("я")).unwrap(), "я".as_bytes());
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(plain("up", None).unwrap(), b"\x1b[A");
        let app = InputMode {
            app_cursor: true,
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&key("left", None, Modifiers::default()), app).unwrap(),
            b"\x1bOD"
        );
        assert_eq!(
            encode_keystroke(&key("right", None, ctrl()), app).unwrap(),
            b"\x1b[1;5C"
        );
    }

    #[test]
    fn control_and_alt_combinations() {
        let encode =
            |k: &str, m: Modifiers| encode_keystroke(&key(k, None, m), InputMode::default());
        assert_eq!(encode("c", ctrl()).unwrap(), [0x03]);
        assert_eq!(encode("d", ctrl()).unwrap(), [0x04]);
        assert_eq!(encode("[", ctrl()).unwrap(), [0x1b]);
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert_eq!(encode("b", alt).unwrap(), b"\x1bb");
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(encode("tab", shift).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn option_characters_are_typed_and_platform_keys_ignored() {
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        let typed = encode_keystroke(&key("s", Some("ß"), alt), InputMode::default());
        assert_eq!(typed.unwrap(), "ß".as_bytes());
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(
            encode_keystroke(&key("c", Some("c"), cmd), InputMode::default()),
            None
        );
    }

    fn route(k: &str, typed: Option<&str>, m: Modifiers) -> KeyAction {
        route_keystroke(&key(k, typed, m), false, false, InputMode::default())
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Default::default()
        }
    }

    fn alt() -> Modifiers {
        Modifiers {
            alt: true,
            ..Default::default()
        }
    }

    #[test]
    fn plain_text_goes_to_the_input_handler() {
        // Left to the platform, which delivers it once through the input
        // handler (insertText / WM_CHAR / replace_text_in_range).
        assert_eq!(route("a", Some("a"), Modifiers::default()), KeyAction::Text);
        assert_eq!(route("a", Some("A"), shift()), KeyAction::Text);
        assert_eq!(
            route("space", Some(" "), Modifiers::default()),
            KeyAction::Text
        );
        assert_eq!(route("я", Some("я"), Modifiers::default()), KeyAction::Text);
    }

    #[test]
    fn control_keys_are_sent_once_from_key_down() {
        assert_eq!(route("c", Some("c"), ctrl()), KeyAction::Send(vec![0x03]));
        assert_eq!(route("c", None, ctrl()), KeyAction::Send(vec![0x03]));
        assert_eq!(
            route("enter", Some("\n"), Modifiers::default()),
            KeyAction::Send(b"\r".to_vec())
        );
        assert_eq!(
            route("tab", Some("\t"), Modifiers::default()),
            KeyAction::Send(b"\t".to_vec())
        );
        assert_eq!(
            route("escape", None, Modifiers::default()),
            KeyAction::Send(b"\x1b".to_vec())
        );
        assert_eq!(
            route("up", None, Modifiers::default()),
            KeyAction::Send(b"\x1b[A".to_vec())
        );
        assert_eq!(route("space", Some(" "), ctrl()), KeyAction::Send(vec![0]));
        // Alt as Meta, and macOS Option characters, stay byte-encoded:
        // Linux would not deliver text with Alt held.
        assert_eq!(route("b", None, alt()), KeyAction::Send(b"\x1bb".to_vec()));
        assert_eq!(
            route("s", Some("ß"), alt()),
            KeyAction::Send("ß".as_bytes().to_vec())
        );
    }

    #[test]
    fn platform_shortcuts_and_compositions_are_left_alone() {
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        assert_eq!(route("c", Some("c"), cmd), KeyAction::Ignore);
        assert_eq!(route("f13", None, Modifiers::default()), KeyAction::Ignore);
        // While preedit is shown the IME owns every key, Ctrl-C included.
        for (k, typed, m) in [
            ("a", Some("a"), Modifiers::default()),
            ("enter", Some("\n"), Modifiers::default()),
            ("escape", None, Modifiers::default()),
            ("c", None, ctrl()),
        ] {
            assert_eq!(
                route_keystroke(&key(k, typed, m), false, true, InputMode::default()),
                KeyAction::Ignore
            );
        }
    }

    #[test]
    fn altgr_and_dead_keys_prefer_character_input() {
        // Windows AltGr: ctrl+alt that produces a character.
        let altgr = Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            route_keystroke(
                &key("q", Some("@"), altgr),
                true,
                false,
                InputMode::default()
            ),
            KeyAction::Text
        );
        // Without the hint the same chord is a control key.
        assert_eq!(
            route_keystroke(
                &key("q", Some("@"), altgr),
                false,
                false,
                InputMode::default()
            ),
            KeyAction::Send(vec![0x1b, 0x11])
        );
    }

    #[test]
    fn committed_text_is_typed_not_pasted() {
        assert_eq!(encode_text("你好"), Some("你好".as_bytes().to_vec()));
        assert_eq!(encode_text("é"), Some("é".as_bytes().to_vec()));
        assert_eq!(encode_text("a\nb"), Some(b"a\rb".to_vec()));
        // No bracketed-paste markers, and no smuggled controls.
        assert_eq!(encode_text("x\x1b[201~"), Some(b"x[201~".to_vec()));
        assert_eq!(encode_text(""), None);
        assert_eq!(encode_text("\x03"), None);
    }

    fn bracketed() -> InputMode {
        InputMode {
            bracketed_paste: true,
            ..Default::default()
        }
    }

    #[test]
    fn paste_converts_newlines_and_brackets() {
        assert_eq!(
            encode_paste("a\nb\r\nc\td", InputMode::default()),
            b"a\rb\rc\td".to_vec()
        );
        assert_eq!(
            encode_paste("x\x1b[201~y", bracketed()),
            b"\x1b[200~x[201~y\x1b[201~".to_vec()
        );
    }

    #[test]
    fn paste_cannot_rebuild_the_end_marker() {
        // A single-pass marker removal would turn this into `ESC[201~`.
        let pasted = encode_paste("ls\x1b[20\x1b[201~1~; rm -rf ~\n", bracketed());
        assert_eq!(
            pasted,
            b"\x1b[200~ls[20[201~1~; rm -rf ~\r\x1b[201~".to_vec()
        );
        let inner = &pasted[6..pasted.len() - 6];
        assert!(!inner.contains(&0x1b));
    }

    #[test]
    fn paste_drops_c1_and_other_controls() {
        // U+009B is CSI on its own; U+0085 (NEL) and U+009D (OSC) are C1 too.
        assert_eq!(
            encode_paste("a\u{9b}201~b\u{85}c\u{9d}d", bracketed()),
            "\x1b[200~a201~bcd\x1b[201~".as_bytes().to_vec()
        );
        // C0 controls and DEL would act as keys in an unbracketed paste.
        assert_eq!(
            encode_paste("a\x03b\x04c\x08d\x7fe\x00f\x1bg", InputMode::default()),
            b"abcdefg".to_vec()
        );
        // Printable text around the C1 range survives.
        assert_eq!(
            encode_paste("~\u{a0}é я 漢", InputMode::default()),
            "~\u{a0}é я 漢".as_bytes().to_vec()
        );
    }
}
