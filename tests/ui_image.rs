//! Image pipeline integration tests (PARITY.md Images row): inline
//! placeholder/rendering tiers and the full-screen viewer. Keyless —
//! `TestBackend` has no graphics protocol, so protocol paths degrade to the
//! placeholder; the halfblocks tier is buffer-native and IS exercised
//! directly. Protocol detection (`detect_protocol`) is env-based and unit
//! tested in `src/render/image.rs` (kitty/iTerm2/sixel/tmux/none tiers).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;

use dsh_tui::app::{Action, App, AppEvent, EventChannel, Focus};
use dsh_tui::i18n::Locale;
use dsh_tui::render::{ChatView, ImageCache, ImageProtocol, RowCache, picker_for};
use dsh_tui::theme::Theme;
use dsh_tui::ui::takeover::Mode;
use dsh_tui::wire::events::MuxFrame;
use dsh_tui::wire::session::{AttachmentId, SessionEvent, SessionId};

// ---------------------------------------------------------------------------
// fixture helpers
// ---------------------------------------------------------------------------

fn ev(seq: i64, r#type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        r#type: r#type.into(),
        seq,
        time: seq as f64,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn frame(session: &str, event: SessionEvent) -> MuxFrame {
    MuxFrame::SessionEvent {
        session_id: SessionId(session.into()),
        event,
        view: None,
    }
}

fn image_block(att: &str, name: &str) -> serde_json::Value {
    json!({
        "type": "image",
        "attachment": {
            "attachmentId": att,
            "mediaType": "image/png",
            "bytes": 45000,
            "width": 640,
            "height": 480,
            "name": name,
        },
    })
}

fn user_msg(id: &str, text: &str) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": [{"type": "text", "text": text}], "source": {"kind": "user"}})
}

fn image_msg(id: &str, blocks: Vec<serde_json::Value>) -> serde_json::Value {
    json!({"id": id, "role": "user", "content": blocks, "source": {"kind": "user"}})
}

/// A tool call whose result carries an image block (tool-output images).
fn tool_with_image(seq: i64, att: &str, name: &str) -> Vec<SessionEvent> {
    vec![
        ev(
            seq,
            "tool/call",
            json!({"turn": 1, "step": 1, "callId": "c1", "name": "screenshot", "arguments": "{}"}),
        ),
        ev(
            seq + 1,
            "tool/result",
            json!({
                "turn": 1, "step": 1,
                "message": {
                    "id": "r1", "role": "user",
                    "content": [{
                        "type": "tool-result",
                        "toolCallId": "c1",
                        "content": [image_block(att, name)],
                        "isError": false,
                    }],
                    "source": {"kind": "tool", "callId": "c1"},
                },
            }),
        ),
    ]
}

/// Three image blocks in display order: two in one user message, one in a
/// tool result.
fn three_image_events() -> Vec<SessionEvent> {
    let mut events = vec![ev(
        1,
        "user/message",
        image_msg(
            "m1",
            vec![
                image_block("att-1", "plot.png"),
                image_block("att-2", "chart.png"),
            ],
        ),
    )];
    events.extend(tool_with_image(2, "att-3", "shot.png"));
    events
}

