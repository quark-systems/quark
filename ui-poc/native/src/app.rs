//! Root view: owns all app state and renders the five screens.
//!
//! Architecture: one `RootView` (a warpui `View` + `TypedActionView`) holds the state. Network
//! threads feed `Net` messages through an `async_channel`, consumed with
//! `ViewContext::spawn_stream_local`. All keyboard input is funneled through the custom
//! `KeyCapture` element into `AppAction`s.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use web_time::Instant;

use futures::StreamExt as _;

use warpui::color::ColorU;
use warpui::elements::{
    Border, ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, CrossAxisAlignment, DispatchEventResult, Empty, EventHandler, Expanded, Flex,
    Highlight, HighlightedRange, MainAxisAlignment, MainAxisSize, OffsetPositioning,
    ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Rect, SavePosition, ScrollTarget,
    ScrollToPositionMode, ScrollbarWidth, SelectableArea, SelectionHandle, Shrinkable, Stack, Text,
};
use warpui::fonts::{FamilyId, Properties, Style, Weight};
use warpui::geometry::vector::vec2f;
use warpui::text_layout::TextStyle;
use warpui::{
    AppContext, CursorInfo, Element, Entity, SingletonEntity as _, TypedActionView, View,
    ViewContext,
};

use crate::api::{ChatMessage, Client, Comment, Decision, Net, Project, PullRequest, Task, Worker};
use crate::diff::{DiffLine, Highlighter, Kind};
use crate::elements::{EditorLine, FrameProbe, KeyCapture, MonoFont, Sel, TermGrid, TextBuf};
use crate::markdown::{self, Block, Rich};
use crate::perf::{pct, Perf};
use crate::term::{Snapshot, TerminalCore};

// ------------------------------------------------------------------ theme

const fn c(hex: u32) -> ColorU {
    ColorU { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8, a: 255 }
}
const BG: ColorU = c(0x0f1115);
const PANEL: ColorU = c(0x15181e);
const PANEL2: ColorU = c(0x1b1f27);
const BORDER: ColorU = c(0x2a2f3a);
const TEXT: ColorU = c(0xe6e8ec);
const MUTED: ColorU = c(0x8b919c);
const DIM: ColorU = c(0x5c6370);
const ACCENT: ColorU = c(0x4d9cff);
const SEL_BG: ColorU = c(0x243552);
const GREEN: ColorU = c(0x98c379);
const RED: ColorU = c(0xe06c75);
const YELLOW: ColorU = c(0xe5c07b);
const PURPLE: ColorU = c(0xc678dd);
const CYAN: ColorU = c(0x56b6c2);

pub const STATES: [&str; 6] = ["queued", "running", "needs_decision", "review", "done", "failed"];

fn state_color(s: &str) -> ColorU {
    match s {
        "queued" => MUTED,
        "running" => ACCENT,
        "needs_decision" => YELLOW,
        "review" => PURPLE,
        "done" => GREEN,
        "failed" => RED,
        _ => MUTED,
    }
}

thread_local! {
    pub static PERF: RefCell<Perf> = RefCell::new(Perf::default());
}

// ------------------------------------------------------------------ types

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Board,
    Terminals,
    Chat,
    Diff,
    Inbox,
}
const SCREENS: [(Screen, &str); 5] = [
    (Screen::Board, "Board"),
    (Screen::Terminals, "Terminals"),
    (Screen::Chat, "Chat"),
    (Screen::Diff, "Diff"),
    (Screen::Inbox, "Decisions & PRs"),
];

#[derive(Debug, Clone)]
pub enum AppAction {
    Key { key: String, ctrl: bool, alt: bool, shift: bool, chars: String, at: Instant },
    Typed(String, Instant),
    Marked(Option<String>),
    TermMouse { pane: usize, cell: (usize, usize), phase: u8 },
    EditorClick { id: u8, index: usize, drag: bool },
    Click(ClickTarget),
    ChatSelection(Option<String>),
    /// Browser paste event (web): warpui stashes the pasted content in the clipboard and
    /// dispatches `StandardAction::Paste`, which is bound to this action.
    Paste,
}

#[derive(Debug, Clone)]
pub enum ClickTarget {
    Screen(usize),
    Project(usize),
    Task(String),
    DiffLine(usize),
    Decision(usize),
    DecisionOption(usize, usize),
    Pr(usize),
    PaletteItem(usize),
    FocusChat,
}

const ED_CHAT: u8 = 1;
const ED_COMMENT: u8 = 2;
const ED_PALETTE: u8 = 3;

struct Pane {
    worker: Worker,
    core: Box<dyn TerminalCore>,
    snap: Rc<RefCell<Snapshot>>,
    grid: Rc<Cell<(usize, usize)>>,
    sent: (usize, usize),
    sel: Option<Sel>,
    dirty: bool,
}

struct ChatMsg {
    id: String,
    role: String,
    text: String,
    streaming: bool,
    blocks: Rc<Vec<Block>>,
    parsed_len: usize,
}

impl ChatMsg {
    fn new(id: String, role: String, text: String, streaming: bool) -> Self {
        let blocks = Rc::new(markdown::parse(&text));
        let n = text.len();
        ChatMsg { id, role, text, streaming, blocks, parsed_len: n }
    }
    fn reparse(&mut self) {
        if self.parsed_len != self.text.len() {
            self.blocks = Rc::new(markdown::parse(&self.text));
            self.parsed_len = self.text.len();
        }
    }
}

#[derive(Clone)]
struct PaletteItem {
    label: String,
    hint: String,
    cmd: Cmd,
}

#[derive(Clone, Debug)]
enum Cmd {
    Screen(Screen),
    Project(usize),
    Pr(String),
    Answer(String, usize),
    TogglePerf,
    ToggleStress,
}

struct Palette {
    input: TextBuf,
    sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Nav,
    ChatInput,
    CommentInput,
    Terminal,
}

#[derive(Default)]
struct BenchPhase {
    name: &'static str,
    start: Option<Instant>,
    end: Option<Instant>,
    frame_ms: Vec<f64>,
    interval_ms: Vec<f64>,
    key_to_echo: Vec<f64>,
    key_to_paint: Vec<f64>,
    echo_queue: Vec<f64>,
    build: Vec<f64>,
    scene: Vec<f64>,
    gpu: Vec<f64>,
    rows_rebuilt: u64,
    events: u64,
    output_bytes: u64,
    feed_us: u64,
    snap_us: u64,
    chat_deltas: u64,
}

pub struct BenchConfig {
    pub seconds: u64,
    pub use_xdotool: bool,
}

pub struct Options {
    pub base: String,
    pub bench: Option<BenchConfig>,
    /// terminal core: "alacritty" or "ghostty"
    pub term: String,
}

struct Bench {
    cfg: BenchConfig,
    started: Option<Instant>,
    phase: usize,
    phases: Vec<BenchPhase>,
    last_key: Option<Instant>,
    keys_in_line: usize,
    awaiting_chat: bool,
    stress_cleared: bool,
    chat_deltas: u64,
    next_char: u8,
    wait_since: Instant,
}

// ------------------------------------------------------------------ fonts

#[derive(Clone, Copy)]
struct Fonts {
    ui: FamilyId,
    mono: MonoFont,
}

// ------------------------------------------------------------------ root view

pub struct RootView {
    client: Client,
    term_kind: String,
    fonts: Fonts,
    hl: Rc<Highlighter>,
    screen: Screen,
    focus: Focus,
    connected: bool,
    last_error: Option<String>,

    projects: Vec<Project>,
    sel_project: usize,
    tasks: Vec<Task>,
    board_sel: (usize, usize),

    workers_known: bool,
    panes: Vec<Pane>,
    focused_pane: usize,
    bash_pane: Option<usize>,

    chats: HashMap<String, Vec<ChatMsg>>,
    chat_loaded: HashMap<String, bool>,
    chat_input: TextBuf,
    chat_scroll: ClippedScrollStateHandle,
    chat_sel: SelectionHandle,
    chat_selected: Rc<RefCell<Option<String>>>,
    chat_follow: bool,
    chat_dock: bool,

    decisions: Vec<Decision>,
    prs: Vec<PullRequest>,
    inbox_col: usize,
    dec_sel: usize,
    pr_sel: usize,

    diff_pr: Option<String>,
    diff_lines: Rc<Vec<DiffLine>>,
    diff_raw_len: usize,
    comments: Rc<Vec<Comment>>,
    diff_cursor: usize,
    diff_scroll: ClippedScrollStateHandle,
    dec_scroll: ClippedScrollStateHandle,
    pr_scroll: ClippedScrollStateHandle,
    commenting: Option<usize>,
    comment_input: TextBuf,

    palette: Option<Palette>,
    show_perf: bool,
    stress: bool,
    bench: Option<Bench>,
    last_overlay_refresh: Instant,
    frames_since_tick: u64,
}

impl Entity for RootView {
    type Event = ();
}

impl RootView {
    pub fn new(ctx: &mut ViewContext<Self>, opts: Options) -> Self {
        let Options { base, bench, term } = opts;
        let (ui, mono_family) = warpui::fonts::Cache::handle(ctx).update(ctx, |cache, _| {
            #[cfg(not(target_family = "wasm"))]
            let (ui, mono) = (
                cache.load_system_font("DejaVu Sans").expect("DejaVu Sans"),
                cache.load_system_font("DejaVu Sans Mono").expect("DejaVu Sans Mono"),
            );
            // warpui has no system fonts (and no fallback fonts) on wasm: embed the two families.
            #[cfg(target_family = "wasm")]
            let (ui, mono) = (
                cache
                    .load_family_from_bytes(
                        "DejaVu Sans",
                        vec![
                            include_bytes!("../assets/fonts/DejaVuSans.ttf").to_vec(),
                            include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf").to_vec(),
                        ],
                    )
                    .expect("DejaVu Sans"),
                cache
                    .load_family_from_bytes(
                        "DejaVu Sans Mono",
                        vec![
                            include_bytes!("../assets/fonts/DejaVuSansMono.ttf").to_vec(),
                            include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf").to_vec(),
                        ],
                    )
                    .expect("DejaVu Sans Mono"),
            );
            (ui, mono)
        });
        let mono_size = 12.;
        // NB: `FontCache::em_width` is the *ink* width of 'm', not its advance; terminal cells need
        // the advance, so compute it from the glyph.
        let cell_w = {
            let fc = ctx.font_cache();
            let font = fc.select_font(mono_family, Properties::default());
            fc.glyph_for_char(font, 'M', false)
                .and_then(|(g, fid)| fc.glyph_advance(fid, mono_size, g).ok())
                .map(|v| v.x())
                .unwrap_or(mono_size * 0.6)
        };
        let fonts = Fonts {
            ui,
            mono: MonoFont { family: mono_family, size: mono_size, cell_w, line_h: (mono_size * 1.3).round() },
        };

        let (tx, rx) = async_channel::unbounded::<Net>();
        let client = Client::new(base, tx);
        client.load_initial();
        client.start_events();
        // Housekeeping tick (perf overlay refresh, resize sync, bench script).
        client.start_ticker(Duration::from_millis(50));
        // Drain network messages in batches: everything queued is applied before the next frame.
        ctx.spawn_stream_local(
            rx.ready_chunks(512),
            |view: &mut RootView, batch: Vec<Net>, ctx| view.apply_batch(batch, ctx),
            |_, _| {},
        );
        ctx.focus_self();

        let bench = bench.map(|cfg| Bench {
            cfg,
            started: None,
            phase: 0,
            phases: vec![
                BenchPhase { name: "idle_typing", ..Default::default() },
                BenchPhase { name: "stress_4_terminals_plus_chat", ..Default::default() },
            ],
            last_key: None,
            keys_in_line: 0,
            awaiting_chat: false,
            stress_cleared: false,
            chat_deltas: 0,
            next_char: b'A',
            wait_since: Instant::now(),
        });
        let screen = if bench.is_some() { Screen::Terminals } else { Screen::Board };

        RootView {
            client,
            term_kind: term,
            fonts,
            hl: Rc::new(Highlighter::new()),
            screen,
            focus: if bench.is_some() { Focus::Terminal } else { Focus::Nav },
            connected: false,
            last_error: None,
            projects: vec![],
            sel_project: 0,
            tasks: vec![],
            board_sel: (0, 0),
            workers_known: false,
            panes: vec![],
            focused_pane: 0,
            bash_pane: None,
            chats: HashMap::new(),
            chat_loaded: HashMap::new(),
            chat_input: TextBuf::default(),
            chat_scroll: Default::default(),
            chat_sel: Default::default(),
            chat_selected: Rc::new(RefCell::new(None)),
            chat_follow: true,
            chat_dock: bench.is_some(),
            decisions: vec![],
            prs: vec![],
            inbox_col: 0,
            dec_sel: 0,
            pr_sel: 0,
            diff_pr: None,
            diff_lines: Rc::new(vec![]),
            diff_raw_len: 0,
            comments: Rc::new(vec![]),
            diff_cursor: 0,
            diff_scroll: Default::default(),
            dec_scroll: Default::default(),
            pr_scroll: Default::default(),
            commenting: None,
            comment_input: TextBuf::default(),
            palette: None,
            show_perf: bench.is_some(),
            stress: false,
            bench,
            last_overlay_refresh: Instant::now(),
            frames_since_tick: 0,
        }
    }

