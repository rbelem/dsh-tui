//! OSC 52 clipboard writer (#12): `ESC ] 52 ; c ; <base64> ESC \`.
//!
//! Terminal-mediated copy — the emulator owns the system clipboard, so no
//! external binary or X11/Wayland dependency is needed. Best-effort:
//! terminals without OSC 52 support silently ignore the sequence. The
//! Shift+drag / Shift+wheel escape hatch is documented in the issue: the
//! terminal's own selection always wins over this sequence.

use base64::Engine;

/// The OSC 52 copy sequence for `text` (base64 payload, no chunking — the
/// payloads here are chat selections, well under terminal limits).
pub fn osc52_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    format!("\x1b]52;c;{encoded}\x1b\\")
}

/// Write `text` to the system clipboard via OSC 52 on stdout (best-effort:
/// a non-terminal stdout or a terminal without OSC 52 support ignores it).
/// Returns the number of CHARS copied (the status flash's count).
pub fn copy_text(text: &str) -> usize {
    use std::io::Write;
    let _ = std::io::stdout().write_all(osc52_sequence(text).as_bytes());
    let _ = std::io::stdout().flush();
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_sequence_encodes_base64() {
        assert_eq!(osc52_sequence("hi"), "\x1b]52;c;aGk=\x1b\\");
        assert_eq!(osc52_sequence(""), "\x1b]52;c;\x1b\\");
        // Non-ASCII payloads ride the UTF-8 bytes (base64 of the bytes).
        assert_eq!(osc52_sequence("héllo"), "\x1b]52;c;aMOpbGxv\x1b\\");
    }
}
