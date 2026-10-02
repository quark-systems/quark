//! Custom warpui elements the framework does not provide:
//! - `KeyCapture`: routes raw key / typed-chars / IME events to the root view as actions.
//! - `TermGrid`: paints a terminal snapshot (cell grid) with batched same-style runs.
//! - `EditorLine`: a single-line text input with caret, selection and IME preedit.
//! - `FrameProbe`: timestamps the start of layout for frame-time measurement.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use web_time::Instant;

use warpui::color::ColorU;
use warpui::elements::{Fill, Point};
use warpui::event::DispatchedEvent;
use warpui::fonts::{FamilyId, Properties, Style, Weight};
use warpui::geometry::rect::RectF;
use warpui::geometry::vector::{vec2f, Vector2F};
use warpui::platform::LineStyle;
use warpui::text_layout::{ClipConfig, StyleAndFont, TextStyle, DEFAULT_TOP_BOTTOM_RATIO};
use warpui::{
    AfterLayoutContext, AppContext, Element, Event, EventContext, LayoutContext, PaintContext,
    SizeConstraint,
};

use crate::app::AppAction;
use crate::term::{Rgba, Snapshot, DEFAULT_BG};

fn col(c: Rgba) -> ColorU {
    ColorU::new(c.0, c.1, c.2, 255)
}

pub const NO_WRAP: f32 = 100_000.;

// ---------------------------------------------------------------- KeyCapture

const SPECIAL: &[&str] = &[
    "enter", "tab", "escape", "backspace", "delete", "up", "down", "left", "right", "home", "end",
    "pageup", "pagedown", "insert", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10",
    "f11", "f12",
];

pub struct KeyCapture {
    child: Box<dyn Element>,
    origin: Option<Point>,
}

impl KeyCapture {
    pub fn new(child: Box<dyn Element>) -> Self {
        Self { child, origin: None }
    }
}

impl Element for KeyCapture {
    fn layout(&mut self, c: SizeConstraint, ctx: &mut LayoutContext, app: &AppContext) -> Vector2F {
        self.child.layout(c, ctx, app)
    }
    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app)
    }
    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        self.child.paint(origin, ctx, app)
    }
    fn size(&self) -> Option<Vector2F> {
        self.child.size()
    }
    fn origin(&self) -> Option<Point> {
        self.origin
    }
    fn dispatch_event(&mut self, event: &DispatchedEvent, ctx: &mut EventContext, app: &AppContext) -> bool {
        // Children first (mouse handling etc.).
        let handled = self.child.dispatch_event(event, ctx, app);
        match event.raw_event() {
            Event::KeyDown { keystroke, chars, is_composing, .. } => {
                if *is_composing {
                    return handled;
                }
                let special = SPECIAL.contains(&keystroke.key.as_str());
                let chord = keystroke.ctrl || keystroke.alt || keystroke.cmd || keystroke.meta;
                if special || chord {
                    ctx.dispatch_typed_action(AppAction::Key {
                        key: keystroke.key.clone(),
                        ctrl: keystroke.ctrl || keystroke.cmd,
                        alt: keystroke.alt || keystroke.meta,
                        shift: keystroke.shift,
                        chars: chars.clone(),
                        at: Instant::now(),
                    });
                    return true;
                }
                handled
            }
            Event::TypedCharacters { chars } => {
                ctx.dispatch_typed_action(AppAction::Typed(chars.clone(), Instant::now()));
                true
            }
            Event::SetMarkedText { marked_text, .. } => {
                ctx.dispatch_typed_action(AppAction::Marked(Some(marked_text.clone())));
                true
            }
            Event::ClearMarkedText => {
                ctx.dispatch_typed_action(AppAction::Marked(None));
                true
            }
            _ => handled,
        }
    }
}

// ---------------------------------------------------------------- FrameProbe