    fn project_id(&self) -> Option<String> {
        self.projects.get(self.sel_project).map(|p| p.id.clone())
    }

    // -------------------------------------------------------------- network

    fn apply_batch(&mut self, batch: Vec<Net>, ctx: &mut ViewContext<Self>) {
        let mut changed = false;
        let mut echo_seen = false;
        for n in batch {
            if let Net::Tick = n {
                self.tick(ctx);
                continue;
            }
            changed = true;
            PERF.with(|p| p.borrow_mut().events += 1);
            match n {
                Net::Connected(c) => self.connected = c,
                Net::Projects(ps) => {
                    self.projects = ps;
                    self.ensure_chat_loaded();
                }
                Net::Tasks(pid, ts) => {
                    self.tasks.retain(|t| t.project_id != pid);
                    self.tasks.extend(ts);
                }
                Net::TaskUpsert(t) => {
                    if let Some(x) = self.tasks.iter_mut().find(|x| x.id == t.id) {
                        *x = t;
                    } else {
                        self.tasks.push(t);
                    }
                }
                Net::Decisions(d) => self.decisions = d,
                Net::DecisionUpsert(d) => {
                    if let Some(x) = self.decisions.iter_mut().find(|x| x.id == d.id) {
                        *x = d;
                    } else {
                        self.decisions.push(d);
                    }
                }
                Net::PullRequests(p) => self.prs = p,
                Net::PrUpsert(p) => {
                    if let Some(x) = self.prs.iter_mut().find(|x| x.id == p.id) {
                        *x = p;
                    } else {
                        self.prs.push(p);
                    }
                }
                Net::Diff(pr, d) => {
                    if Some(&pr) == self.diff_pr.as_ref() {
                        self.diff_raw_len = d.len();
                        self.diff_lines = Rc::new(self.hl.parse_diff(&d));
                        self.diff_cursor = 0;
                    }
                }
                Net::Comments(pr, cm) => {
                    if Some(&pr) == self.diff_pr.as_ref() {
                        self.comments = Rc::new(cm);
                    }
                }
                Net::ChatHistory(coord, msgs) => {
                    let list = self.chats.entry(coord).or_default();
                    // Keep any streaming message that arrived before history.
                    let streaming: Vec<ChatMsg> = list.drain(..).filter(|m| m.streaming).collect();
                    list.extend(msgs.into_iter().map(|m| ChatMsg::new(m.id, m.role, m.text, false)));
                    for s in streaming {
                        if !list.iter().any(|m| m.id == s.id) {
                            list.push(s);
                        }
                    }
                }
                Net::ChatMessage(coord, m) => self.chat_message(coord, m),
                Net::ChatDelta(coord, mid, text) => {
                    let list = self.chats.entry(coord).or_default();
                    if let Some(m) = list.iter_mut().find(|m| m.id == mid) {
                        m.text.push_str(&text);
                    } else {
                        list.push(ChatMsg::new(mid, "coordinator".into(), text, true));
                    }
                    if let Some(b) = self.bench.as_mut() {
                        b.chat_deltas += 1;
                    }
                }
                Net::Workers(ws) => self.set_workers(ws),
                Net::WorkerOutput(wid, bytes, ws_at) => {
                    let probe_byte = PERF.with(|p| p.borrow().probe.map(|(b, _, e)| (b, e.is_none())));
                    if let Some(i) = self.panes.iter().position(|p| p.worker.id == wid) {
                        PERF.with(|p| p.borrow_mut().output_bytes += bytes.len() as u64);
                        if Some(i) == self.bash_pane
                            && let Some((b, true)) = probe_byte
                            && bytes.contains(&b)
                        {
                            // Time the echo was read off the WebSocket (network thread), so
                            // daemon/tmux latency is separable from UI-thread queueing.
                            PERF.with(|p| {
                                let mut p = p.borrow_mut();
                                let applied = Instant::now();
                                if let Some(pr) = p.probe.as_mut() {
                                    pr.2 = Some(ws_at);
                                    let q = applied.duration_since(ws_at).as_secs_f64() * 1000.;
                                    p.echo_queue_ms.push(q);
                                }
                            });
                            echo_seen = true;
                        }
                        let pane = &mut self.panes[i];
                        let t = Instant::now();
                        pane.core.feed(&bytes);
                        let us = t.elapsed().as_micros() as u64;
                        PERF.with(|p| p.borrow_mut().feed_us += us);
                        pane.dirty = true;
                    }
                }
                Net::Error(e) => self.last_error = Some(e),
                Net::Tick => {}
            }
        }
        if changed {
            // Rebuild terminal snapshots only for panes that got output (and only their damaged rows).
            let mut rows = 0;
            let t = Instant::now();
            for p in self.panes.iter_mut().filter(|p| p.dirty) {
                p.dirty = false;
                rows += p.core.snapshot_into(&mut p.snap.borrow_mut());
            }
            let us = t.elapsed().as_micros() as u64;
            PERF.with(|p| {
                let mut p = p.borrow_mut();
                p.rows_rebuilt += rows as u64;
                p.snap_us += us;
            });
            for list in self.chats.values_mut() {
                for m in list.iter_mut().filter(|m| m.streaming) {
                    m.reparse();
                }
            }
            if self.chat_follow {
                self.chat_scroll.scroll_to_position(ScrollTarget {
                    position_id: "chat-bottom".into(),
                    mode: ScrollToPositionMode::FullyIntoView,
                });
            }
            if echo_seen {
                ctx.on_next_frame_drawn(|| {
                    let now = Instant::now();
                    PERF.with(|p| {
                        let mut p = p.borrow_mut();
                        if let Some((_, t0, Some(t1))) = p.probe.take() {
                            p.key_to_echo_ms.push(t1.duration_since(t0).as_secs_f64() * 1000.);
                            p.key_to_paint_ms.push(now.duration_since(t0).as_secs_f64() * 1000.);
                        }
                    });
                });
            }
            ctx.notify();
        }
    }

    fn chat_message(&mut self, coord: String, m: ChatMessage) {
        let list = self.chats.entry(coord).or_default();
        if let Some(x) = list.iter_mut().find(|x| x.id == m.id) {
            x.text = m.text;
            x.streaming = false;
            x.reparse();
        } else {
            list.push(ChatMsg::new(m.id, m.role.clone(), m.text, false));
        }
        if m.role == "coordinator"
            && let Some(b) = self.bench.as_mut()
        {
            b.awaiting_chat = false;
        }
    }

    fn ensure_chat_loaded(&mut self) {
        if let Some(pid) = self.project_id()
            && !self.chat_loaded.contains_key(&pid)
        {
            self.chat_loaded.insert(pid.clone(), true);
            self.client.load_chat(&pid);
        }
    }

    fn set_workers(&mut self, ws: Vec<Worker>) {
        self.workers_known = true;
        let mono = self.fonts.mono;
        let _ = mono;
        for w in ws.into_iter().take(4) {
            if self.panes.iter().any(|p| p.worker.id == w.id) {
                continue;
            }
            let (cols, rows) = (w.cols.max(20) as usize, w.rows.max(5) as usize);
            let mut core = crate::term::new_core(&self.term_kind, cols, rows);
            let snap = Rc::new(RefCell::new(Snapshot::default()));
            core.snapshot_into(&mut snap.borrow_mut());
            let t = w.title.to_lowercase();
            let is_bash = t.contains("bash") || t.contains("shell") || w.id.to_lowercase().contains("bash");
            self.panes.push(Pane {
                worker: w,
                core,
                snap,
                grid: Rc::new(Cell::new((0, 0))),
                sent: (cols, rows),
                sel: None,
                dirty: false,
            });
            if is_bash && self.bash_pane.is_none() {
                self.bash_pane = Some(self.panes.len() - 1);
            }
        }
        if self.bash_pane.is_none() && !self.panes.is_empty() {
            self.bash_pane = Some(0);
        }
        if let Some(b) = self.bash_pane
            && self.bench.is_some()
        {
            self.focused_pane = b;
        }
    }

    fn tick(&mut self, ctx: &mut ViewContext<Self>) {
        // Resize panes whose on-screen grid changed (debounced to the tick).
        if self.screen == Screen::Terminals {
            for p in self.panes.iter_mut() {
                let g = p.grid.get();
                if g.0 >= 2 && g.1 >= 1 && g != p.sent {
                    p.sent = g;
                    p.core.resize(g.0, g.1);
                    p.core.snapshot_into(&mut p.snap.borrow_mut());
                    self.client.worker_resize(&p.worker.id, g.0 as u16, g.1 as u16);
                    ctx.notify();
                }
            }
        }
        if self.show_perf && self.last_overlay_refresh.elapsed() > Duration::from_millis(250) {
            self.last_overlay_refresh = Instant::now();
            ctx.notify();
        }
        self.frames_since_tick = 0;
        self.bench_tick(ctx);
    }

    // -------------------------------------------------------------- bench

