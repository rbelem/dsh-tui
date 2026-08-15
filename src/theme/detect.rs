//! Light/dark mode detection (issue #1): OSC 11 terminal background query,
//! then environment signals, then desktop settings. Layered, best-effort,
//! never blocking longer than ~150ms; all `None` → the caller keeps its
//! fallback default.
//!
//! The OSC 11 layer is interactive-safe: it opens `/dev/tty` non-blocking
//! and polls with a deadline, so it NEVER leaves a reader thread blocking
//! the terminal (which would steal keystrokes from crossterm's input
//! reader) and never reads a frame from the caller's stack. It self-skips
//! when stdin is not a terminal (tests, pipes, CI). Only an `ESC ]`-
//! prefixed reply is consumed; any other first input aborts the query —
//! at most one stray byte is lost, and the app's startup paint makes that
//! harmless.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The detected color scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Light,
    Dark,
}

/// Layered detection: OSC 11 terminal background first, then environment
/// signals, then desktop settings. `None` when every layer is inconclusive
/// (the caller keeps its default).
pub fn detect_color_mode() -> Option<ColorMode> {
    osc11_background()
        .map(classify)
        .or_else(env_signal)
        .or_else(desktop_signal)
}

/// Parse an OSC 11 reply into `(r, g, b)`. Accepts `rgb:RRRR/GGGG/BBBB`
/// (4 hex digits per channel — the high byte, i.e. the first two digits,
/// is taken), `rgb:RR/GG/BB`, and `#RRGGBB`. The `ESC ] 11 ;` prefix and
/// trailing BEL (0x07) / ST (`ESC \`) are ignored — the `rgb:` or `#` part
/// is found anywhere in the bytes. Garbage → `None`.
pub fn parse_osc11_response(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    let text = std::str::from_utf8(bytes).ok()?;
    // A 2- or 4-hex-digit channel value (hex digits only, so anything after
    // the value — the ST terminator, for instance — is naturally ignored).
    let channel = |value: &str| -> Option<u8> {
        let digits: String = value
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        match digits.len() {
            2 => u8::from_str_radix(&digits, 16).ok(),
            4 => u8::from_str_radix(&digits[..2], 16).ok(),
            _ => None,
        }
    };
    if let Some(start) = text.find("rgb:") {
        let mut parts = text[start + 4..].split('/');
        Some((
            channel(parts.next()?)?,
            channel(parts.next()?)?,
            channel(parts.next()?)?,
        ))
    } else if let Some(start) = text.find('#') {
        let hex = &text[start + 1..(start + 7).min(text.len())];
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some((
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
        ))
    } else {
        None
    }
}

/// Classify an RGB color by ITU-R BT.709 relative luminance
/// (`0.2126*R + 0.7152*G + 0.0722*B`, channels normalized to 0–1):
/// `>= 0.5` → `Light`, else `Dark`.
pub fn classify((r, g, b): (u8, u8, u8)) -> ColorMode {
    let luminance =
        0.2126 * (r as f64 / 255.0) + 0.7152 * (g as f64 / 255.0) + 0.0722 * (b as f64 / 255.0);
    if luminance >= 0.5 {
        ColorMode::Light
    } else {
        ColorMode::Dark
    }
}

