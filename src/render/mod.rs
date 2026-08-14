//! Chat renderer core: store nodes → cached rows → viewport slice → buffer.
//!
//! Pipeline (ticket 05 Q1/Q4/Q5/Q10/Q12):
//! 1. The store derives the chat-node tree from mux frames.
//! 2. [`row_cache::RowCache::sync`] reconciles the cache with the store's node
//!    list: new nodes render through the markdown pipeline, changed nodes are
//!    marked dirty, gone nodes drop their rows.
//! 3. [`row_cache::RowCache::render_dirty`] re-renders exactly the dirty nodes
//!    (streaming markdown re-parse per chunk, Q5 — bounded rows make the
//!    re-parse cheap; idle nodes stay cached).
//! 4. [`chat_view::ChatView`] draws only the visible window of the cached row
//!    array (virtualization, Q4).
//!
//! Resize (Q10): a width change drops all cached rows ([`RowCache::invalidate_all`])
//! and the next sync re-renders everything — rare, one-time cost.
//!
//! No terminal, no event loop: everything draws into a ratatui `Buffer`
//! (`TestBackend` in tests). The app shell comes in a later surface lane.

pub mod chat_view;
pub mod image;
pub mod markdown;
pub mod row_cache;

pub use chat_view::ChatView;
pub use image::{ImageCache, ImageProtocol, detect_protocol, picker_for};
pub use markdown::{render_markdown, render_node};
pub use row_cache::{CachedRow, RowCache};