    fn bench_tick(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(b) = self.bench.as_mut() else { return };
        let now = Instant::now();
        if b.started.is_none() {
            // Wait until connected with workers (or give up after 20s).
            let ready = self.connected && !self.panes.is_empty() && self.panes.iter().all(|p| p.grid.get().0 > 0);
            if !ready {
                if b.wait_since.elapsed() > Duration::from_secs(20) {
                    println!("{{\"error\":\"daemon not ready (connected={}, panes={})\"}}", self.connected, self.panes.len());
                    std::process::exit(2);
                }
                return;
            }
            // A previous run may have left floods on; make sure the idle phase really is idle.
            if !b.stress_cleared {
                b.stress_cleared = true;
                for p in &self.panes {
                    self.client.worker_stress(&p.worker.id, false);
                }
            }
            // Let the initial replay settle before measuring.
            if b.wait_since.elapsed() < Duration::from_secs(3) {
                return;
            }
            b.started = Some(now);
            b.phases[0].start = Some(now);
            PERF.with(|p| p.borrow_mut().reset_bench());
            if let Some(bp) = self.bash_pane {
                self.focused_pane = bp;
                // Clear the prompt line.
                self.client.worker_input(&self.panes[bp].worker.id, vec![0x15]);
            }
            return;
        }
        let total = Duration::from_secs(b.cfg.seconds);
        let started = b.started.unwrap();
        let elapsed = now.duration_since(started);
        let idle_len = total.mul_f64(0.35);
        if b.phase == 0 && elapsed >= idle_len {
            Self::close_phase(&mut b.phases[0], now, b.chat_deltas);
            b.phase = 1;
            b.phases[1].start = Some(now);
            b.chat_deltas = 0;
            PERF.with(|p| p.borrow_mut().reset_bench());
            // Stress on for all 4 panes, start a chat stream.
            self.stress = true;
            for p in &self.panes {
                self.client.worker_stress(&p.worker.id, true);
            }
            b.awaiting_chat = false;
        }
        if elapsed >= total {
            Self::close_phase(&mut b.phases[1], now, b.chat_deltas);
            for p in &self.panes {
                self.client.worker_stress(&p.worker.id, false);
            }
            self.client.flush_ordered(std::time::Duration::from_secs(5));
            self.print_bench();
            std::process::exit(0);
        }
        // Keep the coordinator streaming during the stress phase.
        if b.phase == 1 && !b.awaiting_chat {
            b.awaiting_chat = true;
            if let Some(pid) = self.projects.get(self.sel_project).map(|p| p.id.clone()) {
                self.client.send_chat(
                    &pid,
                    "Bench: summarize the status of all workers with a code example and a short list.".into(),
                );
            }
        }
        // Keystroke probe: one outstanding key at a time, ~8 keys/s.
        let pending = PERF.with(|p| p.borrow().probe.map(|(_, t0, _)| t0));
        if let Some(t0) = pending {
            if t0.elapsed() > Duration::from_secs(2) {
                PERF.with(|p| p.borrow_mut().probe = None); // lost; drop it
            } else {
                return;
            }
        }
        if b.last_key.is_some_and(|t| t.elapsed() < Duration::from_millis(120)) {
            return;
        }
        let Some(bp) = self.bash_pane else { return };
        b.last_key = Some(now);
        if b.keys_in_line >= 40 {
            b.keys_in_line = 0;
            self.client.worker_input(&self.panes[bp].worker.id, vec![0x15]); // ^U
            return;
        }
        b.keys_in_line += 1;
        let ch = b.next_char;
        // Uppercase probes: the stress flood output contains no uppercase ASCII, so an echo can
        // be detected unambiguously even while the pane floods.
        b.next_char = if ch >= b'Z' { b'A' } else { ch + 1 };
        if b.cfg.use_xdotool {
            // Real X11 key event through the platform input path; the probe timestamp is taken
            // when the app receives the event (see `on_typed`).
            let s = (ch as char).to_string();
            std::thread::spawn(move || {
                let _ = std::process::Command::new("xdotool").args(["type", "--delay", "0", &s]).status();
            });
        } else {
            self.on_typed((ch as char).to_string(), Instant::now(), ctx);
        }
    }

    fn close_phase(ph: &mut BenchPhase, now: Instant, chat_deltas: u64) {
        ph.end = Some(now);
        PERF.with(|p| {
            let p = p.borrow();
            ph.frame_ms = p.all_frame_ms.clone();
            ph.interval_ms = p.all_intervals_ms.clone();
            ph.key_to_echo = p.key_to_echo_ms.clone();
            ph.key_to_paint = p.key_to_paint_ms.clone();
            ph.echo_queue = p.echo_queue_ms.clone();
            ph.build = p.build_ms.clone();
            ph.scene = p.scene_ms.clone();
            ph.gpu = p.gpu_ms.clone();
            ph.rows_rebuilt = p.rows_rebuilt;
            ph.events = p.events;
            ph.output_bytes = p.output_bytes;
            ph.feed_us = p.feed_us;
            ph.snap_us = p.snap_us;
        });
        ph.chat_deltas = chat_deltas;
    }

    fn print_bench(&self) {
        let b = self.bench.as_ref().unwrap();
        let r = |v: f64| if v.is_nan() { serde_json::Value::Null } else { serde_json::json!((v * 100.).round() / 100.) };
        let phases: Vec<serde_json::Value> = b
            .phases
            .iter()
            .map(|ph| {
                let secs = ph.end.zip(ph.start).map(|(e, s)| e.duration_since(s).as_secs_f64()).unwrap_or(0.);
                serde_json::json!({
                    "phase": ph.name,
                    "seconds": r(secs),
                    "frames": ph.frame_ms.len(),
                    "fps_avg": r(ph.frame_ms.len() as f64 / secs.max(0.001)),
                    "frame_interval_ms": {"p50": r(pct(&ph.interval_ms, 0.5)), "p99": r(pct(&ph.interval_ms, 0.99))},
                    "frame_cpu_ms_layout_to_present": {"p50": r(pct(&ph.frame_ms, 0.5)), "p99": r(pct(&ph.frame_ms, 0.99)), "max": r(ph.frame_ms.iter().cloned().fold(f64::NAN, f64::max))},
                    "keystrokes_measured": ph.key_to_paint.len(),
                    "key_to_echo_received_on_ws_thread_ms": {"p50": r(pct(&ph.key_to_echo, 0.5)), "p99": r(pct(&ph.key_to_echo, 0.99))},
                    "echo_ws_thread_to_ui_thread_ms": {"p50": r(pct(&ph.echo_queue, 0.5)), "p99": r(pct(&ph.echo_queue, 0.99))},
                    "frame_split_p50_ms": {"view_render": r(pct(&ph.build, 0.5)), "layout_paint": r(pct(&ph.scene, 0.5)), "gpu_submit_present": r(pct(&ph.gpu, 0.5))},
                    "frame_split_p99_ms": {"view_render": r(pct(&ph.build, 0.99)), "layout_paint": r(pct(&ph.scene, 0.99)), "gpu_submit_present": r(pct(&ph.gpu, 0.99))},
                    "key_to_echo_painted_ms": {"p50": r(pct(&ph.key_to_paint, 0.5)), "p99": r(pct(&ph.key_to_paint, 0.99))},
                    "terminal_rows_rebuilt": ph.rows_rebuilt,
                    "events_applied": ph.events,
                    "worker_output_bytes": ph.output_bytes,
                    "terminal_core_feed_ms_total": r(ph.feed_us as f64 / 1000.),
                    "terminal_core_snapshot_ms_total": r(ph.snap_us as f64 / 1000.),
                    "terminal_core_ms_per_mb": r((ph.feed_us + ph.snap_us) as f64 / 1000. / (ph.output_bytes.max(1) as f64 / 1e6)),
                    "coordinator_deltas": ph.chat_deltas,
                })
            })
            .collect();
        let out = serde_json::json!({
            "app": "native-warpui",
            "terminal_core": self.panes.first().map(|p| p.core.name()).unwrap_or("none"),
            "input_path": if b.cfg.use_xdotool { "xdotool -> X11 -> winit -> warpui" } else { "in-process synthetic TypedCharacters" },
            "panes": self.panes.len(),
            "phases": phases,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    }

    // -------------------------------------------------------------- input

    fn editor_mut(&mut self) -> Option<&mut TextBuf> {
        if let Some(p) = self.palette.as_mut() {
            return Some(&mut p.input);
        }
        match self.focus {
            Focus::ChatInput => Some(&mut self.chat_input),
            Focus::CommentInput => Some(&mut self.comment_input),
            _ => None,
        }
    }

    fn set_screen(&mut self, s: Screen) {
        self.screen = s;
        self.focus = match s {
            Screen::Chat => Focus::ChatInput,
            Screen::Terminals => Focus::Terminal,
            _ => Focus::Nav,
        };
        if s == Screen::Diff && self.diff_pr.is_none()
            && let Some(pr) = self.prs.get(self.pr_sel).map(|p| p.id.clone())
        {
            self.open_diff(pr);
        }
        if s == Screen::Chat {
            self.chat_follow = true;
            self.ensure_chat_loaded();
        }
    }

    fn open_diff(&mut self, pr: String) {
        self.diff_pr = Some(pr.clone());
        self.diff_lines = Rc::new(vec![]);
        self.comments = Rc::new(vec![]);
        self.commenting = None;
        self.client.load_diff(&pr);
    }

    fn on_typed(&mut self, s: String, at: Instant, ctx: &mut ViewContext<Self>) {
        if let Some(ed) = self.editor_mut() {
            ed.marked = None;
            ed.insert(&s);
            if self.palette.is_some() {
                self.palette.as_mut().unwrap().sel = 0;
            }
            ctx.notify();
            return;
        }
        match self.screen {
            Screen::Terminals if self.focus == Focus::Terminal => {
                if let Some(p) = self.panes.get_mut(self.focused_pane) {
                    if Some(self.focused_pane) == self.bash_pane && s.len() == 1 {
                        let b = s.as_bytes()[0];
                        PERF.with(|pp| {
                            let mut pp = pp.borrow_mut();
                            if pp.probe.is_none() {
                                pp.probe = Some((b, at, None));
                            }
                        });
                    }
                    let bytes = p.core.encode_text(&s);
                    self.client.worker_input(&p.worker.id, bytes);
                }
            }
            _ => self.nav_char(&s, ctx),
        }
    }

    fn nav_char(&mut self, s: &str, ctx: &mut ViewContext<Self>) {
        match self.screen {
            Screen::Board => {
                let counts = self.board_counts();
                let (mut col, mut row) = self.board_sel;
                match s {
                    "j" => row += 1,
                    "k" => row = row.saturating_sub(1),
                    "l" => col = (col + 1).min(STATES.len() - 1),
                    "h" => col = col.saturating_sub(1),
                    "[" => self.sel_project = self.sel_project.saturating_sub(1),
                    "]" => self.sel_project = (self.sel_project + 1).min(self.projects.len().saturating_sub(1)),
                    _ => {}
                }
                row = row.min(counts[col].saturating_sub(1));
                self.board_sel = (col, row);
                self.ensure_chat_loaded();
            }
            Screen::Inbox => {
                let decs = self.visible_decisions();
                match s {
                    "j" => {
                        if self.inbox_col == 0 {
                            self.dec_sel = (self.dec_sel + 1).min(decs.len().saturating_sub(1));
                        } else {
                            self.pr_sel = (self.pr_sel + 1).min(self.prs.len().saturating_sub(1));
                        }
                    }
                    "k" => {
                        if self.inbox_col == 0 {
                            self.dec_sel = self.dec_sel.saturating_sub(1);
                        } else {
                            self.pr_sel = self.pr_sel.saturating_sub(1);
                        }
                    }
                    "g" => self.dec_sel = 0,
                    "h" => self.inbox_col = 0,
                    "l" => self.inbox_col = 1,
                    d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && self.inbox_col == 0 => {
                        let n = (d.as_bytes()[0] - b'0') as usize;
                        if n >= 1
                            && let Some(dec) = decs.get(self.dec_sel)
                            && dec.state == "open"
                            && n <= dec.options.len()
                        {
                            self.client.answer_decision(&dec.id, n - 1);
                        }
                    }
                    _ => {}
                }
            }
            Screen::Diff => match s {
                "j" => self.diff_move(1),
                "k" => self.diff_move(-1),
                "c" => self.start_comment(self.diff_cursor),
                "[" | "]" => {
                    if !self.prs.is_empty() {
                        let cur = self.diff_pr.as_ref().and_then(|id| self.prs.iter().position(|p| &p.id == id)).unwrap_or(0);
                        let next = if s == "]" { (cur + 1) % self.prs.len() } else { (cur + self.prs.len() - 1) % self.prs.len() };
                        self.pr_sel = next;
                        self.open_diff(self.prs[next].id.clone());
                    }
                }
                _ => {}
            },
            _ => {}
        }
        if self.screen == Screen::Inbox {
            self.dec_scroll.scroll_to_position(ScrollTarget { position_id: "dec-sel".into(), mode: ScrollToPositionMode::FullyIntoView });
        }
        ctx.notify();
    }

    fn diff_move(&mut self, d: i64) {
        let n = self.diff_lines.len() as i64;
        if n == 0 {
            return;
        }
        self.diff_cursor = (self.diff_cursor as i64 + d).clamp(0, n - 1) as usize;
        self.diff_scroll.scroll_to_position(ScrollTarget { position_id: "diff-cursor".into(), mode: ScrollToPositionMode::FullyIntoView });
    }

    fn start_comment(&mut self, line: usize) {
        if let Some(l) = self.diff_lines.get(line)
            && matches!(l.kind, Kind::Add | Kind::Ctx | Kind::Del)
        {
            self.diff_cursor = line;
            self.commenting = Some(line);
            self.comment_input = TextBuf::default();
            self.focus = Focus::CommentInput;
        }
    }

    fn visible_decisions(&self) -> Vec<Decision> {
        let mut v: Vec<Decision> = self.decisions.clone();
        v.sort_by_key(|d| (d.state != "open", d.id.clone()));
        v
    }

    fn board_counts(&self) -> [usize; 6] {
        let pid = self.project_id();
        let mut c = [0; 6];
        for t in self.tasks.iter().filter(|t| Some(&t.project_id) == pid.as_ref()) {
            if let Some(i) = STATES.iter().position(|s| *s == t.state) {
                c[i] += 1;
            }
        }
        c
    }

    fn palette_items(&self) -> Vec<PaletteItem> {
        let mut items = vec![];
        for (s, name) in SCREENS {
            items.push(PaletteItem { label: format!("Go to {name}"), hint: "screen".into(), cmd: Cmd::Screen(s) });
        }
        for (i, p) in self.projects.iter().enumerate() {
            items.push(PaletteItem { label: format!("Project: {}", p.name), hint: p.repo.clone(), cmd: Cmd::Project(i) });
        }
        for p in &self.prs {
            items.push(PaletteItem { label: format!("PR #{} {}", p.number, p.title), hint: format!("{} · {}", p.state, p.checks), cmd: Cmd::Pr(p.id.clone()) });
        }
        for d in self.decisions.iter().filter(|d| d.state == "open") {
            for (i, o) in d.options.iter().enumerate() {
                items.push(PaletteItem {
                    label: format!("Answer → {}  ·  {}", o.label, d.question),
                    hint: format!("{} · {}", d.project_id, o.consequence),
                    cmd: Cmd::Answer(d.id.clone(), i),
                });
            }
        }
        items.push(PaletteItem { label: "Toggle perf overlay".into(), hint: "F2".into(), cmd: Cmd::TogglePerf });
        items.push(PaletteItem { label: "Toggle stress (all 4 terminals)".into(), hint: "Ctrl+Shift+S".into(), cmd: Cmd::ToggleStress });
        let q = self.palette.as_ref().map(|p| p.input.text.to_lowercase()).unwrap_or_default();
        if q.is_empty() {
            return items;
        }
        // Simple subsequence fuzzy filter.
        items
            .into_iter()
            .filter(|it| {
                let hay = it.label.to_lowercase();
                let mut hc = hay.chars();
                q.chars().filter(|c| !c.is_whitespace()).all(|qc| hc.any(|h| h == qc))
            })
            .collect()
    }

    fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Screen(s) => self.set_screen(s),
            Cmd::Project(i) => {
                self.sel_project = i;
                self.ensure_chat_loaded();
            }
            Cmd::Pr(id) => {
                if let Some(i) = self.prs.iter().position(|p| p.id == id) {
                    self.pr_sel = i;
                }
                self.open_diff(id);
                self.set_screen(Screen::Diff);
            }
            Cmd::Answer(id, opt) => self.client.answer_decision(&id, opt),
            Cmd::TogglePerf => self.show_perf = !self.show_perf,
            Cmd::ToggleStress => self.toggle_stress(),
        }
    }