/// Write the OSC 11 query (`ESC ] 11 ; ?` + BEL) and read the reply within
/// `timeout`. A detached reader thread accumulates chunks until BEL, ST
/// (`ESC \`), or EOF/error, then sends them over a channel; the caller
/// waits at most `timeout`. On timeout → `None`. A reader that never
/// answers leaves the reader thread blocked — accepted, the process exits
/// anyway.
///
/// Callers must pass a reader that terminates quickly or outlives the
/// thread: the thread holds the borrow beyond this call, so a reader whose
/// lifetime ends here must not be one the thread could still be reading.
/// The interactive [`osc11_background`] path does not use this function —
/// it uses a self-terminating non-blocking poll instead (see the module
/// docs), so this exists for non-tty readers and tests.
pub fn query_osc11(
    write: &mut impl Write,
    read: &mut (impl Read + Send),
    timeout: Duration,
) -> Option<(u8, u8, u8)> {
    write.write_all(b"\x1b]11;?\x07").ok()?;
    write.flush().ok()?;

    // The reader thread must not be joined: a silent reader would block
    // the caller past `timeout`, so it is detached and keeps using `read`
    // for an unbounded lifetime. The borrow crosses as a raw pointer —
    // SAFETY: `read` is `Send`, the pointer is the only remaining access
    // path (the caller never touches `read` again), and the callers this
    // function is built for (tests with instantly-terminating readers)
    // never drop the reader while the thread may still use it.
    let read: &mut (dyn Read + Send + '_) = read;
    let read: &mut (dyn Read + Send + 'static) = unsafe {
        std::mem::transmute::<&mut (dyn Read + Send + '_), &mut (dyn Read + Send + 'static)>(read)
    };
    let mut read = SendPtr(read as *mut (dyn Read + Send));
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let read: &mut (dyn Read + Send) = unsafe { read.deref_mut() };
        let mut reply = Vec::new();
        let mut chunk = [0u8; 128];
        loop {
            match read.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(reply);
                    break;
                }
                Ok(n) => {
                    reply.extend_from_slice(&chunk[..n]);
                    if reply.contains(&0x07) || reply.ends_with(&[0x1b, b'\\']) {
                        let _ = tx.send(reply);
                        break;
                    }
                }
            }
        }
    });

    let reply = rx.recv_timeout(timeout).ok()?;
    parse_osc11_response(&reply)
}

/// A raw pointer moved into the reader thread (`*mut T` is not `Send`).
/// The pointee is `dyn Read + Send`, so crossing the thread boundary is
/// sound; the caller's borrow is not used again after `query_osc11`.
struct SendPtr<T: ?Sized>(*mut T);

impl<T: ?Sized> SendPtr<T> {
    /// Reborrow the pointee. SAFETY: this `SendPtr` is the only remaining
    /// access path to the reader.
    unsafe fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0 }
    }
}

// SAFETY: `query_osc11` only builds `SendPtr` for `dyn Read + Send`, and
// the raw pointer is the sole remaining access path to the reader.
unsafe impl<T: ?Sized> Send for SendPtr<T> {}

