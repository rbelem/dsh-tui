//! Image pipeline (PARITY.md Images row): protocol detection with the
//! fallback tier Kitty → iTerm2 → Sixel → Halfblocks → `[image]` placeholder,
//! plus the decoded-image cache that inline rows and the full-screen viewer
//! draw from.
//!
//! Detection basis: ENVIRONMENT VARIABLES ONLY, resolved once at startup
//! ([`App::init_images`](crate::app::App) calls [`detect_protocol`] and builds
//! the ratatui-image `Picker` then). v1 deliberately does NOT use the crate's
//! `Picker::from_query_stdio()` terminal query: it does stdio IO with a
//! multi-second timeout and must run between alternate-screen entry and the
//! event read — wiring it is a TODO (real-protocol smoke lane). The env tier:
//!
//! - tmux first: escape passthrough needs terminal-specific handling the
//!   crate only does via its query path, so v1 conservatively degrades to
//!   Halfblocks inside tmux/screen.
//! - Kitty: kitty/ghostty terminals (`TERM`, `TERM_PROGRAM`, or the
//!   terminals' own env markers).
//! - iTerm2: `TERM_PROGRAM=iTerm.app` / `ITERM_SESSION_ID`, and WezTerm —
//!   mirroring the crate, which blacklists Kitty+Sixel on WezTerm (broken
//!   placeholder/buggy sixel) and lands on its iTerm2 support.
//! - Sixel: `TERM` naming a sixel terminal (`*-sixel`, foot, mlterm, yaft).
//! - Halfblocks: the universal fallback — buffer-native colored `▀` cells,
//!   works anywhere unicode renders (including `TestBackend`).
//! - None: no `TERM` or `TERM=dumb` — the `[image]` placeholder tier.
//!
//! The byte cache ([`ImageCache`]) is keyed by `AttachmentId`. v1 has no
//! fetch path: image content blocks carry only a durable `ImageAttachmentRef`
//! (id/media-type/dimensions/name), and the `session.attachment` RPC that
//! returns the base64 payload is NOT wired (TODO: attachment fetch lane).
//! Until then every image renders its placeholder; the inline/viewer paths
//! below light up unchanged once inserts land.

use std::collections::HashMap;

use image::DynamicImage;
use ratatui_image::FontSize;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::wire::session::AttachmentId;

/// Assumed terminal cell size in pixels when no terminal query ran (the
/// crate's own default-picker guess — roughly 1:2, exact value irrelevant
/// for halfblocks and only used for aspect math otherwise).
pub const ASSUMED_FONT_SIZE: FontSize = FontSize::new(10, 20);

/// Cap on inline image height in chat rows (a tall image never eats the
/// whole viewport; the full-screen viewer is the large look).
pub const MAX_INLINE_ROWS: u16 = 12;

/// The negotiated graphics protocol tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageProtocol {
    Kitty,
    ITerm2,
    Sixel,
    /// Universal buffer-native fallback (colored half-block cells).
    Halfblocks,
    /// No graphics at all (`TERM` unset/dumb): the `[image]` placeholder.
    #[default]
    None,
}

/// Detect the protocol tier from the process environment (see module docs;
/// resolved once at startup by the app shell, never in `App::default` —
/// tests stay terminal-agnostic).
pub fn detect_protocol() -> ImageProtocol {
    detect_protocol_with(|key| std::env::var(key).ok())
}

/// The pure core of [`detect_protocol`], env lookup injected for tests.
fn detect_protocol_with(env: impl Fn(&str) -> Option<String>) -> ImageProtocol {
    let set = |key: &str| env(key).is_some_and(|value| !value.is_empty());
    let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
    let term_program = env("TERM_PROGRAM").unwrap_or_default().to_ascii_lowercase();

    // tmux/screen: no passthrough assumptions in v1 (see module docs).
    if set("TMUX") || term.starts_with("screen") || term.starts_with("tmux") {
        return ImageProtocol::Halfblocks;
    }
    if term.contains("kitty")
        || term_program == "kitty"
        || set("KITTY_WINDOW_ID")
        || term.contains("ghostty")
        || term_program == "ghostty"
        || set("GHOSTTY_RESOURCES_DIR")
    {
        return ImageProtocol::Kitty;
    }
    if term_program == "iterm.app"
        || set("ITERM_SESSION_ID")
        || term_program == "wezterm"
        || set("WEZTERM_EXECUTABLE")
    {
        return ImageProtocol::ITerm2;
    }
    if term.contains("sixel")
        || term.starts_with("foot")
        || term.contains("mlterm")
        || term.contains("yaft")
    {
        return ImageProtocol::Sixel;
    }
    if term.is_empty() || term == "dumb" {
        return ImageProtocol::None;
    }
    ImageProtocol::Halfblocks
}

/// Build the ratatui-image picker for a detected protocol (env detection
/// supplies no font size, so the picker runs on [`ASSUMED_FONT_SIZE`]).
/// `None` protocol → no picker.
pub fn picker_for(protocol: ImageProtocol) -> Option<Picker> {
    let protocol_type = match protocol {
        ImageProtocol::Kitty => ratatui_image::picker::ProtocolType::Kitty,
        ImageProtocol::ITerm2 => ratatui_image::picker::ProtocolType::Iterm2,
        ImageProtocol::Sixel => ratatui_image::picker::ProtocolType::Sixel,
        ImageProtocol::Halfblocks => ratatui_image::picker::ProtocolType::Halfblocks,
        ImageProtocol::None => return None,
    };
    // `halfblocks()` is the non-deprecated constructor with the same
    // (10, 20) assumed font size; the protocol type is forced right after.
    let mut picker = Picker::halfblocks();
    picker.set_protocol_type(protocol_type);
    Some(picker)
}