    fn toggle_stress(&mut self) {
        self.stress = !self.stress;
        for p in &self.panes {
            self.client.worker_stress(&p.worker.id, self.stress);
        }
    }

    fn copy(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        ctx.clipboard().write(warpui::clipboard::ClipboardContent::plain_text(text));
    }

    fn paste_text(&mut self, ctx: &mut ViewContext<Self>) -> String {
        ctx.clipboard().read().plain_text
    }

    fn on_key(&mut self, key: &str, ctrl: bool, alt: bool, shift: bool, chars: &str, ctx: &mut ViewContext<Self>) {
        // With Shift held, winit reports the shifted key ("S"); normalise for chord matching.
        let lower = key.to_lowercase();
        let key = lower.as_str();
        // Global chords.
        if ctrl && key == "k" {
            self.palette = if self.palette.is_some() { None } else { Some(Palette { input: TextBuf::default(), sel: 0 }) };
            ctx.notify();
            return;
        }
        if key == "f2" {
            self.show_perf = !self.show_perf;
            ctx.notify();
            return;
        }
        if ctrl && shift && key == "s" {
            self.toggle_stress();
            ctx.notify();
            return;
        }
        if ctrl && !shift && key.len() == 1 && ("1"..="5").contains(&key) {
            let i = key.parse::<usize>().unwrap() - 1;
            self.palette = None;
            self.set_screen(SCREENS[i].0);
            ctx.notify();
            return;
        }
        if ctrl && key == "j" && self.screen == Screen::Terminals {
            self.chat_dock = !self.chat_dock;
            ctx.notify();
            return;
        }
        if alt && (key == "up" || key == "down") {
            let n = self.projects.len();
            if n > 0 {
                self.sel_project = if key == "up" { (self.sel_project + n - 1) % n } else { (self.sel_project + 1) % n };
                self.ensure_chat_loaded();
            }
            ctx.notify();
            return;
        }

        // Palette.
        if self.palette.is_some() {
            let n = self.palette_items().len();
            match key {
                "escape" => self.palette = None,
                "down" => {
                    let p = self.palette.as_mut().unwrap();
                    p.sel = (p.sel + 1).min(n.saturating_sub(1));
                }
                "up" => {
                    let p = self.palette.as_mut().unwrap();
                    p.sel = p.sel.saturating_sub(1);
                }
                "enter" => {
                    let sel = self.palette.as_ref().unwrap().sel;
                    if let Some(it) = self.palette_items().get(sel).cloned() {
                        self.palette = None;
                        self.run_cmd(it.cmd);
                    }
                }
                _ => {
                    if ctrl && key == "v" {
                        let t = self.paste_text(ctx);
                        self.palette.as_mut().unwrap().input.insert(&t);
                    } else {
                        self.palette.as_mut().unwrap().input.handle_key(key, ctrl, shift);
                    }
                }
            }
            ctx.notify();
            return;
        }

        // Text inputs.
        if matches!(self.focus, Focus::ChatInput | Focus::CommentInput) {
            let is_chat = self.focus == Focus::ChatInput;
            match key {
                "enter" if !shift => {
                    let ed = if is_chat { &mut self.chat_input } else { &mut self.comment_input };
                    let text = ed.take();
                    if !text.trim().is_empty() {
                        if is_chat {
                            if let Some(pid) = self.project_id() {
                                self.client.send_chat(&pid, text);
                                self.chat_follow = true;
                            }
                        } else if let (Some(line), Some(pr)) = (self.commenting, self.diff_pr.clone()) {
                            let l = &self.diff_lines[line];
                            let ln = l.new_no.or(l.old_no).unwrap_or(0);
                            self.client.post_comment(&pr, l.path.clone(), ln, text);
                            self.commenting = None;
                            self.focus = Focus::Nav;
                        }
                    }
                }
                "escape" => {
                    if is_chat {
                        if self.screen == Screen::Terminals {
                            self.focus = Focus::Terminal;
                        } else {
                            self.focus = Focus::Nav;
                        }
                    } else {
                        self.commenting = None;
                        self.focus = Focus::Nav;
                    }
                }
                "c" if ctrl => {
                    let sel = if is_chat { self.chat_input.selected_text() } else { self.comment_input.selected_text() };
                    let sel = sel.or_else(|| self.chat_selected.borrow().clone());
                    if let Some(t) = sel {
                        self.copy(t, ctx);
                    }
                }
                "x" if ctrl => {
                    let ed = if is_chat { &mut self.chat_input } else { &mut self.comment_input };
                    if let Some(t) = ed.selected_text() {
                        ed.delete_selection();
                        self.copy(t, ctx);
                    }
                }
                "v" if ctrl => {
                    let t = self.paste_text(ctx);
                    let ed = if is_chat { &mut self.chat_input } else { &mut self.comment_input };
                    ed.insert(&t.replace('\n', " "));
                }
                "up" | "pageup" if is_chat => {
                    self.chat_follow = false;
                    self.chat_scroll.scroll_by(warpui::units::Pixels::new(if key == "up" { -40. } else { -400. }));
                }
                "down" | "pagedown" if is_chat => {
                    self.chat_scroll.scroll_by(warpui::units::Pixels::new(if key == "down" { 40. } else { 400. }));
                }
                _ => {
                    let ed = if is_chat { &mut self.chat_input } else { &mut self.comment_input };
                    ed.handle_key(key, ctrl, shift);
                }
            }
            ctx.notify();
            return;
        }

        // Terminals.
        if self.screen == Screen::Terminals && self.focus == Focus::Terminal {
            if alt && ("1"..="4").contains(&key) && key.len() == 1 {
                self.focused_pane = key.parse::<usize>().unwrap() - 1;
                ctx.notify();
                return;
            }
            if ctrl && shift && key == "c" {
                if let Some(p) = self.panes.get(self.focused_pane) {
                    let text = match p.sel {
                        Some(sel) => selection_text(&p.snap.borrow(), sel),
                        None => {
                            let snap = p.snap.borrow();
                            let last = (snap.rows.saturating_sub(1), snap.cols.saturating_sub(1));
                            selection_text(&snap, ((0, 0), last))
                        }
                    };
                    self.copy(text, ctx);
                }
                return;
            }
            if ctrl && shift && key == "v" {
                let t = self.paste_text(ctx);
                if let Some(p) = self.panes.get(self.focused_pane) {
                    self.client.worker_input(&p.worker.id, t.into_bytes());
                }
                return;
            }
            if self.chat_dock && ctrl && key == "l" {
                self.focus = Focus::ChatInput;
                ctx.notify();
                return;
            }
            if let Some(p) = self.panes.get_mut(self.focused_pane)
                && let Some(bytes) = p.core.encode_key(key, ctrl, alt, shift, chars)
            {
                self.client.worker_input(&p.worker.id, bytes);
            }
            return;
        }

        // List navigation screens.
        match (self.screen, key) {
            (Screen::Board, "down") => self.nav_char("j", ctx),
            (Screen::Board, "up") => self.nav_char("k", ctx),
            (Screen::Board, "left") => self.nav_char("h", ctx),
            (Screen::Board, "right") => self.nav_char("l", ctx),
            (Screen::Inbox, "down") => self.nav_char("j", ctx),
            (Screen::Inbox, "up") => self.nav_char("k", ctx),
            (Screen::Inbox, "tab") => {
                self.inbox_col = 1 - self.inbox_col;
                ctx.notify();
            }
            (Screen::Inbox, "enter") => {
                if self.inbox_col == 1 {
                    if let Some(pr) = self.prs.get(self.pr_sel).map(|p| p.id.clone()) {
                        self.open_diff(pr);
                        self.set_screen(Screen::Diff);
                    }
                } else if let Some(d) = self.visible_decisions().get(self.dec_sel)
                    && d.state == "open"
                {
                    let opt = d.recommended.unwrap_or(0);
                    self.client.answer_decision(&d.id, opt);
                }
                ctx.notify();
            }
            (Screen::Diff, "down") => {
                self.diff_move(1);
                ctx.notify();
            }
            (Screen::Diff, "up") => {
                self.diff_move(-1);
                ctx.notify();
            }
            (Screen::Diff, "pagedown") => {
                self.diff_move(30);
                ctx.notify();
            }
            (Screen::Diff, "pageup") => {
                self.diff_move(-30);
                ctx.notify();
            }
            (Screen::Diff, "enter") => {
                self.start_comment(self.diff_cursor);
                ctx.notify();
            }
            (Screen::Chat, "enter") => {
                self.focus = Focus::ChatInput;
                ctx.notify();
            }
            (Screen::Chat, "c") if ctrl => {
                let t = self.chat_selected.borrow().clone();
                if let Some(t) = t {
                    self.copy(t, ctx);
                }
            }
            _ => {}
        }
    }
}