fn app_with_events(events: Vec<SessionEvent>) -> App {
    let mut app = App::default();
    app.focus = Focus::Chat; // 'i' (open the viewer) is chat-bound; boot is Composer
    app.active_session = Some(SessionId("s1".into()));
    for event in events {
        app.store.ingest(frame("s1", event)).expect("ingest");
    }
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// Feed buffered events into a fresh channel and run the loop to completion
/// (the quit key breaks it).
async fn run_with(app: &mut App, term: &mut Terminal<TestBackend>, events: Vec<AppEvent>) {
    let mut channel = EventChannel::new();
    for event in events {
        channel.tx.send(event).expect("event channel");
    }
    app.run(term, &mut channel)
        .await
        .expect("run must not fail");
}

// ---------------------------------------------------------------------------
// inline rendering (placeholder tier + halfblocks pipeline)
// ---------------------------------------------------------------------------

#[test]
fn inline_placeholder_unchanged_without_bytes() {
    // No protocol, no cached bytes: the image block renders exactly the
    // `[image: name]` caption — one line, no filler, no segments.
    let app = app_with_events(vec![ev(
        1,
        "user/message",
        image_msg("m1", vec![image_block("att-1", "plot.png")]),
    )]);
    let mut cache = RowCache::new();
    cache.sync(
        &app.store,
        &SessionId("s1".into()),
        120,
        &Theme::default(),
        Locale::En,
        &ImageCache::default(),
        &std::collections::HashMap::new(),
    );
    let row = &cache.lines()[0];
    assert_eq!(row.lines.len(), 1, "caption only, no filler lines");
    assert!(row.images.is_empty(), "no segments without cached bytes");
    let text: String = row.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(text, "[image: plot.png]");
}

#[test]
fn inline_image_expands_and_draws_with_cached_bytes() {
    // The real pipeline, keyless: halfblocks are buffer-native, so a cached
    // image renders actual `▀` cells into the TestBackend.
    let app = app_with_events(vec![ev(
        1,
        "user/message",
        image_msg("m1", vec![image_block("att-1", "plot.png")]),
    )]);
    let mut images = ImageCache::default();
    let picker = picker_for(ImageProtocol::Halfblocks).expect("halfblocks picker");
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 32, |x, _| {
        if x < 32 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    }))
    .write_to(&mut png, image::ImageFormat::Png)
    .expect("encode png");
    images
        .insert(&picker, AttachmentId("att-1".into()), &png.into_inner())
        .expect("decode png");

    let mut cache = RowCache::new();
    cache.sync(
        &app.store,
        &SessionId("s1".into()),
        100,
        &Theme::default(),
        Locale::En,
        &images,
        &std::collections::HashMap::new(),
    );
    let row = &cache.lines()[0];
    assert_eq!(row.images.len(), 1, "one inline segment");
    let segment = &row.images[0];
    assert_eq!(segment.attachment_id, AttachmentId("att-1".into()));
    // 64x32px at the assumed 10x20 cell: ceil(32/20) = 2 filler rows.
    assert_eq!(segment.rows, 2);
    assert_eq!(segment.line_index, 1, "filler follows the caption");
    assert_eq!(row.lines.len(), 1 + 2, "caption + filler lines");
    let caption: String = row.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(caption, "[image: plot.png]", "caption stays the label");

    // Draw: the segment paints halfblock cells over the filler lines.
    let backend = TestBackend::new(100, 10);
    let mut terminal = Terminal::new(backend).expect("backend");
    terminal
        .draw(|f| {
            f.render_widget(
                ChatView {
                    store: &app.store,
                    session_id: &SessionId("s1".into()),
                    offset: 0,
                    row_cache: &mut cache,
                    images: &mut images,
                },
                f.area(),
            );
        })
        .expect("draw");
    let view = format!("{}", terminal.backend());
    assert!(view.contains("[image: plot.png]"), "caption drawn: {view}");
    assert!(view.contains('▀'), "halfblock cells drawn: {view}");
}

// ---------------------------------------------------------------------------
// viewer: open/close, cycling, fit toggle, key swallow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn viewer_opens_draws_placeholder_and_closes() {
    for (width, height) in [(120, 30), (60, 15)] {
        let mut app = app_with_events(vec![ev(
            1,
            "user/message",
            image_msg("m1", vec![image_block("att-1", "plot.png")]),
        )]);
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        run_with(
            &mut app,
            &mut term,
            vec![
                AppEvent::Key(key(KeyCode::Char('i'))),
                AppEvent::Key(key(KeyCode::Esc)),
                AppEvent::Key(ctrl(KeyCode::Char('q'))),
            ],
        )
        .await;
        let view = format!("{}", term.backend());
        // (The Esc closed the viewer before this snapshot — reopen state is
        // asserted below; here the placeholder path already drew without a
        // panic at both sizes.)
        assert!(matches!(app.mode, Mode::Chat), "Esc closed the viewer");
        let _ = view;
    }

    // Reopen and assert the placeholder content while the viewer is up.
    let mut app = app_with_events(vec![ev(
        1,
        "user/message",
        image_msg("m1", vec![image_block("att-1", "plot.png")]),
    )]);
    let backend = TestBackend::new(120, 30);
    let mut term = Terminal::new(backend).unwrap();
    let mut channel = EventChannel::new();
    channel
        .tx
        .send(AppEvent::Key(key(KeyCode::Char('i'))))
        .expect("v");
    // Draw happens on the key event; quit from the viewer via Ctrl+Q.
    channel
        .tx
        .send(AppEvent::Key(ctrl(KeyCode::Char('q'))))
        .expect("ctrl+q");
    app.run(&mut term, &mut channel).await.expect("run");
    let view = format!("{}", term.backend());
    assert!(view.contains("image 1/1"), "viewer title: {view}");
    assert!(view.contains("plot.png"), "image name: {view}");
    assert!(view.contains("640×480"), "attachment meta: {view}");
    assert!(view.contains("image/png"), "media type: {view}");
    assert!(view.contains("45 KB"), "byte size: {view}");
    assert!(
        view.contains("no graphics protocol"),
        "placeholder notice: {view}"
    );
    assert!(view.contains("next"), "hint line: {view}");
}

