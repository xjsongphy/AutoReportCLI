//! Real terminal palette detection. Codex's build relies on a *forked*
//! crossterm that exposes `query_background_color` / `query_foreground_color`;
//! upstream crossterm 0.28 lacks these, so we implement the same OSC 10/11
//! color query ourselves (write the request, read the `rgb:` reply from the
//! controlling terminal with a bounded timeout) — the identical mechanism, not
//! a stub. Results are cached for the process lifetime.

use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct DefaultColors {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
}

struct Cache<T: Copy> {
    attempted: bool,
    value: Option<T>,
}

impl<T: Copy> Default for Cache<T> {
    fn default() -> Self {
        Self {
            attempted: false,
            value: None,
        }
    }
}

impl<T: Copy> Cache<T> {
    fn get_or_init_with(&mut self, mut init: impl FnMut() -> Option<T>) -> Option<T> {
        if !self.attempted {
            self.value = init();
            self.attempted = true;
        }
        self.value
    }
}

fn cache() -> &'static Mutex<Cache<DefaultColors>> {
    static CACHE: OnceLock<Mutex<Cache<DefaultColors>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

pub fn default_colors() -> Option<DefaultColors> {
    let mut c = cache().lock().ok()?;
    c.get_or_init_with(query_default_colors)
}

pub fn default_fg() -> Option<(u8, u8, u8)> {
    default_colors().map(|c| c.fg)
}

pub fn default_bg() -> Option<(u8, u8, u8)> {
    default_colors().map(|c| c.bg)
}

/// Query both default colors via OSC 10/11. Returns `None` if the terminal
/// does not reply within the timeout (e.g. piped output, unsupported term).
fn query_default_colors() -> Option<DefaultColors> {
    let fg = query_color(10)?;
    let bg = query_color(11)?;
    Some(DefaultColors { fg, bg })
}

/// Query one default color: `which` is `10` (foreground) or `11` (background).
fn query_color(which: u8) -> Option<(u8, u8, u8)> {
    // The reply read can block; do it on a thread with a hard deadline so a
    // non-responsive terminal can never hang the UI.
    let (tx, rx) = std::sync::mpsc::channel();
    let builder = std::thread::Builder::new()
        .name("osc-color-query".into());
    builder.spawn(move || {
        let res = (|| -> Option<(u8, u8, u8)> {
            // Talk to the controlling terminal directly so we don't disturb the
            // app's stdin/stdout buffers.
            let mut tty = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()?;
            // OSC Ps ; ? — request current color. ST = ESC backslash.
            let req = format!("\x1b]{};?\x1b\\", which);
            tty.write_all(req.as_bytes()).ok()?;
            tty.flush().ok()?;
            // Read until BEL (0x07) or ST (ESC \).
            let mut buf = Vec::with_capacity(64);
            let mut byte = [0u8; 1];
            for _ in 0..256 {
                match tty.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {
                        buf.push(byte[0]);
                        if byte[0] == 0x07 {
                            break;
                        }
                        if buf.len() >= 2 && buf[buf.len() - 2] == 0x1b && buf[buf.len() - 1] == b'\\'
                        {
                            break;
                        }
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            parse_color_reply(&buf)
        })();
        let _ = tx.send(res);
    })
    .ok()?;

    rx.recv_timeout(Duration::from_millis(120))
        .ok()
        .flatten()
}

/// Parse an OSC color reply of the form `ESC ] <ps> ; rgb:RRRR/GGGG/BBBB ST`.
fn parse_color_reply(buf: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(buf).ok()?;
    // Locate `rgb:` payload.
    let idx = s.find("rgb:")?;
    let rest = &s[idx + 4..];
    // Take up to the first terminator (ST/BEL/whitespace).
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '\x07' || c == '\\')
        .unwrap_or(rest.len());
    let payload = &rest[..end];
    let parts: Vec<&str> = payload.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parse_channel(parts[0])?,
        parse_channel(parts[1])?,
        parse_channel(parts[2])?,
    ))
}

/// Each channel may be 1–4 hex digits; normalize to 8-bit.
fn parse_channel(s: &str) -> Option<u8> {
    let s = s.trim();
    let val = u32::from_str_radix(s, 16).ok()?;
    let digits = s.len();
    // Scale full-range (all-F) to 255.
    let max = (1usize << (4 * digits)) - 1;
    if max == 0 {
        return None;
    }
    Some(((val * 255) / max as u32) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_4_digit_rgb_reply() {
        // ESC ] 11 ; rgb:0000/0000/0000 BEL
        let reply = b"\x1b]11;rgb:aaaa/bbbb/cccc\x07";
        assert_eq!(parse_color_reply(reply), Some((170, 187, 204)));
    }

    #[test]
    fn parses_2_digit_rgb_reply() {
        let reply = b"\x1b]11;rgb:aa/bb/cc\x07";
        assert_eq!(parse_color_reply(reply), Some((170, 187, 204)));
    }
}