fn selection_text(snap: &Snapshot, sel: Sel) -> String {
    let (a, b) = if sel.0 <= sel.1 { (sel.0, sel.1) } else { (sel.1, sel.0) };
    let mut out = String::new();
    for r in a.0..=b.0.min(snap.rows.saturating_sub(1)) {
        let c0 = if r == a.0 { a.1 } else { 0 };
        let c1 = if r == b.0 { b.1 } else { snap.cols.saturating_sub(1) };
        let mut line = String::new();
        for c in c0..=c1.min(snap.cols.saturating_sub(1)) {
            let cell = snap.cells[r * snap.cols + c];
            if cell.width != 0 {
                line.push(cell.ch);
            }
        }
        out.push_str(line.trim_end());
        if r != b.0 {
            out.push('\n');
        }
    }
    out
}

// ------------------------------------------------------------------ actions

impl TypedActionView for RootView {
    type Action = AppAction;

    fn handle_action(&mut self, action: &AppAction, ctx: &mut ViewContext<Self>) {
        match action {
            AppAction::Key { key, ctrl, alt, shift, chars, at } => {
                // Enter/backspace etc. in the bash pane are not probed; only printable chars are.
                let _ = at;
                self.on_key(key, *ctrl, *alt, *shift, chars, ctx)
            }
            AppAction::Typed(s, at) => self.on_typed(s.clone(), *at, ctx),
            AppAction::Marked(m) => {
                if let Some(ed) = self.editor_mut() {
                    ed.marked = m.clone().filter(|s| !s.is_empty());
                    ctx.notify();
                }
            }
            AppAction::TermMouse { pane, cell, phase } => {
                self.focused_pane = *pane;
                self.focus = Focus::Terminal;
                if let Some(p) = self.panes.get_mut(*pane) {
                    match phase {
                        0 => p.sel = Some((*cell, *cell)),
                        1 => {
                            if let Some(s) = p.sel.as_mut() {
                                s.1 = *cell;
                            }
                        }
                        _ => {
                            if let Some(s) = p.sel
                                && s.0 == s.1
                            {
                                p.sel = None;
                            } else if let Some(s) = p.sel {
                                // X11-style: selecting copies (also Ctrl+Shift+C).
                                let t = selection_text(&p.snap.borrow(), s);
                                ctx.clipboard().write(warpui::clipboard::ClipboardContent::plain_text(t));
                            }
                        }
                    }
                }
                ctx.notify();
            }
            AppAction::EditorClick { id, index, drag } => {
                self.focus = match id {
                    &ED_CHAT => Focus::ChatInput,
                    &ED_COMMENT => Focus::CommentInput,
                    _ => self.focus,
                };
                if let Some(ed) = self.editor_mut() {
                    ed.move_to(*index, *drag);
                }
                ctx.notify();
            }
            AppAction::ChatSelection(s) => {
                *self.chat_selected.borrow_mut() = s.clone();
            }
            AppAction::Paste => {
                let t = self.paste_text(ctx);
                if let Some(ed) = self.editor_mut() {
                    ed.insert(&t.replace('\n', " "));
                } else if self.screen == Screen::Terminals
                    && self.focus == Focus::Terminal
                    && let Some(p) = self.panes.get(self.focused_pane)
                {
                    self.client.worker_input(&p.worker.id, t.into_bytes());
                }
                ctx.notify();
            }
            AppAction::Click(t) => {
                match t.clone() {
                    ClickTarget::Screen(i) => self.set_screen(SCREENS[i].0),
                    ClickTarget::Project(i) => {
                        self.sel_project = i;
                        self.ensure_chat_loaded();
                    }
                    ClickTarget::Task(id) => {
                        let pid = self.project_id();
                        for (ci, s) in STATES.iter().enumerate() {
                            let col: Vec<&Task> = self.tasks.iter().filter(|t| Some(&t.project_id) == pid.as_ref() && t.state == *s).collect();
                            if let Some(ri) = col.iter().position(|t| t.id == id) {
                                self.board_sel = (ci, ri);
                            }
                        }
                    }
                    ClickTarget::DiffLine(i) => self.start_comment(i),
                    ClickTarget::Decision(i) => {
                        self.inbox_col = 0;
                        self.dec_sel = i;
                    }
                    ClickTarget::DecisionOption(i, o) => {
                        if let Some(d) = self.visible_decisions().get(i) {
                            self.client.answer_decision(&d.id, o);
                        }
                    }
                    ClickTarget::Pr(i) => {
                        self.inbox_col = 1;
                        self.pr_sel = i;
                    }
                    ClickTarget::PaletteItem(i) => {
                        if let Some(it) = self.palette_items().get(i).cloned() {
                            self.palette = None;
                            self.run_cmd(it.cmd);
                        }
                    }
                    ClickTarget::FocusChat => self.focus = Focus::ChatInput,
                }
                ctx.notify();
            }
        }
    }
}

// ------------------------------------------------------------------ rendering helpers

fn label(s: impl Into<String>, family: FamilyId, size: f32, color: ColorU) -> Box<dyn Element> {
    Text::new_inline(s.into(), family, size).with_color(color).finish()
}

fn bold_label(s: impl Into<String>, family: FamilyId, size: f32, color: ColorU) -> Box<dyn Element> {
    Text::new_inline(s.into(), family, size)
        .with_color(color)
        .with_style(Properties::default().weight(Weight::Bold))
        .finish()
}

fn clickable(child: Box<dyn Element>, target: ClickTarget) -> Box<dyn Element> {
    EventHandler::new(child)
        .on_left_mouse_down(move |ctx, _, _| {
            ctx.dispatch_typed_action(AppAction::Click(target.clone()));
            DispatchEventResult::StopPropagation
        })
        .finish()
}

fn panel(child: Box<dyn Element>, bg: ColorU) -> Container {
    Container::new(child).with_background_color(bg)
}

fn radius(px: f32) -> CornerRadius {
    CornerRadius::with_all(Radius::Pixels(px))
}

fn chip(s: &str, family: FamilyId, fg: ColorU) -> Box<dyn Element> {
    Container::new(label(s, family, 10.5, fg))
        .with_horizontal_padding(5.)
        .with_vertical_padding(1.)
        .with_background_color(ColorU::new(fg.r, fg.g, fg.b, 40))
        .with_corner_radius(radius(3.))
        .finish()
}

fn rich_text(r: &Rich, family: FamilyId, mono: FamilyId, size: f32, color: ColorU) -> Box<dyn Element> {
    let _ = mono;
    let mut hs: Vec<HighlightedRange> = r
        .spans
        .iter()
        .map(|(range, st)| {
            let mut props = Properties::default();
            if st.bold {
                props = props.weight(Weight::Bold);
            }
            if st.italic {
                props = props.style(Style::Italic);
            }
            let mut ts = TextStyle::new();
            if st.code {
                ts = ts.with_foreground_color(c(0xe5c07b)).with_background_color(c(0x262a33));
            } else if st.link {
                ts = ts.with_foreground_color(ACCENT).with_underline_color(ACCENT);
            } else {
                ts = ts.with_foreground_color(color);
            }
            HighlightedRange {
                highlight: Highlight::new().with_properties(props).with_text_style(ts),
                highlight_indices: range.clone().collect(),
            }
        })
        .collect();
    hs.sort_by_key(|h| h.highlight_indices[0]);
    Text::new(r.text.clone(), family, size)
        .with_color(color)
        .with_highlights(hs)
        .finish()
}

type CodeHl = Rc<Vec<(String, Vec<(std::ops::Range<usize>, (u8, u8, u8))>)>>;

thread_local! {
    /// syntect is far too slow to run per frame; cache highlighted code blocks by content.
    static CODE_HL: RefCell<HashMap<(String, String), CodeHl>> = RefCell::new(HashMap::new());
}