thread_local! {
    pub static LAYOUT_START: Cell<Option<Instant>> = const { Cell::new(None) };
    pub static PAINT_END: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Wraps the root: records when layout of a new frame starts.
pub struct FrameProbe {
    child: Box<dyn Element>,
}
impl FrameProbe {
    pub fn new(child: Box<dyn Element>) -> Self {
        Self { child }
    }
}
impl Element for FrameProbe {
    fn layout(&mut self, c: SizeConstraint, ctx: &mut LayoutContext, app: &AppContext) -> Vector2F {
        LAYOUT_START.with(|s| {
            if s.get().is_none() {
                s.set(Some(Instant::now()))
            }
        });
        self.child.layout(c, ctx, app)
    }
    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        self.child.after_layout(ctx, app)
    }
    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.child.paint(origin, ctx, app);
        PAINT_END.with(|s| s.set(Some(Instant::now())));
    }
    fn size(&self) -> Option<Vector2F> {
        self.child.size()
    }
    fn origin(&self) -> Option<Point> {
        self.child.origin()
    }
    fn dispatch_event(&mut self, event: &DispatchedEvent, ctx: &mut EventContext, app: &AppContext) -> bool {
        self.child.dispatch_event(event, ctx, app)
    }
}

// ---------------------------------------------------------------- TermGrid

#[derive(Clone, Copy)]
pub struct MonoFont {
    pub family: FamilyId,
    pub size: f32,
    pub cell_w: f32,
    pub line_h: f32,
}

pub type Sel = ((usize, usize), (usize, usize));

pub struct TermGrid {
    pub pane: usize,
    snap: Rc<RefCell<Snapshot>>,
    font: MonoFont,
    grid_out: Rc<Cell<(usize, usize)>>,
    selection: Option<Sel>,
    focused: bool,
    size: Option<Vector2F>,
    origin: Option<Point>,
    dragging: bool,
}

impl TermGrid {
    pub fn new(
        pane: usize,
        snap: Rc<RefCell<Snapshot>>,
        font: MonoFont,
        grid_out: Rc<Cell<(usize, usize)>>,
        selection: Option<Sel>,
        focused: bool,
    ) -> Self {
        Self { pane, snap, font, grid_out, selection, focused, size: None, origin: None, dragging: false }
    }

    fn cell_at(&self, pos: Vector2F) -> Option<(usize, usize)> {
        let o = self.origin?.xy();
        let s = self.size?;
        let rel = pos - o;
        if rel.x() < 0. || rel.y() < 0. || rel.x() > s.x() || rel.y() > s.y() {
            return None;
        }
        Some(((rel.y() / self.font.line_h) as usize, (rel.x() / self.font.cell_w) as usize))
    }
}

pub fn sel_contains(sel: &Sel, r: usize, c: usize) -> bool {
    let (a, b) = if sel.0 <= sel.1 { (sel.0, sel.1) } else { (sel.1, sel.0) };
    (r, c) >= a && (r, c) <= b
}

