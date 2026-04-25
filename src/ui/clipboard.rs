//! Terminal-side clipboard via OSC 52.
//!
//! Modern terminals (kitty, Windows Terminal, iTerm2, recent xterm,
//! Tabby, ghostty, wezterm, …) interpret the escape sequence
//! `ESC ] 52 ; c ; <base64-payload> BEL` as "set the system clipboard
//! to the decoded payload". The terminal silently consumes the
//! sequence — nothing is printed — so it works even mid-TUI.
//!
//! This avoids pulling in a native clipboard dep (`arboard`,
//! `copypasta`) which would either link X11/Wayland on Linux or break
//! the static musl build. Terminals that don't support OSC 52 just
//! drop the sequence on the floor; the user gets nothing in the
//! clipboard but the app stays alive.

use std::io::{self, Write};

/// Emits an OSC 52 sequence to stdout that asks the host terminal to
/// store `text` in the system clipboard. Returns the underlying I/O
/// error if writing to stdout fails (rare — only when the TTY is closed).
pub fn copy(text: &str) -> io::Result<()> {
    let payload = b64_encode(text.as_bytes());
    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{payload}\x07")?;
    out.flush()
}

/// Standard MIME / RFC 4648 base64 encoder.
fn b64_encode(input: &[u8]) -> String {
    const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8) | (input[i + 2] as u32);
        out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPH[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(ALPH[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPH[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPH[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_known_vectors_match_rfc4648() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn b64_handles_high_bytes_and_padding() {
        assert_eq!(b64_encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(b64_encode(&[0x00, 0x00]), "AAA=");
        assert_eq!(b64_encode(&[0x00]), "AA==");
    }

    #[test]
    fn b64_round_trips_arbitrary_text() {
        let s = "Hello, 세상! 🌍";
        let enc = b64_encode(s.as_bytes());
        // The decoder lives in the host terminal; just sanity-check
        // length and alphabet.
        assert_eq!(enc.len() % 4, 0);
        assert!(enc
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }
}