fn code_block(hl: &Highlighter, lang: &str, code: &str, mono: MonoFont) -> Box<dyn Element> {
    let key = (lang.to_string(), code.to_string());
    let lines = CODE_HL.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 512 {
            c.clear();
        }
        c.entry(key).or_insert_with(|| Rc::new(hl.highlight_code(lang, code))).clone()
    });
    let mut col = Flex::column();
    for (line, spans) in lines.iter().cloned() {
        let hs: Vec<HighlightedRange> = spans
            .into_iter()
            .map(|(r, (cr, cg, cb))| HighlightedRange {
                highlight: Highlight::new().with_foreground_color(ColorU::new(cr, cg, cb, 255)),
                highlight_indices: r.collect(),
            })
            .collect();
        let text = if line.is_empty() { " ".to_string() } else { line };
        col.add_child(Text::new_inline(text, mono.family, mono.size).with_color(TEXT).with_highlights(hs).finish());
    }
    let header = if lang.is_empty() { Empty::new().finish() } else { Container::new(label(lang, mono.family, 10., DIM)).with_padding_bottom(4.).finish() };
    Container::new(Flex::column().with_child(header).with_child(col.finish()).finish())
        .with_uniform_padding(8.)
        .with_background_color(c(0x11141a))
        .with_border(Border::all(1.).with_border_color(BORDER))
        .with_corner_radius(radius(4.))
        .with_vertical_margin(4.)
        .finish()
}

// ------------------------------------------------------------------ View

/// Bindings registered once at startup. On the web, Ctrl+V never reaches the app as a key event:
/// warpui lets the browser handle it and turns the resulting paste event into
/// `StandardAction::Paste` (macOS menus dispatch it too).
pub fn register_bindings(ctx: &mut warpui::AppContext) {
    ctx.register_fixed_bindings([warpui::keymap::FixedBinding::standard(
        warpui::actions::StandardAction::Paste,
        AppAction::Paste,
        warpui::keymap::macros::id!("QuarkRoot"),
    )]);
}

impl View for RootView {
    fn ui_name() -> &'static str {
        "QuarkRoot"
    }

    fn active_cursor_position(&self, ctx: &ViewContext<Self>) -> Option<CursorInfo> {
        // Lets the platform place the IME candidate window at the caret.
        let has_editor = self.palette.is_some() || matches!(self.focus, Focus::ChatInput | Focus::CommentInput);
        if !has_editor {
            return None;
        }
        ctx.element_position_by_id("editor-caret").map(|r| CursorInfo { position: r, font_size: 13. })
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        let t0 = Instant::now();
        let el = self.render_root();
        PERF.with(|p| p.borrow_mut().last_build_ms += t0.elapsed().as_secs_f64() * 1000.);
        el
    }
}

impl RootView {
    fn render_root(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let body = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(self.render_sidebar())
            .with_child(Expanded::new(1., self.render_main()).finish())
            .finish();
        let root = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(Expanded::new(1., body).finish())
            .with_child(self.render_status())
            .finish();
        let mut stack = Stack::new()
            .with_child(Rect::new().with_background_color(BG).finish())
            .with_child(root);
        if self.show_perf {
            stack.add_positioned_overlay_child(
                self.render_perf(),
                OffsetPositioning::offset_from_parent(vec2f(-12., 40.), ParentOffsetBounds::ParentByPosition, ParentAnchor::TopRight, ChildAnchor::TopRight),
            );
        }
        if self.palette.is_some() {
            stack.add_positioned_overlay_child(
                self.render_palette(),
                OffsetPositioning::offset_from_parent(vec2f(0., 80.), ParentOffsetBounds::ParentByPosition, ParentAnchor::TopMiddle, ChildAnchor::TopMiddle),
            );
        }
        let _ = f;
        FrameProbe::new(KeyCapture::new(stack.finish()).finish()).finish()
    }