impl Element for TermGrid {
    fn layout(&mut self, c: SizeConstraint, _ctx: &mut LayoutContext, _app: &AppContext) -> Vector2F {
        let size = c.max;
        let cols = (size.x() / self.font.cell_w).floor().max(2.) as usize;
        let rows = (size.y() / self.font.line_h).floor().max(1.) as usize;
        self.grid_out.set((cols, rows));
        self.size = Some(size);
        size
    }
    fn after_layout(&mut self, _: &mut AfterLayoutContext, _: &AppContext) {}

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let size = self.size.unwrap();
        ctx.scene
            .draw_rect_without_hit_recording(RectF::new(origin, size))
            .with_background(Fill::Solid(col(DEFAULT_BG)));
        let snap = self.snap.borrow();
        let f = self.font;
        let line_style = LineStyle {
            font_size: f.size,
            line_height_ratio: f.line_h / f.size,
            baseline_ratio: DEFAULT_TOP_BOTTOM_RATIO,
            fixed_width_tab_size: None,
        };
        let tls = app.font_cache().text_layout_system();
        let visible_rows = ((size.y() / f.line_h) as usize).min(snap.rows);
        let visible_cols = ((size.x() / f.cell_w).ceil() as usize).min(snap.cols);
        let mut text = String::with_capacity(snap.cols * 2);
        for r in 0..visible_rows {
            let row = &snap.cells[r * snap.cols..(r + 1) * snap.cols];
            let y = origin.y() + r as f32 * f.line_h;
            // Backgrounds: merge runs of identical non-default bg (plus selection).
            let mut c = 0;
            while c < visible_cols {
                let selected = self.selection.as_ref().is_some_and(|s| sel_contains(s, r, c));
                let bg = if selected { Rgba(0x3a, 0x4f, 0x7a) } else { row[c].bg };
                let start = c;
                c += 1;
                while c < visible_cols {
                    let s2 = self.selection.as_ref().is_some_and(|s| sel_contains(s, r, c));
                    let bg2 = if s2 { Rgba(0x3a, 0x4f, 0x7a) } else { row[c].bg };
                    if bg2 != bg {
                        break;
                    }
                    c += 1;
                }
                if bg != DEFAULT_BG {
                    ctx.scene
                        .draw_rect_without_hit_recording(RectF::new(
                            vec2f(origin.x() + start as f32 * f.cell_w, y),
                            vec2f((c - start) as f32 * f.cell_w, f.line_h),
                        ))
                        .with_background(Fill::Solid(col(bg)));
                }
            }
            // Cursor block under the glyph.
            if let Some((cr, cc)) = snap.cursor
                && cr == r
                && cc < visible_cols
            {
                let rect = RectF::new(vec2f(origin.x() + cc as f32 * f.cell_w, y), vec2f(f.cell_w, f.line_h));
                let fill = if self.focused { ColorU::new(0x7d, 0xc4, 0xff, 200) } else { ColorU::new(0x7d, 0xc4, 0xff, 70) };
                ctx.scene.draw_rect_without_hit_recording(rect).with_background(Fill::Solid(fill));
            }
            // Text: runs of same fg/attrs; wide chars get their own run so the grid stays aligned.
            let mut c = 0;
            while c < visible_cols {
                let cell = row[c];
                if cell.width == 0 || cell.ch == ' ' {
                    c += 1;
                    continue;
                }
                let start = c;
                text.clear();
                text.push(cell.ch);
                c += cell.width.max(1) as usize;
                // Only ASCII is batched: the primary mono font covers it with exact cell advances.
                // Anything else (box drawing, symbols, CJK, emoji) may come from a fallback font
                // with a different advance, so it is placed cell-by-cell to keep the grid aligned.
                if cell.width == 1 && cell.ch.is_ascii() {
                    while c < visible_cols {
                        let n = row[c];
                        if n.width != 1 || !n.ch.is_ascii() || n.fg != cell.fg || n.attrs != cell.attrs {
                            break;
                        }
                        text.push(n.ch);
                        c += 1;
                    }
                }
                let trimmed = text.trim_end();
                let props = Properties::default()
                    .weight(if cell.attrs.bold { Weight::Bold } else { Weight::Normal })
                    .style(if cell.attrs.italic { Style::Italic } else { Style::Normal });
                let mut ts = TextStyle::new().with_foreground_color(col(cell.fg));
                if cell.attrs.underline {
                    ts = ts.with_underline_color(col(cell.fg));
                }
                if cell.attrs.strike {
                    ts = ts.with_show_strikethrough(true);
                }
                let n_chars = trimmed.chars().count();
                let runs = [(0..n_chars, StyleAndFont::new(f.family, props, ts))];
                let line = ctx.text_layout_cache.layout_line(
                    trimmed,
                    line_style,
                    &runs,
                    NO_WRAP,
                    ClipConfig::default(),
                    &tls,
                );
                let x = origin.x() + start as f32 * f.cell_w;
                line.paint(
                    RectF::new(vec2f(x, y), vec2f(NO_WRAP, f.line_h)),
                    &Default::default(),
                    col(cell.fg),
                    app.font_cache(),
                    ctx.scene,
                );
            }
        }
        if self.focused {
            ctx.scene
                .draw_rect_without_hit_recording(RectF::new(origin, size))
                .with_border(warpui::elements::Border::all(1.).with_border_color(ColorU::new(0x4d, 0x9c, 0xff, 255)));
        }
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }
    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(&mut self, event: &DispatchedEvent, ctx: &mut EventContext, _app: &AppContext) -> bool {
        let Some(z) = self.z_index() else { return false };
        match event.at_z_index(z, ctx) {
            Some(Event::LeftMouseDown { position, .. }) => {
                if let Some(cell) = self.cell_at(*position) {
                    self.dragging = true;
                    ctx.dispatch_typed_action(AppAction::TermMouse { pane: self.pane, cell, phase: 0 });
                    return true;
                }
                false
            }
            Some(Event::LeftMouseDragged { position, .. }) => {
                if self.dragging || self.selection.is_some() {
                    if let Some(cell) = self.cell_at(*position) {
                        ctx.dispatch_typed_action(AppAction::TermMouse { pane: self.pane, cell, phase: 1 });
                        return true;
                    }
                }
                false
            }
            Some(Event::LeftMouseUp { position, .. }) => {
                if self.dragging {
                    self.dragging = false;
                    let cell = self.cell_at(*position).unwrap_or((0, 0));
                    ctx.dispatch_typed_action(AppAction::TermMouse { pane: self.pane, cell, phase: 2 });
                    return true;
                }
                false
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------- EditorLine

/// Minimal editable text buffer (char-indexed). Lives in the view; rendered by [`EditorLine`].
#[derive(Clone, Default, Debug)]
pub struct TextBuf {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub marked: Option<String>,
}

impl TextBuf {
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }
    fn byte(&self, ci: usize) -> usize {
        self.text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(self.text.len())
    }
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some((a.min(self.cursor), a.max(self.cursor)))
    }
    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        Some(self.text[self.byte(a)..self.byte(b)].to_string())
    }
    pub fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            let (ba, bb) = (self.byte(a), self.byte(b));
            self.text.replace_range(ba..bb, "");
            self.cursor = a;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let b = self.byte(self.cursor);
        self.text.insert_str(b, s);
        self.cursor += s.chars().count();
    }
    pub fn backspace(&mut self) {
        if self.delete_selection() || self.cursor == 0 {
            return;
        }
        let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
        self.text.replace_range(a..b, "");
        self.cursor -= 1;
    }
    pub fn delete(&mut self) {
        if self.delete_selection() || self.cursor >= self.len() {
            return;
        }
        let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
        self.text.replace_range(a..b, "");
    }
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = pos.min(self.len());
    }
    pub fn word_left(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }
    pub fn word_right(&self) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len();
    }
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        self.anchor = None;
        std::mem::take(&mut self.text)
    }
    /// Handles editing keys. Returns true if consumed.
    pub fn handle_key(&mut self, key: &str, ctrl: bool, shift: bool) -> bool {
        match key {
            "backspace" => self.backspace(),
            "delete" => self.delete(),
            "left" => {
                let p = if ctrl { self.word_left() } else { self.cursor.saturating_sub(1) };
                self.move_to(p, shift)
            }
            "right" => {
                let p = if ctrl { self.word_right() } else { self.cursor + 1 };
                self.move_to(p, shift)
            }
            "home" => self.move_to(0, shift),
            "end" => self.move_to(usize::MAX, shift),
            "a" if ctrl => self.select_all(),
            _ => return false,
        }
        true
    }
}