#[test]
fn viewer_cycles_next_prev_with_wraparound() {
    let mut app = app_with_events(three_image_events());
    assert_eq!(
        app.handle_key(key(KeyCode::Char('i'))),
        Some(Action::None),
        "v opens the viewer"
    );
    let Mode::Image(viewer) = &app.mode else {
        panic!("viewer mode");
    };
    assert_eq!(viewer.images.len(), 3, "user + tool-result images");
    assert_eq!(viewer.index, 0, "starts at the first image (empty cache)");
    assert_eq!(viewer.current().name.as_deref(), Some("plot.png"));

    app.handle_key(key(KeyCode::Char('n')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert_eq!(viewer.index, 1);
    assert_eq!(viewer.current().name.as_deref(), Some("chart.png"));
    app.handle_key(key(KeyCode::Char('n')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert_eq!(viewer.index, 2);
    assert_eq!(
        viewer.current().name.as_deref(),
        Some("shot.png"),
        "tool-result image is in the cycle"
    );
    app.handle_key(key(KeyCode::Char('n')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert_eq!(viewer.index, 0, "n wraps past the end");
    app.handle_key(key(KeyCode::Char('p')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert_eq!(viewer.index, 2, "p wraps before the first");
}

#[test]
fn viewer_t_toggles_fit_actual() {
    let mut app = app_with_events(three_image_events());
    app.handle_key(key(KeyCode::Char('i')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert!(viewer.fit, "opens in fit mode");
    app.handle_key(key(KeyCode::Char('t')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert!(!viewer.fit, "t → actual size");
    app.handle_key(key(KeyCode::Char('t')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    assert!(viewer.fit, "t → fit again");
}

#[test]
fn viewer_swallows_chat_keys_and_ctrl_q_quits() {
    let mut app = app_with_events(three_image_events());
    app.handle_key(key(KeyCode::Char('i')));
    assert!(matches!(app.mode, Mode::Image(_)));
    // Chat keys are inert while the viewer is open.
    assert_eq!(app.handle_key(key(KeyCode::Char('j'))), Some(Action::None));
    assert_eq!(app.view.offset, 0, "no scrolling in the viewer");
    assert_eq!(app.handle_key(key(KeyCode::Char('x'))), Some(Action::None));
    assert_eq!(app.handle_key(key(KeyCode::Char('g'))), Some(Action::None));
    assert!(matches!(app.mode, Mode::Image(_)), "still open");
    // q closes the viewer (does NOT quit).
    assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Some(Action::None));
    assert!(matches!(app.mode, Mode::Chat), "q closed the viewer");
    // Reopen; Ctrl+Q still quits from the viewer (takeover exception).
    app.handle_key(key(KeyCode::Char('i')));
    assert!(matches!(app.mode, Mode::Image(_)));
    assert_eq!(
        app.handle_key(ctrl(KeyCode::Char('q'))),
        Some(Action::Quit),
        "Ctrl+Q quits in every mode"
    );
}

#[test]
fn viewer_hint_without_images() {
    let mut app = app_with_events(vec![ev(1, "user/message", user_msg("m1", "just text"))]);
    assert_eq!(
        app.handle_key(key(KeyCode::Char('i'))),
        Some(Action::None),
        "v with no images is a consumed no-op"
    );
    assert!(matches!(app.mode, Mode::Chat), "no mode change");
    assert_eq!(app.hint.as_deref(), Some("no images in this session"));
}

#[test]
fn image_block_without_bytes_is_a_placeholder_not_a_panic() {
    // The store's image block carries only a durable ref (no bytes); both
    // the inline row and the viewer degrade to the placeholder. Covered
    // inline by `inline_placeholder_unchanged_without_bytes`; here the
    // viewer's placeholder path draws at a small size without a panic.
    let mut app = app_with_events(vec![ev(
        1,
        "user/message",
        image_msg("m1", vec![image_block("att-1", "wide.png")]),
    )]);
    app.handle_key(key(KeyCode::Char('i')));
    let Mode::Image(viewer) = &app.mode else {
        panic!()
    };
    let mut images = ImageCache::default(); // no bytes for att-1
    let backend = TestBackend::new(60, 15);
    let mut term = Terminal::new(backend).unwrap();
    let theme = app.theme.clone();
    term.draw(|f| {
        f.render_widget(
            dsh_tui::ui::image_viewer::ImageViewerView {
                viewer,
                images: &mut images,
                protocol: ImageProtocol::None,
                notice: None,
                theme: &theme,
                locale: Locale::En,
            },
            f.area(),
        );
    })
    .expect("draw");
    let view = format!("{}", term.backend());
    assert!(view.contains("[image: wide.png]"), "placeholder: {view}");
    assert!(view.contains("fit/actual"), "hints still live: {view}");
}