/// OSC 11 on the interactive `/dev/tty` — a self-terminating poll instead
/// of a blocking reader thread. The tty is opened non-blocking, so a read
/// with no data returns `WouldBlock` and the loop sleeps until the
/// deadline; the thread never blocks in the kernel, never competes with
/// crossterm for keystrokes, and always exits by `timeout` + one poll
/// beat. Only an `ESC ]`-prefixed reply is consumed: any other first
/// input aborts the query, so an early keystroke loses at most that one
/// byte (the app's startup paint makes it harmless).
fn query_osc11_poll(
    write: &mut impl Write,
    read: &mut impl Read,
    timeout: Duration,
) -> Option<(u8, u8, u8)> {
    use std::io::ErrorKind;
    write.write_all(b"\x1b]11;?\x07").ok()?;
    write.flush().ok()?;
    let deadline = Instant::now() + timeout;
    let mut reply = Vec::new();
    let mut chunk = [0u8; 1];
    loop {
        match read.read(&mut chunk) {
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Ok(0) | Err(_) => return None, // EOF or error
            Ok(_) => {}
        }
        let byte = chunk[0];
        match reply.len() {
            // Only an OSC reply (`ESC ] ...`) is consumed.
            0 if byte == 0x1b => reply.push(byte),
            0 => return None, // stray input (e.g. an early key): abort
            1 if byte == b']' => reply.push(byte),
            1 => return None, // ESC + non-`]`: not an OSC reply
            _ => reply.push(byte),
        }
        if reply.contains(&0x07) || reply.ends_with(&[0x1b, b'\\']) {
            return parse_osc11_response(&reply);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Layer 1: query the terminal background via `/dev/tty`. Skips itself
/// when stdin is not a terminal (tests, pipes, CI — the ~150ms window
/// never happens there) or when `/dev/tty` cannot be opened. Any error →
/// `None`.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
fn osc11_background() -> Option<(u8, u8, u8)> {
    use std::io::IsTerminal;
    // O_NONBLOCK for the poll above (no libc dependency: the values are
    // stable ABI constants per platform).
    #[cfg(target_os = "linux")]
    const O_NONBLOCK: i32 = 0o4000;
    #[cfg(not(target_os = "linux"))]
    const O_NONBLOCK: i32 = 0x4;

    if !std::io::stdin().is_terminal() {
        return None;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(O_NONBLOCK)
        .open("/dev/tty")
        .ok()?;
    if !file.is_terminal() {
        return None;
    }
    let mut writer = &file;
    let mut reader = &file;
    query_osc11_poll(&mut writer, &mut reader, Duration::from_millis(150))
}

/// Layer 1 on unix platforms without a known `O_NONBLOCK` constant: the
/// blocking-thread query, with the reader heap-stable so the detached
/// thread can never touch freed memory.
#[cfg(unix)]
#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
fn osc11_background() -> Option<(u8, u8, u8)> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    if !file.is_terminal() {
        return None;
    }
    // Heap-stable reader: the detached reader thread may outlive this
    // frame (a terminal that never answers), so nothing the thread
    // touches may live on the stack. Two bounded leaks per query — the
    // process exits anyway.
    let file: &'static std::fs::File = Box::leak(Box::new(file));
    let reader: &'static mut (dyn Read + Send) =
        Box::leak(Box::new(file as &'static std::fs::File));
    let mut writer = file;
    let mut reader = &mut *reader;
    query_osc11(&mut writer, &mut reader, Duration::from_millis(150))
}

/// Layer 1 on non-unix platforms: there is no `/dev/tty`.
#[cfg(not(unix))]
fn osc11_background() -> Option<(u8, u8, u8)> {
    None
}

/// Layer 2: environment signals — `GTK_THEME` (`:dark` / `:light` suffix)
/// then `COLORFGBG` (last `;`-separated field: `< 128` → dark, else light;
/// non-numeric → `None`).
fn env_signal() -> Option<ColorMode> {
    if let Ok(theme) = std::env::var("GTK_THEME") {
        if theme.contains(":dark") {
            return Some(ColorMode::Dark);
        }
        if theme.contains(":light") {
            return Some(ColorMode::Light);
        }
    }
    if let Ok(colorfgbg) = std::env::var("COLORFGBG")
        && let Some(last) = colorfgbg.rsplit(';').next()
        && let Ok(value) = last.parse::<u8>()
    {
        return Some(if value < 128 {
            ColorMode::Dark
        } else {
            ColorMode::Light
        });
    }
    None
}

/// Layer 3: desktop settings. macOS: `defaults read -g AppleInterfaceStyle`
/// (`Dark` → dark). Linux: `gsettings get org.gnome.desktop.interface
/// gtk-theme` (`:dark` / `:light` suffix). Windows or any failure → `None`.
#[cfg(target_os = "macos")]
fn desktop_signal() -> Option<ColorMode> {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok()?;
    if String::from_utf8_lossy(&output.stdout).trim() == "Dark" {
        Some(ColorMode::Dark)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn desktop_signal() -> Option<ColorMode> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
        .output()
        .ok()?;
    let theme = String::from_utf8_lossy(&output.stdout);
    if theme.contains(":dark") {
        Some(ColorMode::Dark)
    } else if theme.contains(":light") {
        Some(ColorMode::Light)
    } else {
        None
    }
}

#[cfg(target_os = "windows")]
fn desktop_signal() -> Option<ColorMode> {
    None
}
