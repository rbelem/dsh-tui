//! App events and the production event bridges (Q3).
//!
//! One channel carries everything into the single main loop:
//! - keys/resizes from a crossterm reader task (blocking reads on the tokio
//!   blocking pool);
//! - mux frames drained from the wire client's subscriber;
//! - a 16ms `Tick` (the run loop also selects on its own interval; the
//!   channel variant exists so tests can inject ticks deterministically).

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use crate::client::{ClientError, DownlinkFrame};
use crate::wire::approvals::ApprovalRequestId;
use crate::wire::events::{HostFrame, MuxFrame};
use crate::wire::rpc::{RpcId, RpcReceipt};
use crate::wire::session::{
    AttachmentId, SessionAttachmentValue, SessionCancelValue, SessionCreateValue, SessionForkValue,
    SessionHistoryValue, SessionId, SessionModelsValue, SessionPromptValue, SessionRenameValue,
    SessionSearchValue, SessionUpdateQueueValue,
};
use crate::wire::settings::{SettingsDescribeValue, SettingsWriteValue};
use crate::wire::skills::SkillListValue;
use crate::wire::workspace::WorkspaceArchiveSessionValue;

/// One event for the main loop.
#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    /// A crossterm mouse event (capture is enabled at terminal setup, #12).
    Mouse(crossterm::event::MouseEvent),
    /// Bracketed-paste payload (a terminal-mediated paste; the composer
    /// inserts it when focused).
    Paste(String),
    Frame(MuxFrame),
    /// An answerable frame (`approval/requested`, `question/requested`) with
    /// its envelope rpcId — the echo target for the answering ClientResponse
    /// ("rpcId echoed, never minted anew", rpc.ts:178).
    Answerable {
        rpc_id: RpcId,
        frame: MuxFrame,
    },
    /// The spawned answer task finished: correlate back to the open takeover
    /// via the tag.
    AnswerDone {
        tag: AnswerTag,
        result: Result<RpcReceipt, ClientError>,
    },
    /// The spawned `session.prompt` task finished.
    PromptDone {
        result: Result<SessionPromptValue, ClientError>,
    },
    /// The spawned `session.cancel` task finished (Q15).
    CancelDone {
        result: Result<SessionCancelValue, ClientError>,
    },
    /// A lazy `session.attachment` fetch finished (the image-cache producer
    /// lane): the base64 payload lands in `ImageCache` on success.
    AttachmentDone {
        attachment_id: AttachmentId,
        result: Result<SessionAttachmentValue, ClientError>,
    },
    /// A history page for a switched-to session arrived (Q9); the app folds
    /// it only when the session is still active (stale guard).
    HistoryLoaded {
        session_id: SessionId,
        result: Result<SessionHistoryValue, ClientError>,
    },
    /// A spawned `session.models` fetch finished (#43); the app caches the
    /// selection only when the session is still active (stale guard).
    ModelsLoaded {
        session_id: SessionId,
        result: Result<SessionModelsValue, ClientError>,
    },
    /// A spawned `session.updateQueue` action finished.
    QueueActionDone {
        kind: QueueActionKind,
        result: Result<SessionUpdateQueueValue, ClientError>,
    },
    /// A spawned sidebar `session.rename` finished (`r`).
    RenameDone {
        session_id: SessionId,
        result: Result<SessionRenameValue, ClientError>,
    },
    /// A spawned sidebar `session.fork` finished (`f`).
    ForkDone {
        result: Result<SessionForkValue, ClientError>,
    },
    /// A spawned new-session picker `session.create` finished (`n`).
    SessionCreateDone {
        result: Result<SessionCreateValue, ClientError>,
    },
    /// A spawned sidebar `workspace.archiveSession` finished (`a`).
    ArchiveDone {
        session_id: SessionId,
        result: Result<WorkspaceArchiveSessionValue, ClientError>,
    },
    /// A spawned sidebar `workspace.create` finished (6g: the Add button's
    /// path editor).
    WorkspaceCreateDone {
        result: Result<crate::wire::workspace::WorkspaceCreateValue, ClientError>,
    },
    /// A spawned sidebar-search `session.search` finished (`/`). `query`
    /// echoes the POSTed text so a stale result (the buffer moved on while
    /// the POST was in flight) is detected and re-searched.
    SessionSearchDone {
        query: String,
        result: Result<SessionSearchValue, ClientError>,
    },
    /// The `@` catalog fetch (`skill.list`) finished.
    CatalogLoaded {
        result: Result<SkillListValue, ClientError>,
    },
    /// The spawned `settings.describe` task finished (opening the settings
    /// view, or the conflict refresh). The app folds it only while the
    /// settings view is open.
    SettingsDescribeDone {
        result: Result<SettingsDescribeValue, ClientError>,
    },
    /// The spawned `settings.update` task finished; `ns` correlates the
    /// result back to the form that spawned it.
    SettingsSaveDone {
        ns: String,
        result: Result<SettingsWriteValue, ClientError>,
    },
    /// A host-stream frame (session list liveness). Host frames are pure
    /// pushes — the downlink rpcId is ignored.
    HostFrame(HostFrame),
    Resize(u16, u16),
    Tick,
}