    fn render_sidebar(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let mut col = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        col.add_child(
            Container::new(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(ConstrainedBox::new(Rect::new().with_background_color(ACCENT).with_corner_radius(radius(3.)).finish()).with_width(14.).with_height(14.).finish())
                    .with_child(bold_label("Quark", f.ui, 15., TEXT))
                    .finish(),
            )
            .with_uniform_padding(12.)
            .finish(),
        );
        col.add_child(Container::new(label("PROJECTS", f.ui, 10., DIM)).with_horizontal_padding(12.).with_vertical_padding(4.).finish());
        for (i, p) in self.projects.iter().enumerate() {
            let sel = i == self.sel_project;
            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1., label(&p.name, f.ui, 12.5, if sel { TEXT } else { MUTED })).finish())
                .with_child(chip(&p.active_tasks.to_string(), f.ui, if p.active_tasks > 0 { ACCENT } else { DIM }))
                .finish();
            let mut cont = Container::new(row).with_horizontal_padding(12.).with_vertical_padding(5.);
            if sel {
                cont = cont.with_background_color(SEL_BG).with_border(Border::left(2.).with_border_color(ACCENT));
            }
            col.add_child(clickable(cont.finish(), ClickTarget::Project(i)));
        }
        if self.projects.is_empty() {
            col.add_child(Container::new(label(if self.connected { "loading…" } else { "daemon offline" }, f.ui, 12., DIM)).with_uniform_padding(12.).finish());
        }
        col.add_child(Container::new(label("SCREENS", f.ui, 10., DIM)).with_horizontal_padding(12.).with_padding_top(16.).with_padding_bottom(4.).finish());
        for (i, (s, name)) in SCREENS.iter().enumerate() {
            let sel = *s == self.screen;
            let mut badge = String::new();
            if *s == Screen::Inbox {
                let open = self.decisions.iter().filter(|d| d.state == "open").count();
                if open > 0 {
                    badge = open.to_string();
                }
            }
            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_child(label(*name, f.ui, 12.5, if sel { TEXT } else { MUTED }))
                .with_child(
                    Flex::row()
                        .with_spacing(4.)
                        .with_child(if badge.is_empty() { Empty::new().finish() } else { chip(&badge, f.ui, YELLOW) })
                        .with_child(label(format!("^{}", i + 1), f.mono.family, 10.5, DIM))
                        .finish(),
                )
                .finish();
            let mut cont = Container::new(row).with_horizontal_padding(12.).with_vertical_padding(5.);
            if sel {
                cont = cont.with_background_color(SEL_BG).with_border(Border::left(2.).with_border_color(ACCENT));
            }
            col.add_child(clickable(cont.finish(), ClickTarget::Screen(i)));
        }
        col.add_child(Expanded::new(1., Empty::new().finish()).finish());
        col.add_child(
            Container::new(Text::new("Ctrl+K  command palette\nF2  perf overlay\nAlt+↑/↓  switch project", f.ui, 10.5).with_color(DIM).finish())
                .with_uniform_padding(12.)
                .finish(),
        );
        ConstrainedBox::new(
            panel(col.finish(), PANEL).with_border(Border::right(1.).with_border_color(BORDER)).finish(),
        )
        .with_width(210.)
        .finish()
    }

    fn render_status(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let seq = self.client.last_seq.load(std::sync::atomic::Ordering::Relaxed);
        let conn = if self.connected { ("● connected", GREEN) } else { ("● reconnecting", RED) };
        let mut row = Flex::row()
            .with_spacing(16.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(label(conn.0, f.ui, 11., conn.1))
            .with_child(label(format!("seq {seq}"), f.mono.family, 11., MUTED))
            .with_child(label(format!("{} workers", self.panes.len()), f.ui, 11., MUTED));
        if self.stress {
            row.add_child(chip("STRESS", f.ui, RED));
        }
        if let Some(e) = &self.last_error {
            row.add_child(Shrinkable::new(1., label(format!("last error: {e}"), f.ui, 11., RED)).finish());
        }
        ConstrainedBox::new(
            Container::new(row.finish())
                .with_horizontal_padding(12.)
                .with_vertical_padding(4.)
                .with_background_color(PANEL)
                .with_border(Border::top(1.).with_border_color(BORDER))
                .finish(),
        )
        .with_height(24.)
        .finish()
    }

    fn render_main(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let title = SCREENS.iter().find(|(s, _)| *s == self.screen).map(|(_, n)| *n).unwrap_or("");
        let proj = self.projects.get(self.sel_project).map(|p| p.name.clone()).unwrap_or_default();
        let header = Container::new(
            Flex::row()
                .with_spacing(10.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(bold_label(title, f.ui, 14., TEXT))
                .with_child(label(format!("· {proj}"), f.ui, 12.5, MUTED))
                .with_child(label(self.screen_hint(), f.ui, 11., DIM))
                .finish(),
        )
        .with_horizontal_padding(14.)
        .with_vertical_padding(9.)
        .with_border(Border::bottom(1.).with_border_color(BORDER))
        .finish();
        let content = match self.screen {
            Screen::Board => self.render_board(),
            Screen::Terminals => self.render_terminals(),
            Screen::Chat => self.render_chat(false),
            Screen::Diff => self.render_diff(),
            Screen::Inbox => self.render_inbox(),
        };
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(Expanded::new(1., content).finish())
            .finish()
    }

    fn screen_hint(&self) -> &'static str {
        match self.screen {
            Screen::Board => "h/j/k/l move · [ ] project",
            Screen::Terminals => "click to focus · Alt+1..4 pane · Ctrl+Shift+C/V copy/paste · Ctrl+J chat dock · Ctrl+L chat input",
            Screen::Chat => "Enter send · ↑/↓ scroll · drag to select, Ctrl+C copy",
            Screen::Diff => "j/k move · Enter/c or click: comment · [ ] PR",
            Screen::Inbox => "j/k · Tab switch list · 1-9 answer · Enter recommended / open PR",
        }
    }

    // ---------------------------------------------------------- 1. board

    fn render_board(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let pid = self.project_id();
        let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(8.);
        for (ci, s) in STATES.iter().enumerate() {
            let tasks: Vec<&Task> = self.tasks.iter().filter(|t| Some(&t.project_id) == pid.as_ref() && t.state == *s).collect();
            let mut col = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(6.);
            col.add_child(
                Container::new(
                    Flex::row()
                        .with_spacing(6.)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(ConstrainedBox::new(Rect::new().with_background_color(state_color(s)).with_corner_radius(radius(4.)).finish()).with_width(8.).with_height(8.).finish())
                        .with_child(bold_label(s.replace('_', " ").to_uppercase(), f.ui, 10.5, MUTED))
                        .with_child(label(tasks.len().to_string(), f.ui, 10.5, DIM))
                        .finish(),
                )
                .with_padding_bottom(4.)
                .finish(),
            );
            for (ri, t) in tasks.iter().enumerate() {
                let sel = self.board_sel == (ci, ri);
                let card = Container::new(
                    Flex::column()
                        .with_spacing(4.)
                        .with_child(Text::new(t.title.clone(), f.ui, 12.5).with_color(TEXT).finish())
                        .with_child(
                            Flex::row()
                                .with_spacing(6.)
                                .with_child(chip(&t.harness, f.ui, CYAN))
                                .with_child(Shrinkable::new(1., label(&t.branch, f.mono.family, 10.5, DIM)).finish())
                                .finish(),
                        )
                        .finish(),
                )
                .with_uniform_padding(8.)
                .with_background_color(if sel { SEL_BG } else { PANEL2 })
                .with_border(Border::all(1.).with_border_color(if sel { ACCENT } else { BORDER }))
                .with_corner_radius(radius(5.))
                .finish();
                col.add_child(clickable(card, ClickTarget::Task(t.id.clone())));
            }
            row.add_child(Expanded::new(1., Container::new(col.finish()).with_uniform_padding(4.).finish()).finish());
        }
        Container::new(row.finish()).with_uniform_padding(10.).finish()
    }

    // ---------------------------------------------------------- 2. terminals

    fn render_terminals(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let pane = |i: usize| -> Box<dyn Element> {
            match self.panes.get(i) {
                Some(p) => {
                    let focused = i == self.focused_pane && self.focus == Focus::Terminal;
                    let title = Container::new(
                        Flex::row()
                            .with_spacing(8.)
                            .with_child(label(format!("{}", i + 1), f.mono.family, 11., DIM))
                            .with_child(bold_label(&p.worker.title, f.ui, 11.5, if focused { TEXT } else { MUTED }))
                            .with_child(label(p.worker.task_id.clone().unwrap_or_default(), f.mono.family, 10.5, DIM))
                            .with_child(label({ let s = p.snap.borrow(); format!("{}×{}", s.cols, s.rows) }, f.mono.family, 10.5, DIM))
                            .finish(),
                    )
                    .with_horizontal_padding(8.)
                    .with_vertical_padding(4.)
                    .with_background_color(if focused { c(0x1d2533) } else { PANEL })
                    .finish();
                    let grid = TermGrid::new(i, p.snap.clone(), f.mono, p.grid.clone(), p.sel, focused).finish();
                    Container::new(
                        Flex::column()
                            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                            .with_child(title)
                            .with_child(Expanded::new(1., Container::new(grid).with_uniform_padding(4.).with_background_color(c(0x101216)).finish()).finish())
                            .finish(),
                    )
                    .with_border(Border::all(1.).with_border_color(if focused { ACCENT } else { BORDER }))
                    .finish()
                }
                None => Container::new(label(if self.workers_known { "no worker" } else { "waiting for workers…" }, f.ui, 12., DIM))
                    .with_uniform_padding(12.)
                    .with_border(Border::all(1.).with_border_color(BORDER))
                    .finish(),
            }
        };
        let row = |a: usize, b: usize| {
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(6.)
                .with_child(Expanded::new(1., pane(a)).finish())
                .with_child(Expanded::new(1., pane(b)).finish())
                .finish()
        };
        let grid = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(6.)
            .with_child(Expanded::new(1., row(0, 1)).finish())
            .with_child(Expanded::new(1., row(2, 3)).finish())
            .finish();
        let mut main = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(6.);
        main.add_child(Expanded::new(1., grid).finish());
        if self.chat_dock {
            main.add_child(
                ConstrainedBox::new(
                    Container::new(self.render_chat(true)).with_border(Border::all(1.).with_border_color(BORDER)).finish(),
                )
                .with_width(380.)
                .finish(),
            );
        }
        Container::new(main.finish()).with_uniform_padding(6.).finish()
    }

    // ---------------------------------------------------------- 3. chat

    fn render_chat(&self, docked: bool) -> Box<dyn Element> {
        let f = self.fonts;
        let pid = self.project_id().unwrap_or_default();
        let empty = vec![];
        let msgs = self.chats.get(&pid).unwrap_or(&empty);
        let mut col = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(10.);
        for m in msgs {
            let is_user = m.role == "user";
            let mut body = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(4.);
            body.add_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_child(bold_label(if is_user { "you" } else { "coordinator" }, f.ui, 11., if is_user { CYAN } else { PURPLE }))
                    .with_child(if m.streaming { label("streaming…", f.ui, 10.5, DIM) } else { Empty::new().finish() })
                    .finish(),
            );
            for b in m.blocks.iter() {
                body.add_child(match b {
                    Block::Para(r) => rich_text(r, f.ui, f.mono.family, 13., TEXT),
                    Block::Heading(l, r) => {
                        let size = match l { 1 => 17., 2 => 15., _ => 13.5 };
                        let mut r2 = r.clone();
                        r2.spans.push((0..r.text.chars().count(), markdown::SpanStyle { bold: true, ..Default::default() }));
                        Container::new(rich_text(&r2, f.ui, f.mono.family, size, TEXT)).with_padding_top(4.).finish()
                    }
                    Block::Item(depth, num, r) => Flex::row()
                        .with_child(
                            ConstrainedBox::new(label(match num { Some(n) => format!("{n}."), None => "•".into() }, f.ui, 13., MUTED))
                                .with_width(14. + 14. * (*depth as f32))
                                .finish(),
                        )
                        .with_child(Shrinkable::new(1., rich_text(r, f.ui, f.mono.family, 13., TEXT)).finish())
                        .finish(),
                    Block::Quote(r) => Container::new(rich_text(r, f.ui, f.mono.family, 13., MUTED))
                        .with_padding_left(10.)
                        .with_border(Border::left(2.).with_border_color(BORDER))
                        .finish(),
                    Block::Code(lang, code) => code_block(&self.hl, lang, code, f.mono),
                    Block::Rule => ConstrainedBox::new(Rect::new().with_background_color(BORDER).finish()).with_height(1.).finish(),
                    Block::Table(rows) => {
                        let mut t = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
                        for (ri, row) in rows.iter().enumerate() {
                            let mut r = Flex::row();
                            for cell in row {
                                let mut cell = cell.clone();
                                if ri == 0 {
                                    cell.spans.push((0..cell.text.chars().count(), markdown::SpanStyle { bold: true, ..Default::default() }));
                                }
                                r.add_child(Expanded::new(1., Container::new(rich_text(&cell, f.ui, f.mono.family, 12.5, TEXT)).with_horizontal_padding(8.).with_vertical_padding(4.).finish()).finish());
                            }
                            t.add_child(
                                Container::new(r.finish())
                                    .with_background_color(if ri == 0 { c(0x222733) } else { PANEL2 })
                                    .with_border(Border::bottom(1.).with_border_color(BORDER))
                                    .finish(),
                            );
                        }
                        Container::new(t.finish()).with_border(Border::all(1.).with_border_color(BORDER)).with_vertical_margin(4.).finish()
                    }
                });
            }
            col.add_child(
                Container::new(body.finish())
                    .with_uniform_padding(10.)
                    .with_background_color(if is_user { c(0x18202c) } else { PANEL2 })
                    .with_corner_radius(radius(6.))
                    .finish(),
            );
        }
        col.add_child(SavePosition::new(ConstrainedBox::new(Empty::new().finish()).with_height(1.).finish(), "chat-bottom").finish());
        let selected = self.chat_selected.clone();
        let _ = selected;
        let selectable = SelectableArea::new(
            self.chat_sel.clone(),
            |args, ctx, _| {
                ctx.dispatch_typed_action(AppAction::ChatSelection(args.selection));
            },
            Container::new(col.finish()).with_uniform_padding(if docked { 8. } else { 14. }).finish(),
        )
        .finish();
        let scroll = ClippedScrollable::vertical(
            self.chat_scroll.clone(),
            selectable,
            ScrollbarWidth::Auto,
            BORDER.into(),
            MUTED.into(),
            warpui::elements::Fill::None,
        )
        .finish();
        let focused = self.focus == Focus::ChatInput && self.palette.is_none();
        let input = clickable(
            Container::new(EditorLine::new(self.chat_input.clone(), f.ui, 13., focused, ED_CHAT, "Message the coordinator…  (Enter to send)").finish())
                .with_horizontal_padding(10.)
                .with_vertical_padding(7.)
                .with_background_color(PANEL2)
                .with_border(Border::all(1.).with_border_color(if focused { ACCENT } else { BORDER }))
                .with_corner_radius(radius(6.))
                .finish(),
            ClickTarget::FocusChat,
        );
        let mut colm = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        if docked {
            colm.add_child(Container::new(bold_label("Coordinator", f.ui, 11.5, MUTED)).with_horizontal_padding(8.).with_vertical_padding(4.).with_background_color(PANEL).finish());
        }
        colm.add_child(Expanded::new(1., scroll).finish());
        colm.add_child(Container::new(input).with_uniform_padding(if docked { 6. } else { 12. }).finish());
        colm.finish()
    }

    // ---------------------------------------------------------- 4. diff

    fn render_diff(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let pr = self.diff_pr.as_ref().and_then(|id| self.prs.iter().find(|p| &p.id == id));
        let head = match pr {
            Some(p) => Flex::row()
                .with_spacing(10.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(bold_label(format!("#{} {}", p.number, p.title), f.ui, 13., TEXT))
                .with_child(chip(&p.state, f.ui, if p.state == "merged" { PURPLE } else { GREEN }))
                .with_child(chip(&format!("checks {}", p.checks), f.ui, match p.checks.as_str() { "passing" => GREEN, "failing" => RED, _ => YELLOW }))
                .with_child(label(format!("+{} −{}", p.additions, p.deletions), f.mono.family, 11., MUTED))
                .with_child(label(format!("{} comments", self.comments.len()), f.ui, 11., MUTED))
                .finish(),
            None => label("No PR selected — pick one in Decisions & PRs or Ctrl+K", f.ui, 12., DIM),
        };
        let mono = f.mono;
        let mut col = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (i, l) in self.diff_lines.iter().enumerate() {
            let (bg, sign, sign_col) = match l.kind {
                Kind::Add => (c(0x16271c), "+", GREEN),
                Kind::Del => (c(0x2c1719), "−", RED),
                Kind::Hunk => (c(0x172033), "", CYAN),
                Kind::FileHeader => (c(0x1f2430), "", TEXT),
                Kind::Meta => (BG, "", DIM),
                Kind::Ctx => (BG, "", TEXT),
            };
            let bg = if i == self.diff_cursor { c(0x26324a) } else { bg };
            let num = |n: Option<i64>| n.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
            let content: Box<dyn Element> = match l.kind {
                Kind::FileHeader => bold_label(format!("▸ {}", l.text), mono.family, mono.size, TEXT),
                Kind::Hunk | Kind::Meta => label(&l.text, mono.family, mono.size, sign_col),
                _ => {
                    let has_tab = l.text.contains('\t');
                    let hs: Vec<HighlightedRange> = if has_tab {
                        vec![]
                    } else {
                        l.spans
                            .iter()
                            .map(|(r, (cr, cg, cb))| HighlightedRange {
                                highlight: Highlight::new().with_foreground_color(ColorU::new(*cr, *cg, *cb, 255)),
                                highlight_indices: r.clone().collect(),
                            })
                            .collect()
                    };
                    let t = if l.text.is_empty() { " ".to_string() } else { l.text.replace('\t', "    ") };
                    Text::new_inline(t, mono.family, mono.size).with_color(TEXT).with_highlights(hs).finish()
                }
            };
            let code_line = matches!(l.kind, Kind::Add | Kind::Ctx | Kind::Del);
            let ln = l.new_no.or(l.old_no);
            let here: Vec<&Comment> = if code_line {
                self.comments.iter().filter(|cm| cm.path == l.path && Some(cm.line) == ln).collect()
            } else {
                vec![]
            };
            let line_row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(label(format!("{} {} ", num(l.old_no), num(l.new_no)), mono.family, mono.size, DIM))
                .with_child(ConstrainedBox::new(label(sign, mono.family, mono.size, sign_col)).with_width(14.).finish())
                .with_child(Shrinkable::new(1., content).finish())
                .finish();
            let el = ConstrainedBox::new(Container::new(line_row).with_background_color(bg).with_horizontal_padding(6.).finish())
                .with_height(mono.line_h)
                .finish();
            let el = clickable(el, ClickTarget::DiffLine(i));
            col.add_child(if i == self.diff_cursor { SavePosition::new(el, "diff-cursor").finish() } else { el });
            // Inline thread + composer under the line.
            if !here.is_empty() || self.commenting == Some(i) {
                let mut thread = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(4.);
                for cm in &here {
                    thread.add_child(
                        Container::new(
                            Flex::column()
                                .with_child(bold_label(&cm.author, f.ui, 11., CYAN))
                                .with_child(Text::new(cm.body.clone(), f.ui, 12.5).with_color(TEXT).finish())
                                .finish(),
                        )
                        .with_uniform_padding(6.)
                        .with_background_color(PANEL2)
                        .with_border(Border::all(1.).with_border_color(BORDER))
                        .with_corner_radius(radius(4.))
                        .finish(),
                    );
                }
                if self.commenting == Some(i) {
                    let focused = self.focus == Focus::CommentInput && self.palette.is_none();
                    thread.add_child(
                        Container::new(EditorLine::new(self.comment_input.clone(), f.ui, 13., focused, ED_COMMENT, "Write a line comment… (Enter to post, Esc to cancel)").finish())
                            .with_horizontal_padding(8.)
                            .with_vertical_padding(6.)
                            .with_background_color(PANEL2)
                            .with_border(Border::all(1.).with_border_color(if focused { ACCENT } else { BORDER }))
                            .with_corner_radius(radius(5.))
                            .finish(),
                    );
                }
                col.add_child(
                    Container::new(thread.finish())
                        .with_padding_left(90.)
                        .with_padding_right(16.)
                        .with_vertical_padding(6.)
                        .with_background_color(c(0x13161c))
                        .finish(),
                );
            }
        }
        let body: Box<dyn Element> = if self.diff_lines.is_empty() {
            Container::new(label(if self.diff_pr.is_some() { "loading diff…" } else { "" }, f.ui, 12., DIM)).with_uniform_padding(14.).finish()
        } else {
            ClippedScrollable::vertical(
                self.diff_scroll.clone(),
                col.finish(),
                ScrollbarWidth::Auto,
                BORDER.into(),
                MUTED.into(),
                warpui::elements::Fill::None,
            )
            .finish()
        };
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(Container::new(head).with_horizontal_padding(14.).with_vertical_padding(8.).finish())
            .with_child(Expanded::new(1., Container::new(body).with_border(Border::top(1.).with_border_color(BORDER)).finish()).finish())
            .finish()
    }

    // ---------------------------------------------------------- 5. decisions + PRs

    fn render_inbox(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let decs = self.visible_decisions();
        let mut dcol = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(6.);
        dcol.add_child(bold_label(format!("DECISIONS · {} open", decs.iter().filter(|d| d.state == "open").count()), f.ui, 10.5, if self.inbox_col == 0 { TEXT } else { MUTED }));
        for (i, d) in decs.iter().enumerate() {
            let sel = i == self.dec_sel && self.inbox_col == 0;
            let open = d.state == "open";
            let mut body = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(4.);
            body.add_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_child(chip(if open { "open" } else { "answered" }, f.ui, if open { YELLOW } else { GREEN }))
                    .with_child(chip(&d.project_id, f.ui, MUTED))
                    .with_child(Shrinkable::new(1., Text::new(d.question.clone(), f.ui, 12.5).with_color(if open { TEXT } else { MUTED }).finish()).finish())
                    .finish(),
            );
            if sel && !d.context.is_empty() {
                body.add_child(Text::new(d.context.clone(), f.ui, 11.5).with_color(MUTED).finish());
            }
            if sel {
                for (oi, o) in d.options.iter().enumerate() {
                    let rec = d.recommended == Some(oi);
                    let chosen = d.answer == Some(oi);
                    let row = Flex::row()
                        .with_spacing(8.)
                        .with_child(label(format!("{}", oi + 1), f.mono.family, 11., if sel { ACCENT } else { DIM }))
                        .with_child(label(&o.label, f.ui, 12., if chosen { GREEN } else { TEXT }))
                        .with_child(if rec { chip("recommended", f.ui, ACCENT) } else { Empty::new().finish() })
                        .with_child(Shrinkable::new(1., label(&o.consequence, f.ui, 11., DIM)).finish())
                        .finish();
                    body.add_child(if open { clickable(Container::new(row).with_padding_left(6.).finish(), ClickTarget::DecisionOption(i, oi)) } else { Container::new(row).with_padding_left(6.).finish() });
                }
            }
            if !sel && !open {
                if let Some(a) = d.answer.and_then(|a| d.options.get(a)) {
                    body.add_child(label(format!("→ {}", a.label), f.ui, 11.5, GREEN));
                }
            }
            let card = clickable(
                Container::new(body.finish())
                    .with_uniform_padding(8.)
                    .with_background_color(if sel { SEL_BG } else { PANEL2 })
                    .with_border(Border::all(1.).with_border_color(if sel { ACCENT } else { BORDER }))
                    .with_corner_radius(radius(5.))
                    .finish(),
                ClickTarget::Decision(i),
            );
            dcol.add_child(if sel { SavePosition::new(card, "dec-sel").finish() } else { card });
        }
        let mut pcol = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch).with_spacing(4.);
        pcol.add_child(bold_label(format!("PULL REQUESTS · {}", self.prs.len()), f.ui, 10.5, if self.inbox_col == 1 { TEXT } else { MUTED }));
        for (i, p) in self.prs.iter().enumerate() {
            let sel = i == self.pr_sel && self.inbox_col == 1;
            let risk = match &p.risk {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                v => v.to_string(),
            };
            let row = Flex::row()
                .with_spacing(8.)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(label(format!("#{}", p.number), f.mono.family, 11.5, MUTED))
                .with_child(Shrinkable::new(1., label(&p.title, f.ui, 12.5, TEXT)).finish())
                .with_child(chip(&p.state, f.ui, match p.state.as_str() { "merged" => PURPLE, "draft" => MUTED, _ => GREEN }))
                .with_child(chip(&p.checks, f.ui, match p.checks.as_str() { "passing" => GREEN, "failing" => RED, _ => YELLOW }))
                .with_child(if risk.is_empty() { Empty::new().finish() } else { chip(&format!("risk {risk}"), f.ui, match risk.as_str() { "high" => RED, "medium" => YELLOW, _ => MUTED }) })
                .with_child(label(format!("+{} −{}", p.additions, p.deletions), f.mono.family, 11., DIM))
                .finish();
            pcol.add_child(clickable(
                Container::new(row)
                    .with_horizontal_padding(8.)
                    .with_vertical_padding(6.)
                    .with_background_color(if sel { SEL_BG } else { PANEL2 })
                    .with_border(Border::all(1.).with_border_color(if sel { ACCENT } else { BORDER }))
                    .with_corner_radius(radius(4.))
                    .finish(),
                ClickTarget::Pr(i),
            ));
        }
        Container::new(
            Flex::row()
                .with_spacing(12.)
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(Expanded::new(1., ClippedScrollable::vertical(self.dec_scroll.clone(), dcol.finish(), ScrollbarWidth::Auto, BORDER.into(), MUTED.into(), warpui::elements::Fill::None).finish()).finish())
                .with_child(Expanded::new(1., ClippedScrollable::vertical(self.pr_scroll.clone(), pcol.finish(), ScrollbarWidth::Auto, BORDER.into(), MUTED.into(), warpui::elements::Fill::None).finish()).finish())
                .finish(),
        )
        .with_uniform_padding(12.)
        .finish()
    }

    // ---------------------------------------------------------- overlays

    fn render_palette(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let p = self.palette.as_ref().unwrap();
        let items = self.palette_items();
        let mut col = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        col.add_child(
            Container::new(EditorLine::new(p.input.clone(), f.ui, 14., true, ED_PALETTE, "Jump to screen, project, PR, or answer a decision…").finish())
                .with_horizontal_padding(12.)
                .with_vertical_padding(10.)
                .with_border(Border::bottom(1.).with_border_color(BORDER))
                .finish(),
        );
        let start = p.sel.saturating_sub(11);
        for (i, it) in items.iter().enumerate().skip(start).take(12) {
            let sel = i == p.sel;
            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_spacing(12.)
                .with_child(Shrinkable::new(3., label(&it.label, f.ui, 12.5, if sel { TEXT } else { MUTED })).finish())
                .with_child(Shrinkable::new(1., label(&it.hint, f.ui, 11., DIM)).finish())
                .finish();
            let mut cont = Container::new(row).with_horizontal_padding(12.).with_vertical_padding(6.);
            if sel {
                cont = cont.with_background_color(SEL_BG);
            }
            col.add_child(clickable(cont.finish(), ClickTarget::PaletteItem(i)));
        }
        if items.is_empty() {
            col.add_child(Container::new(label("no matches", f.ui, 12., DIM)).with_uniform_padding(12.).finish());
        }
        ConstrainedBox::new(
            Container::new(col.finish())
                .with_background_color(c(0x1a1e26))
                .with_border(Border::all(1.).with_border_color(c(0x3a4150)))
                .with_corner_radius(radius(8.))
                .finish(),
        )
        .with_width(620.)
        .finish()
    }

    fn render_perf(&self) -> Box<dyn Element> {
        let f = self.fonts;
        let (fps, p50, p99, lat50, lat99, n, rows, split) = PERF.with(|p| {
            let p = p.borrow();
            let (fps, p50, p99) = p.summary();
            let tail: Vec<f64> = p.key_to_paint_ms.iter().rev().take(200).cloned().collect();
            let last = |v: &Vec<f64>| -> Vec<f64> { v.iter().rev().take(120).cloned().collect() };
            let split = (pct(&last(&p.build_ms), 0.5), pct(&last(&p.scene_ms), 0.5), pct(&last(&p.gpu_ms), 0.5));
            (fps, p50, p99, pct(&tail, 0.5), pct(&tail, 0.99), tail.len(), p.rows_rebuilt, split)
        });
        let fmt = |v: f64| if v.is_nan() { "–".to_string() } else { format!("{v:.1}") };
        let text = format!(
            "fps        {}\nframe p50  {} ms\nframe p99  {} ms\n  p50 split: view {} · layout+paint {} · gpu+present {} ms\nkey→paint  p50 {} / p99 {} ms (n={})\nrows rebuilt {}\n{}",
            fmt(fps),
            fmt(p50),
            fmt(p99),
            fmt(split.0),
            fmt(split.1),
            fmt(split.2),
            fmt(lat50),
            fmt(lat99),
            n,
            rows,
            if self.stress { "STRESS ON" } else { "stress off (Ctrl+Shift+S)" }
        );
        Container::new(Text::new(text, f.mono.family, 11.).with_color(c(0xb5e890)).finish())
            .with_uniform_padding(8.)
            .with_background_color(ColorU::new(0, 0, 0, 200))
            .with_border(Border::all(1.).with_border_color(c(0x3a4150)))
            .with_corner_radius(radius(4.))
            .finish()
    }
}
