//! Maps Warp keyboard input onto RMUX's `send_text` / `send_key` surface.

/// One input action to forward to an RMUX pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmuxInputAction {
    /// Forward literal UTF-8 text via `Pane::send_text`.
    Text(String),
    /// Forward a tmux-compatible key token via `Pane::send_key`.
    Key(&'static str),
}

/// Maps a printable-text input event onto `Pane::send_text`.
///
/// Printable text never carries key semantics of its own, so this is a
/// direct, infallible wrap.
pub fn map_text_input(text: &str) -> RmuxInputAction {
    RmuxInputAction::Text(text.to_owned())
}

/// Maps the raw bytes Warp would otherwise write to a local PTY onto RMUX
/// input actions.
///
/// `TerminalView` already funnels keyboard handling (printable text,
/// Enter/Tab/Backspace/Escape/arrows, and Ctrl-C/D/L) through
/// `Event::WriteBytesToPty { bytes }` by the time it reaches a
/// `TerminalManager`, so this is the actual interception point used by
/// [`super::terminal_manager::RmuxTerminalManager`]: recognized control
/// bytes/escape sequences are re-expressed as RMUX's tmux-compatible
/// `Pane::send_key` tokens, and everything else (printable UTF-8) is
/// forwarded verbatim via [`map_text_input`]/`Pane::send_text`.
pub fn map_pty_bytes_to_rmux_input(bytes: &[u8]) -> RmuxInputAction {
    match bytes {
        b"\r" | b"\n" => RmuxInputAction::Key("Enter"),
        b"\t" => RmuxInputAction::Key("Tab"),
        b"\x7f" | b"\x08" => RmuxInputAction::Key("BSpace"),
        b"\x1b" => RmuxInputAction::Key("Escape"),
        b"\x1b[A" => RmuxInputAction::Key("Up"),
        b"\x1b[B" => RmuxInputAction::Key("Down"),
        b"\x1b[C" => RmuxInputAction::Key("Right"),
        b"\x1b[D" => RmuxInputAction::Key("Left"),
        b"\x03" => RmuxInputAction::Key("C-c"),
        b"\x04" => RmuxInputAction::Key("C-d"),
        b"\x0c" => RmuxInputAction::Key("C-l"),
        _ => map_text_input(&String::from_utf8_lossy(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_text_goes_through_send_text() {
        assert_eq!(
            map_text_input("hello"),
            RmuxInputAction::Text("hello".to_owned())
        );
    }

    #[test]
    fn pty_bytes_recognizes_named_keys_and_control_chords() {
        let cases: &[(&[u8], RmuxInputAction)] = &[
            (b"\r", RmuxInputAction::Key("Enter")),
            (b"\n", RmuxInputAction::Key("Enter")),
            (b"\t", RmuxInputAction::Key("Tab")),
            (b"\x7f", RmuxInputAction::Key("BSpace")),
            (b"\x1b", RmuxInputAction::Key("Escape")),
            (b"\x1b[A", RmuxInputAction::Key("Up")),
            (b"\x1b[B", RmuxInputAction::Key("Down")),
            (b"\x1b[C", RmuxInputAction::Key("Right")),
            (b"\x1b[D", RmuxInputAction::Key("Left")),
            (b"\x03", RmuxInputAction::Key("C-c")),
            (b"\x04", RmuxInputAction::Key("C-d")),
            (b"\x0c", RmuxInputAction::Key("C-l")),
        ];
        for (bytes, expected) in cases {
            assert_eq!(map_pty_bytes_to_rmux_input(bytes), *expected);
        }
    }

    #[test]
    fn pty_bytes_falls_back_to_text_for_printable_utf8() {
        assert_eq!(
            map_pty_bytes_to_rmux_input("héllo".as_bytes()),
            RmuxInputAction::Text("héllo".to_owned())
        );
    }
}