/// Which queue action a finished task applied (the success toast echoes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueActionKind {
    Remove,
    Steer,
    Edit,
}

/// Correlates a finished answer task back to the takeover that spawned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerTag {
    Approval {
        approval_id: ApprovalRequestId,
        /// The outcome the user chose — the success toast echoes it
        /// ("allowed once" / "rejected").
        outcome: crate::wire::approvals::ApprovalResponseOutcome,
    },
    /// The question frame's envelope rpcId (the echo target).
    Question(RpcId),
}

/// The app event channel: the run loop owns the `rx` end; spawned
/// back-channel tasks (answers, prompts) hold `tx` clones.
pub struct EventChannel {
    pub tx: mpsc::UnboundedSender<AppEvent>,
    pub rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl EventChannel {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        EventChannel { tx, rx }
    }
}

impl Default for EventChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a mux frame is answerable (its envelope rpcId is the respond echo
/// target). Only approval/question requested frames — `session/event` etc.
/// are pure pushes.
pub fn is_answerable(frame: &MuxFrame) -> bool {
    matches!(
        frame,
        MuxFrame::ApprovalRequested { .. } | MuxFrame::QuestionRequested { .. }
    )
}

/// Swallows a terminal's OSC 11 reply when it leaks into the key stream.
///
/// The startup background query (`theme::detect::osc11_background`) polls
/// `/dev/tty` for ~150ms; a terminal that answers later (tmux, SSH, slow
/// emulators) leaves the reply — `ESC ] 11 ; rgb:RRRR/GGGG/BBBB ESC \` —
/// pending in the input queue. By the time the input bridge starts, that
/// queue is drained into crossterm, which tokenizes the reply as keystrokes:
/// Alt+`]` (the `ESC ]` prefix), the payload as plain chars, then Alt+`\`
/// (the ST terminator; a BEL reply ends as Ctrl+G). Without a filter those
/// would be typed into whatever is focused at startup — the composer.
///
/// This filter reassembles that shape: when the collected payload validates
/// as a real OSC 11 reply (via
/// [`parse_osc11_response`](crate::theme::detect::parse_osc11_response)),
/// the whole run is dropped. Anything that does not match is replayed
/// untouched, so genuine input is never lost — a false start is only
/// buffered until the next event resolves it.
#[derive(Debug, Default)]
pub(crate) struct OscReplyFilter {
    /// Key events collected since the leading Alt+`]` — the reply's body
    /// (the `ESC ]` prefix and the ST/BEL terminator are separate events).
    buffered: Vec<AppEvent>,
    /// ASCII bytes of the buffered payload, for reply validation.
    bytes: Vec<u8>,
}

/// A reply body is at most `11;rgb:` + 3×4 hex digits + 2 `/` = 26 chars;
/// anything longer is ordinary input, not a reply.
const MAX_OSC11_PAYLOAD: usize = 32;

impl OscReplyFilter {
    /// Feed one bridge event; returns the events the caller must forward.
    /// Usually just `event` itself; empty when the event completed and
    /// validated an OSC 11 reply (swallowed); possibly several events when
    /// a would-be reply turned out to be ordinary input (the buffered run
    /// is replayed, in order, before `event`).
    pub(crate) fn filter(&mut self, event: AppEvent) -> Vec<AppEvent> {
        let AppEvent::Key(key) = event else {
            // Any non-key event breaks a partial reply: replay the run.
            return self.replay(event);
        };
        if self.buffered.is_empty() {
            // Idle: only a leading Alt+`]` opens a reply run (`ESC ]`).
            return if is_osc11_start(&key) {
                self.buffered.push(AppEvent::Key(key));
                Vec::new()
            } else {
                vec![AppEvent::Key(key)]
            };
        }
        if is_osc11_end(&key) {
            // Terminator: is the collected payload a real OSC 11 reply?
            let mut reply = Vec::with_capacity(self.bytes.len() + 4);
            reply.extend_from_slice(b"\x1b]");
            reply.extend_from_slice(&self.bytes);
            reply.extend_from_slice(b"\x1b\\");
            let is_reply = crate::theme::detect::parse_osc11_response(&reply).is_some();
            let buffered = std::mem::take(&mut self.buffered);
            self.bytes.clear();
            if is_reply {
                return Vec::new(); // a real reply: swallowed.
            }
            let mut out = buffered;
            out.push(AppEvent::Key(key));
            return out;
        }
        if is_osc11_payload(&key) && self.bytes.len() < MAX_OSC11_PAYLOAD {
            if let KeyCode::Char(c) = key.code {
                self.bytes.push(c as u8);
            }
            self.buffered.push(AppEvent::Key(key));
            return Vec::new();
        }
        // A key that cannot be part of a reply breaks the run: replay.
        self.replay(AppEvent::Key(key))
    }

    /// Replay any buffered run (in order), then `event`, and reset.
    fn replay(&mut self, event: AppEvent) -> Vec<AppEvent> {
        let mut out = std::mem::take(&mut self.buffered);
        self.bytes.clear();
        out.push(event);
        out
    }
}

/// The leading event of a leaked OSC reply: `ESC ]` parses as Alt+`]`.
fn is_osc11_start(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char(']') && key.modifiers.contains(KeyModifiers::ALT)
}

/// The trailing event: the ST `ESC \` parses as Alt+`\`; a BEL (0x07)
/// reply parses as Ctrl+G (crossterm's control-char mapping).
fn is_osc11_end(key: &KeyEvent) -> bool {
    (key.code == KeyCode::Char('\\') && key.modifiers.contains(KeyModifiers::ALT))
        || (key.code == KeyCode::Char('g') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// A payload char of a leaked reply: plain text (Shift-capped hex included;
/// crossterm adds SHIFT to uppercase chars).
fn is_osc11_payload(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && !key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Spawn the crossterm input bridge: reads terminal events and forwards
/// Key/Resize/Mouse/Paste. `crossterm::event::read` blocks, so the loop runs
/// on the tokio blocking pool. Stops when the channel closes or the terminal
/// errors.
///
/// Every event passes through an [`OscReplyFilter`] first: a late OSC 11
/// reply (the startup background query, see [`OscReplyFilter`]) would
/// otherwise be typed into the composer as `]11;rgb:...\`.
///
/// Poll interval: 50ms — snappier wheel response than the historical 100ms
/// (#12; the 10Hz bridge still coalesces wheel bursts into one draw per
/// tick via `needs_draw` + `DRAW_INTERVAL`).
pub fn spawn_input_bridge(tx: mpsc::UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let mut osc = OscReplyFilter::default();
            loop {
                // Poll with a timeout instead of blocking forever: the loop
                // must be able to notice the channel closing so the app can
                // exit cleanly (an unbounded `read()` would hang the runtime
                // shutdown after Ctrl+Q).
                match crossterm::event::poll(Duration::from_millis(50)) {
                    Ok(true) => match crossterm::event::read() {
                        Ok(event) => {
                            let app_event = match event {
                                crossterm::event::Event::Key(key) => AppEvent::Key(key),
                                crossterm::event::Event::Mouse(mouse) => AppEvent::Mouse(mouse),
                                crossterm::event::Event::Paste(text) => AppEvent::Paste(text),
                                crossterm::event::Event::Resize(width, height) => {
                                    AppEvent::Resize(width, height)
                                }
                                _ => continue, // focus gain/loss etc.
                            };
                            for filtered in osc.filter(app_event) {
                                if tx.send(filtered).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {
                        // The run loop dropped the receiver (app quitting).
                        if tx.is_closed() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
        let _ = result;
    });
}

/// Spawn the mux frame bridge: drains the wire client's mux subscriber into
/// events. Answerable frames travel as [`AppEvent::Answerable`] (envelope
/// rpcId preserved); everything else as [`AppEvent::Frame`].
pub fn spawn_frame_bridge(
    mut mux: mpsc::UnboundedReceiver<DownlinkFrame<MuxFrame>>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(downlink) = mux.recv().await {
            let event = if is_answerable(&downlink.frame) {
                AppEvent::Answerable {
                    rpc_id: downlink.rpc_id,
                    frame: downlink.frame,
                }
            } else {
                AppEvent::Frame(downlink.frame)
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
}

/// Spawn the host frame bridge: drains the wire client's host subscriber
/// into [`AppEvent::HostFrame`]s. Host frames are pure pushes (the downlink
/// rpcId is ignored); the app handles the session-liveness subset and
/// ignores the rest (workspace grouping, archived filtering — later lanes).
pub fn spawn_host_bridge(
    mut host: mpsc::UnboundedReceiver<DownlinkFrame<HostFrame>>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(downlink) = host.recv().await {
            if tx.send(AppEvent::HostFrame(downlink.frame)).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A comparable projection of an `AppEvent` — the enum itself cannot
    /// derive `PartialEq` because of its `ClientError`-carrying variants.
    #[derive(Debug, PartialEq)]
    enum Token {
        Key(KeyCode, KeyModifiers),
        Mouse,
        Paste(String),
        Resize(u16, u16),
        Other,
    }

    fn token(event: &AppEvent) -> Token {
        match event {
            AppEvent::Key(key) => Token::Key(key.code, key.modifiers),
            AppEvent::Mouse(_) => Token::Mouse,
            AppEvent::Paste(text) => Token::Paste(text.clone()),
            AppEvent::Resize(w, h) => Token::Resize(*w, *h),
            _ => Token::Other,
        }
    }

    fn tokens(events: &[AppEvent]) -> Vec<Token> {
        events.iter().map(token).collect()
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> AppEvent {
        AppEvent::Key(KeyEvent::new(code, modifiers))
    }

    /// A plain char, SHIFT-capped like crossterm's `char_code_to_event`.
    fn plain(c: char) -> AppEvent {
        let modifiers = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::empty()
        };
        key(KeyCode::Char(c), modifiers)
    }

    fn alt(c: char) -> AppEvent {
        key(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// The event run crossterm emits for a leaked OSC 11 reply with body
    /// `payload` (`ESC ]` → Alt+`]`, body chars, ST → Alt+`\`, or BEL →
    /// Ctrl+G).
    fn reply_events(payload: &str, st: bool) -> Vec<AppEvent> {
        let mut events = vec![alt(']')];
        events.extend(payload.chars().map(plain));
        events.push(if st {
            alt('\\')
        } else {
            key(KeyCode::Char('g'), KeyModifiers::CONTROL)
        });
        events
    }

    fn feed_all(filter: &mut OscReplyFilter, events: Vec<AppEvent>) -> Vec<AppEvent> {
        let mut out = Vec::new();
        for event in events {
            out.extend(filter.filter(event));
        }
        out
    }

    #[test]
    fn swallows_a_st_terminated_reply() {
        // The exact shape the reporter saw: `]11;rgb:1f1f/1f1f/2828\`.
        let events = reply_events("11;rgb:1f1f/1f1f/2828", true);
        assert!(feed_all(&mut OscReplyFilter::default(), events).is_empty());
    }

    #[test]
    fn swallows_a_bel_terminated_reply() {
        let events = reply_events("11;rgb:0f0f/0f0f/0f0f", false);
        assert!(feed_all(&mut OscReplyFilter::default(), events).is_empty());
    }

    #[test]
    fn swallows_uppercase_hex_payload() {
        let events = reply_events("11;rgb:FFFF/0000/0000", true);
        assert!(feed_all(&mut OscReplyFilter::default(), events).is_empty());
    }

    #[test]
    fn swallows_the_hash_form_reply() {
        let events = reply_events("11;#1f1f28", true);
        assert!(feed_all(&mut OscReplyFilter::default(), events).is_empty());
    }

    #[test]
    fn ordinary_keys_pass_through() {
        let events = vec![plain('h'), plain('i')];
        let expected = tokens(&events);
        let out = feed_all(&mut OscReplyFilter::default(), events);
        assert_eq!(tokens(&out), expected);
    }

    #[test]
    fn input_around_a_reply_flows_unchanged() {
        let mut filter = OscReplyFilter::default();
        // Before the reply: forwarded immediately.
        let before = feed_all(&mut filter, vec![plain('x')]);
        assert_eq!(
            tokens(&before),
            vec![Token::Key(KeyCode::Char('x'), KeyModifiers::empty())]
        );
        // The reply itself: swallowed.
        assert!(feed_all(&mut filter, reply_events("11;rgb:1f1f/1f1f/2828", true)).is_empty());
        // After: forwarded immediately.
        let after = feed_all(&mut filter, vec![plain('y')]);
        assert_eq!(
            tokens(&after),
            vec![Token::Key(KeyCode::Char('y'), KeyModifiers::empty())]
        );
    }

    #[test]
    fn a_false_start_is_replayed_in_order() {
        // Alt+`]` + text that is not an OSC reply + Alt+`\`: nothing is
        // lost, the whole run comes back in order.
        let events = vec![alt(']'), plain('a'), plain('b'), alt('\\')];
        let expected = tokens(&events);
        let out = feed_all(&mut OscReplyFilter::default(), events);
        assert_eq!(tokens(&out), expected);
    }

    #[test]
    fn an_invalid_payload_is_replayed() {
        let events = reply_events("11;rgb:zz/zz/zz", true);
        let expected = tokens(&events);
        let out = feed_all(&mut OscReplyFilter::default(), events);
        assert_eq!(tokens(&out), expected);
    }

    #[test]
    fn a_non_key_event_breaks_a_partial_reply() {
        let mut filter = OscReplyFilter::default();
        let mut out = feed_all(&mut filter, vec![alt(']'), plain('1')]);
        assert!(out.is_empty()); // still buffered
        out = feed_all(&mut filter, vec![AppEvent::Resize(100, 50)]);
        assert_eq!(
            tokens(&out),
            vec![
                Token::Key(KeyCode::Char(']'), KeyModifiers::ALT),
                Token::Key(KeyCode::Char('1'), KeyModifiers::empty()),
                Token::Resize(100, 50),
            ]
        );
    }

    #[test]
    fn an_oversized_run_breaks_and_replays() {
        // A payload longer than any real reply: the run breaks at the cap
        // and everything is replayed, nothing dropped.
        let mut events = vec![alt(']')];
        events.extend((0..40).map(|_| plain('a')));
        events.push(alt('\\'));
        let expected = tokens(&events);
        let out = feed_all(&mut OscReplyFilter::default(), events);
        assert_eq!(tokens(&out), expected);
    }

    #[test]
    fn a_second_alt_close_bracket_breaks_a_partial_reply() {
        let events = vec![alt(']'), alt(']'), plain('a'), alt('\\')];
        let expected = tokens(&events);
        let out = feed_all(&mut OscReplyFilter::default(), events);
        assert_eq!(tokens(&out), expected);
    }
}