pub struct EditorLine {
    buf: TextBuf,
    family: FamilyId,
    font_size: f32,
    color: ColorU,
    placeholder: &'static str,
    focused: bool,
    /// Identifies the editor in click actions.
    id: u8,
    caret_id: &'static str,
    size: Option<Vector2F>,
    origin: Option<Point>,
    line: Option<std::sync::Arc<warpui::text_layout::Line>>,
}

impl EditorLine {
    pub fn new(buf: TextBuf, family: FamilyId, font_size: f32, focused: bool, id: u8, placeholder: &'static str) -> Self {
        Self {
            buf,
            family,
            font_size,
            color: ColorU::new(0xe6, 0xe8, 0xec, 255),
            placeholder,
            focused,
            id,
            caret_id: "editor-caret",
            size: None,
            origin: None,
            line: None,
        }
    }
    fn line_h(&self) -> f32 {
        self.font_size * 1.4
    }
}

impl Element for EditorLine {
    fn layout(&mut self, c: SizeConstraint, ctx: &mut LayoutContext, app: &AppContext) -> Vector2F {
        // Display text = text with marked (IME preedit) text spliced in at the caret.
        let mut display = self.buf.text.clone();
        let mut runs = Vec::new();
        let base = StyleAndFont::new(self.family, Properties::default(), TextStyle::new().with_foreground_color(self.color));
        let total;
        if let Some(m) = &self.buf.marked {
            let b = self.buf.text.char_indices().nth(self.buf.cursor).map(|(b, _)| b).unwrap_or(self.buf.text.len());
            display.insert_str(b, m);
            let mlen = m.chars().count();
            total = display.chars().count();
            runs.push((0..self.buf.cursor, base));
            runs.push((
                self.buf.cursor..self.buf.cursor + mlen,
                StyleAndFont::new(
                    self.family,
                    Properties::default(),
                    TextStyle::new().with_foreground_color(self.color).with_underline_color(ColorU::new(0xe5, 0xc0, 0x7b, 255)),
                ),
            ));
            runs.push((self.buf.cursor + mlen..total, base));
        } else if display.is_empty() {
            display = self.placeholder.to_string();
            total = display.chars().count();
            runs.push((
                0..total,
                StyleAndFont::new(self.family, Properties::default(), TextStyle::new().with_foreground_color(ColorU::new(0x6b, 0x71, 0x7d, 255))),
            ));
        } else {
            total = display.chars().count();
            runs.push((0..total, base));
        }
        let line = ctx.text_layout_cache.layout_line(
            &display,
            LineStyle { font_size: self.font_size, line_height_ratio: 1.4, baseline_ratio: DEFAULT_TOP_BOTTOM_RATIO, fixed_width_tab_size: None },
            &runs,
            NO_WRAP,
            ClipConfig::default(),
            &app.font_cache().text_layout_system(),
        );
        self.line = Some(line);
        let size = vec2f(c.max.x().max(c.min.x()), self.line_h());
        self.size = Some(size);
        size
    }
    fn after_layout(&mut self, _: &mut AfterLayoutContext, _: &AppContext) {}
    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let size = self.size.unwrap();
        let line = self.line.clone().unwrap();
        let empty = self.buf.text.is_empty() && self.buf.marked.is_none();
        let caret_idx = self.buf.cursor + self.buf.marked.as_ref().map(|m| m.chars().count()).unwrap_or(0);
        let caret_x = if empty { 0. } else { line.caret_position_for_index(caret_idx) };
        // Scroll horizontally so the caret stays visible.
        let scroll = (caret_x - size.x() + 4.).max(0.);
        ctx.scene.start_layer(warpui::ClipBounds::BoundedBy(RectF::new(origin, size)));
        if let Some((a, b)) = self.buf.selection() {
            let xa = line.caret_position_for_index(a);
            let xb = line.caret_position_for_index(b);
            ctx.scene
                .draw_rect_without_hit_recording(RectF::new(vec2f(origin.x() + xa - scroll, origin.y()), vec2f(xb - xa, self.line_h())))
                .with_background(Fill::Solid(ColorU::new(0x3a, 0x4f, 0x7a, 255)));
        }
        line.paint(
            RectF::new(origin - vec2f(scroll, 0.), vec2f(NO_WRAP, self.line_h())),
            &Default::default(),
            self.color,
            app.font_cache(),
            ctx.scene,
        );
        let caret = RectF::new(vec2f(origin.x() + caret_x - scroll, origin.y() + 2.), vec2f(1.5, self.line_h() - 4.));
        if self.focused {
            ctx.scene
                .draw_rect_without_hit_recording(caret)
                .with_background(Fill::Solid(ColorU::new(0x7d, 0xc4, 0xff, 255)));
            ctx.position_cache.cache_position_indefinitely(self.caret_id.to_string(), caret);
        }
        ctx.scene.stop_layer();
    }
    fn size(&self) -> Option<Vector2F> {
        self.size
    }
    fn origin(&self) -> Option<Point> {
        self.origin
    }
    fn dispatch_event(&mut self, event: &DispatchedEvent, ctx: &mut EventContext, _app: &AppContext) -> bool {
        let Some(z) = self.z_index() else { return false };
        let (Some(o), Some(s), Some(line)) = (self.origin, self.size, self.line.as_ref()) else { return false };
        let hit = |p: &Vector2F| RectF::new(o.xy(), s).contains_point(*p);
        match event.at_z_index(z, ctx) {
            Some(Event::LeftMouseDown { position, .. }) if hit(position) => {
                let idx = if self.buf.text.is_empty() { 0 } else { line.caret_index_for_x_unbounded(position.x() - o.x()) };
                ctx.dispatch_typed_action(AppAction::EditorClick { id: self.id, index: idx, drag: false });
                true
            }
            Some(Event::LeftMouseDragged { position, .. }) if hit(position) => {
                let idx = line.caret_index_for_x_unbounded(position.x() - o.x());
                ctx.dispatch_typed_action(AppAction::EditorClick { id: self.id, index: idx, drag: true });
                true
            }
            _ => false,
        }
    }
}