/// One decoded image with its resize protocol, ready to draw. Static once
/// loaded — the cache never re-decodes.
pub struct LoadedImage {
    pub source: DynamicImage,
    pub protocol: StatefulProtocol,
}

/// One inline image segment inside a node's rendered lines (row-cache
/// metadata): the image widget draws over `rows` blank filler lines starting
/// at `line_index` (post-wrap; the caption line sits right above).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRow {
    pub line_index: usize,
    pub attachment_id: AttachmentId,
    pub rows: u16,
}

/// Attachment-id → decoded image. Empty in v1 (no fetch path — module docs).
#[derive(Default)]
pub struct ImageCache {
    map: HashMap<AttachmentId, LoadedImage>,
}

impl ImageCache {
    /// The loaded image for an attachment, when decoded and cached.
    pub fn get(&self, id: &AttachmentId) -> Option<&LoadedImage> {
        self.map.get(id)
    }

    /// Mutable access (stateful protocols draw through `&mut`).
    pub fn get_mut(&mut self, id: &AttachmentId) -> Option<&mut LoadedImage> {
        self.map.get_mut(id)
    }

    /// Decode `bytes` (png/jpeg/webp/gif — the wire's media types) and cache
    /// the image with a resize protocol built from `picker`.
    ///
    /// TODO(attachment lane): the only producer is the future
    /// `session.attachment` fetch (base64 payload → these bytes).
    pub fn insert(
        &mut self,
        picker: &Picker,
        id: AttachmentId,
        bytes: &[u8],
    ) -> Result<(), image::ImageError> {
        let source = image::load_from_memory(bytes)?;
        let protocol = picker.new_resize_protocol(source.clone());
        self.map.insert(id, LoadedImage { source, protocol });
        Ok(())
    }

    /// Rows an inline rendering of `image` occupies at `width` columns:
    /// aspect-preserving fit to the row width, capped at [`MAX_INLINE_ROWS`].
    pub fn inline_rows(image: &DynamicImage, width: u16) -> u16 {
        if width == 0 {
            return 1;
        }
        let size = ratatui_image::Resize::Fit(None).size_for(
            image,
            ASSUMED_FONT_SIZE,
            ratatui::layout::Size {
                width,
                height: u16::MAX,
            },
        );
        size.height.clamp(1, MAX_INLINE_ROWS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)]) -> ImageProtocol {
        detect_protocol_with(|key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn detection_tier() {
        assert_eq!(
            detect(&[("TERM", "xterm-kitty")]),
            ImageProtocol::Kitty,
            "kitty TERM"
        );
        assert_eq!(
            detect(&[("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-256color")]),
            ImageProtocol::Kitty,
            "ghostty"
        );
        assert_eq!(
            detect(&[("TERM_PROGRAM", "iTerm.app"), ("TERM", "xterm-256color")]),
            ImageProtocol::ITerm2,
            "iTerm2"
        );
        assert_eq!(
            detect(&[
                ("WEZTERM_EXECUTABLE", "/usr/bin/wezterm"),
                ("TERM", "xterm-256color")
            ]),
            ImageProtocol::ITerm2,
            "wezterm lands on iterm2 (crate parity)"
        );
        assert_eq!(
            detect(&[("TERM", "foot")]),
            ImageProtocol::Sixel,
            "foot is sixel"
        );
        assert_eq!(
            detect(&[("TERM", "xterm-256color")]),
            ImageProtocol::Halfblocks,
            "unknown truecolor-ish terminal falls to halfblocks"
        );
        assert_eq!(detect(&[]), ImageProtocol::None, "no TERM");
        assert_eq!(detect(&[("TERM", "dumb")]), ImageProtocol::None, "dumb");
        assert_eq!(
            detect(&[
                ("TMUX", "/tmp/tmux-1000/default,1,0"),
                ("TERM", "xterm-kitty"),
                ("KITTY_WINDOW_ID", "1"),
            ]),
            ImageProtocol::Halfblocks,
            "tmux wins over leaked kitty markers (conservative v1)"
        );
    }

    #[test]
    fn picker_only_for_real_protocols() {
        assert!(picker_for(ImageProtocol::None).is_none());
        for protocol in [
            ImageProtocol::Kitty,
            ImageProtocol::ITerm2,
            ImageProtocol::Sixel,
            ImageProtocol::Halfblocks,
        ] {
            assert!(picker_for(protocol).is_some());
        }
    }

    #[test]
    fn inline_rows_respect_aspect_and_cap() {
        let wide = DynamicImage::new_rgb8(2000, 500);
        // 80 cols × 10px = 800px wide; 4:1 → 200px tall; 200/20px rows = 10.
        assert_eq!(ImageCache::inline_rows(&wide, 80), 10);
        let tall = DynamicImage::new_rgb8(100, 5000);
        assert_eq!(
            ImageCache::inline_rows(&tall, 80),
            MAX_INLINE_ROWS,
            "tall images cap at MAX_INLINE_ROWS"
        );
        assert_eq!(
            ImageCache::inline_rows(&wide, 0),
            1,
            "zero width degenerates"
        );
    }
}
