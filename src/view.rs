//! Document state: lazy layout, scrolling, hit testing, selection, search and
//! navigation. Only blocks near the viewport are shaped; the rest are height
//! estimates, corrected without visible jumps as they come into range.

use crate::doc::{self, BOLD, CODE, Doc, ITALIC, Kind, LINK, NO_LINK, Span, TOKEN_SHIFT};
use crate::fonts::{Fonts, MONO, SANS};
use crate::highlight;
use crate::images::Images;
use cosmic_text::{Align, Attrs, Buffer, Family, Metrics, Shaping, Style, Weight, Wrap};
use pulldown_cmark::Alignment;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_CONTENT: f32 = 860.0;
pub const SIDE_PAD: f32 = 32.0;
pub const TOP_PAD: f32 = 32.0;
pub const BOTTOM_PAD: f32 = 64.0;
pub const INDENT: f32 = 26.0;
pub const CODE_PAD_X: f32 = 16.0;
pub const CODE_PAD_Y: f32 = 13.0;
pub const CELL_PAD_X: f32 = 12.0;
pub const CELL_PAD_Y: f32 = 7.0;
pub const SIDEBAR: f32 = 280.0;
pub const TOC_ROW: f32 = 30.0;
pub const SCROLLBAR_HIT: f32 = 16.0;
const SCROLL_STEP: f64 = 100.0;
/// Undoes the compositor's touchpad scaling (Omarchy ships scroll_factor = 0.4)
/// so content tracks the finger. `--scroll-speed` adjusts it for other setups.
const TOUCHPAD_GAIN: f64 = 2.5;
const FLING_FRICTION: f64 = 3.2;
const FLING_MIN_SPEED: f64 = 40.0;
const FLING_MAX_AGE: Duration = Duration::from_millis(80);
const MAX_SHAPED: usize = 96;
const MAX_MATCHES: usize = 50_000;
const SMOOTHING: f64 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
    pub block: u32,
    pub off: u32,
}

pub struct Part {
    pub buf: Buffer,
    pub x: f32,
    pub y: f32,
    pub base: u32,
    pub line_starts: Box<[u32]>,
}

pub struct Shaped {
    pub parts: Vec<Part>,
    pub marker: Option<Buffer>,
    pub height: f32,
}

pub struct Search {
    pub query: String,
    pub editing: bool,
    pub matches: Vec<(u32, u32, u32)>,
    pub current: usize,
}

struct History {
    path: Option<PathBuf>,
    kept: Option<Doc>,
    block: usize,
    offset: f64,
}

struct Press {
    x: f32,
    y: f32,
    link: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cursor {
    Default,
    Text,
    Link,
}

pub struct View {
    pub fonts: Fonts,
    pub doc: Doc,
    pub path: Option<PathBuf>,
    pub images: Images,
    pub w: i32,
    pub h: i32,
    win_scale: f32,
    zoom: f32,
    pub s: f32,
    pub dark: bool,
    pub heights: Vec<f32>,
    pub ys: Vec<f64>,
    ys_dirty: bool,
    pub shaped: HashMap<u32, Shaped>,
    table_cols: HashMap<u32, Box<[f32]>>,
    pub content_x: f32,
    pub content_w: f32,
    pub scroll: f64,
    target: f64,
    stick_end: bool,
    last_tick: Instant,
    pub scroll_speed: f64,
    /// Touchpad velocity in pixels per second, and when it last moved.
    touch: Option<(f64, Instant)>,
    fling: f64,
    pub sel: Option<(Pos, Pos)>,
    selecting: bool,
    press: Option<Press>,
    last_click: Option<(Instant, f32, f32, u32)>,
    pub search: Option<Search>,
    pub toc_open: bool,
    pub toc_scroll: f32,
    pub toc_bufs: HashMap<u32, Buffer>,
    toc_current: Option<usize>,
    status: Option<(String, Option<Instant>)>,
    pub hover_link: Option<u32>,
    pub cursor: Cursor,
    history: Vec<History>,
    clipboard: Option<arboard::Clipboard>,
    pointer: (f32, f32),
    dragging_scrollbar: bool,
    pub needs_redraw: bool,
}

/// Packs a style and link index into cosmic-text's per-glyph metadata.
pub fn pack_meta(style: u16, link: u32) -> usize {
    style as usize | (link.wrapping_add(1) as usize) << 16
}

pub fn meta_style(meta: usize) -> u16 {
    meta as u16
}

pub fn meta_link(meta: usize) -> Option<u32> {
    match (meta >> 16) as u32 {
        0 => None,
        n => Some(n - 1),
    }
}

fn attrs(style: u16, mono: bool, force_bold: bool) -> Attrs<'static> {
    let family = if mono || style & CODE != 0 { MONO } else { SANS };
    let mut a = Attrs::new().family(Family::Name(family));
    if force_bold || style & BOLD != 0 {
        a = a.weight(Weight::BOLD);
    }
    if style & ITALIC != 0 {
        a = a.style(Style::Italic);
    }
    a
}

struct Rich<'a> {
    text: &'a str,
    base: usize,
    spans: &'a [Span],
    size: f32,
    line: f32,
    mono: bool,
    bold: bool,
    width: Option<f32>,
    align: Option<Align>,
}

fn rich_buffer(fonts: &mut Fonts, r: Rich) -> (Buffer, Box<[u32]>) {
    fonts.ensure_coverage(r.text);
    let metrics = Metrics::new(r.size, r.line);
    let default = attrs(0, r.mono, r.bold).metadata(pack_meta(0, NO_LINK));
    let mut pieces: Vec<(&str, Attrs)> = Vec::with_capacity(r.spans.len() * 2 + 1);
    let mut pos = 0;
    for span in r.spans {
        let start = (span.start as usize).saturating_sub(r.base).min(r.text.len());
        let end = (span.end as usize).saturating_sub(r.base).min(r.text.len());
        if start > pos {
            pieces.push((&r.text[pos..start], default.clone()));
        }
        if end > start {
            let mut a = attrs(span.style, r.mono, r.bold).metadata(pack_meta(span.style, span.link));
            if span.style & CODE != 0 && !r.mono {
                a = a.metrics(Metrics::new(r.size * 0.875, r.line));
            }
            pieces.push((&r.text[start..end], a));
        }
        pos = pos.max(end);
    }
    if pos < r.text.len() || pieces.is_empty() {
        pieces.push((&r.text[pos..], default.clone()));
    }
    let mut buf = Buffer::new(&mut fonts.fs, metrics);
    buf.set_wrap(Wrap::WordOrGlyph);
    buf.set_tab_width(4);
    buf.set_size(r.width, None);
    buf.set_rich_text(pieces, &default, Shaping::Advanced, r.align);
    buf.shape_until_scroll(&mut fonts.fs, false);
    let mut starts = vec![0u32];
    for (i, b) in r.text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    (buf, starts.into())
}

pub fn buffer_height(buf: &Buffer) -> f32 {
    buf.layout_runs().map(|r| r.line_top + r.line_height).fold(0.0, f32::max)
}

pub fn buffer_width(buf: &Buffer) -> f32 {
    buf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

pub fn plain_buffer(fonts: &mut Fonts, text: &str, size: f32, line: f32, bold: bool, mono: bool) -> Buffer {
    let r = Rich { text, base: 0, spans: &[], size, line, mono, bold, width: None, align: None };
    rich_buffer(fonts, r).0
}

pub fn heading_size(level: u8) -> (f32, f32) {
    match level {
        1 => (32.0, 42.0),
        2 => (25.0, 34.0),
        3 => (20.5, 29.0),
        4 => (17.5, 26.0),
        5 => (15.5, 24.0),
        _ => (14.5, 22.0),
    }
}

const BODY: (f32, f32) = (16.0, 26.5);
const CODE_TEXT: (f32, f32) = (13.5, 21.0);
pub const RULE_H_PX: f32 = 3.0;

/// Logical gap above a block.
pub fn gap(blocks: &[doc::Block], i: usize) -> f32 {
    if i == 0 {
        return 0.0;
    }
    let (prev, b) = (&blocks[i - 1], &blocks[i]);
    match &b.kind {
        Kind::Code { first: false, .. } => 0.0,
        Kind::Row { row, .. } if *row > 0 => 0.0,
        Kind::Heading(1 | 2) => 34.0,
        Kind::Heading(_) => 26.0,
        Kind::Rule => 26.0,
        Kind::Code { .. } | Kind::Image { .. } | Kind::Row { .. } => 16.0,
        Kind::Para if b.tight => 5.0,
        Kind::Para if matches!(prev.kind, Kind::Heading(_)) => 10.0,
        Kind::Para if matches!(prev.kind, Kind::Rule) => 26.0,
        Kind::Para => 15.0,
    }
}

/// Display size of an image block in physical pixels.
pub fn image_size(natural: (u32, u32), scale: f32, max_width: f32) -> (u32, u32) {
    let width = (natural.0 as f32 * scale).min(max_width).max(1.0);
    let height = (natural.1 as f32 * width / natural.0.max(1) as f32).max(1.0);
    (width.round() as u32, height.round() as u32)
}

fn cell_align(a: Alignment) -> Option<Align> {
    match a {
        Alignment::Center => Some(Align::Center),
        Alignment::Right => Some(Align::Right),
        _ => None,
    }
}

impl View {
    pub fn new(doc: Doc, path: Option<PathBuf>, dark: bool, wake: Arc<dyn Fn() + Send + Sync>) -> View {
        View {
            fonts: Fonts::new(),
            doc,
            path,
            images: Images::new(wake),
            w: 1,
            h: 1,
            win_scale: 1.0,
            zoom: 1.0,
            s: 1.0,
            dark,
            heights: Vec::new(),
            ys: Vec::new(),
            ys_dirty: true,
            shaped: HashMap::new(),
            table_cols: HashMap::new(),
            content_x: 0.0,
            content_w: 1.0,
            scroll: 0.0,
            target: 0.0,
            stick_end: false,
            last_tick: Instant::now(),
            scroll_speed: 1.0,
            touch: None,
            fling: 0.0,
            sel: None,
            selecting: false,
            press: None,
            last_click: None,
            search: None,
            toc_open: false,
            toc_scroll: 0.0,
            toc_bufs: HashMap::new(),
            toc_current: None,
            status: None,
            hover_link: None,
            cursor: Cursor::Default,
            history: Vec::new(),
            clipboard: None,
            pointer: (0.0, 0.0),
            dragging_scrollbar: false,
            needs_redraw: true,
        }
    }

    pub fn win_scale(&self) -> f32 {
        self.win_scale
    }

    pub fn title(&self) -> String {
        match self.path.as_ref().and_then(|p| p.file_name()) {
            Some(name) => format!("{} — markdonk", name.to_string_lossy()),
            None => "markdonk".to_string(),
        }
    }

    pub fn status_text(&self) -> Option<&str> {
        if let Some((text, _)) = &self.status {
            return Some(text);
        }
        self.hover_link.and_then(|l| self.doc.links.get(l as usize)).map(|s| &**s)
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = Some((text.into(), Some(Instant::now() + Duration::from_secs(3))));
        self.needs_redraw = true;
    }

    /// When the event loop must wake on its own, for example to hide a message.
    pub fn next_wake(&self) -> Option<Instant> {
        self.status.as_ref().and_then(|s| s.1)
    }

    pub fn tick_timers(&mut self) {
        if let Some((_, Some(at))) = &self.status {
            if Instant::now() >= *at {
                self.status = None;
                self.needs_redraw = true;
            }
        }
    }

    pub fn resize(&mut self, w: i32, h: i32, win_scale: f32) {
        let scale_changed = win_scale != self.win_scale;
        self.w = w.max(1);
        self.h = h.max(1);
        self.win_scale = win_scale;
        self.s = win_scale * self.zoom;
        self.relayout(scale_changed);
    }

    fn sidebar_px(&self) -> f32 {
        if self.toc_open { SIDEBAR * self.win_scale } else { 0.0 }
    }

    /// Recomputes geometry and heights, keeping the same block at the top.
    fn relayout(&mut self, force: bool) {
        let sidebar = self.sidebar_px();
        let avail = (self.w as f32 - sidebar).max(1.0);
        let pad = SIDE_PAD * self.s;
        let content_w = (MAX_CONTENT * self.s).min(avail - 2.0 * pad).max(40.0);
        let content_x = sidebar + ((avail - content_w) / 2.0).max(0.0).floor();
        let width_changed = (content_w - self.content_w).abs() > 0.5;
        self.content_x = content_x;
        if !force && !width_changed && self.heights.len() == self.doc.blocks.len() {
            self.ys_dirty = true;
            self.needs_redraw = true;
            return;
        }
        let (anchor, ratio) = self.anchor();
        self.content_w = content_w;
        self.shaped.clear();
        self.table_cols.clear();
        self.toc_bufs.clear();
        self.heights.clear();
        self.heights.reserve_exact(self.doc.blocks.len());
        for i in 0..self.doc.blocks.len() {
            let h = self.estimate(i);
            self.heights.push(h);
        }
        self.rebuild_ys();
        self.restore_anchor(anchor, ratio);
        self.needs_redraw = true;
    }

    fn anchor(&self) -> (usize, f64) {
        if self.ys.len() < 2 {
            return (0, 0.0);
        }
        let i = self.block_at(self.scroll);
        let h = self.heights.get(i).copied().unwrap_or(1.0).max(1.0) as f64;
        (i, ((self.scroll - self.ys[i]) / h).clamp(0.0, 1.0))
    }

    fn restore_anchor(&mut self, block: usize, ratio: f64) {
        if self.doc.blocks.is_empty() || self.ys.len() < 2 {
            self.scroll = 0.0;
            self.target = 0.0;
            return;
        }
        if self.scroll <= 0.0 && block == 0 && ratio == 0.0 {
            return;
        }
        let i = block.min(self.doc.blocks.len() - 1);
        self.scroll = self.ys[i] + self.heights[i] as f64 * ratio;
        self.target = self.scroll;
    }

    fn text_width(&self, b: &doc::Block) -> f32 {
        (self.content_w - b.level as f32 * INDENT * self.s).max(20.0)
    }

    pub fn block_x(&self, b: &doc::Block) -> f32 {
        self.content_x + b.level as f32 * INDENT * self.s
    }

    fn estimate(&self, i: usize) -> f32 {
        let b = &self.doc.blocks[i];
        let s = self.s;
        let width = self.text_width(b);
        let wrapped = |size: f32, line: f32, ratio: f32| {
            let lines = ((b.text.len() as f32 * size * s * ratio) / width).ceil().max(1.0);
            lines * line * s
        };
        let content = match &b.kind {
            Kind::Para => wrapped(BODY.0, BODY.1, 0.5),
            Kind::Heading(l) => {
                let (size, line) = heading_size(*l);
                wrapped(size, line, 0.55) + if *l <= 2 { 11.0 * s } else { 0.0 }
            }
            Kind::Code { first, last, .. } => {
                let lines = b.text.bytes().filter(|&c| c == b'\n').count() as f32 + 1.0;
                let pads = *first as u8 as f32 + *last as u8 as f32;
                lines * CODE_TEXT.1 * s + pads * CODE_PAD_Y * s
            }
            Kind::Rule => RULE_H_PX * s,
            Kind::Image { width: w, height: h, .. } => image_size((*w, *h), s, width).1 as f32,
            Kind::Row { .. } => (BODY.1 + 2.0 * CELL_PAD_Y) * s,
        };
        content + gap(&self.doc.blocks, i) * s
    }

    fn rebuild_ys(&mut self) {
        let n = self.heights.len();
        self.ys.clear();
        self.ys.reserve_exact(n + 1);
        let mut y = TOP_PAD as f64 * self.s as f64;
        for &h in &self.heights {
            self.ys.push(y);
            y += h as f64;
        }
        self.ys.push(y);
        self.ys_dirty = false;
    }

    pub fn max_scroll(&self) -> f64 {
        let total = self.ys.last().copied().unwrap_or(0.0) + (BOTTOM_PAD * self.s) as f64;
        (total - self.h as f64).max(0.0)
    }

    pub fn total_height(&self) -> f64 {
        self.ys.last().copied().unwrap_or(0.0) + (BOTTOM_PAD * self.s) as f64
    }

    pub fn block_at(&self, y: f64) -> usize {
        let n = self.doc.blocks.len();
        if n == 0 || self.ys.len() < 2 {
            return 0;
        }
        self.ys[1..].partition_point(|&end| end <= y).min(n - 1)
    }

    /// Lays out one block; returns how much its height changed.
    fn ensure_shaped(&mut self, i: usize) -> f32 {
        if self.shaped.contains_key(&(i as u32)) {
            return 0.0;
        }
        let shaped = self.shape_block(i);
        let height = shaped.height + gap(&self.doc.blocks, i) * self.s;
        self.shaped.insert(i as u32, shaped);
        let delta = height - self.heights[i];
        if delta.abs() > 0.01 {
            self.heights[i] = height;
            self.ys_dirty = true;
            delta
        } else {
            0.0
        }
    }

    fn shape_block(&mut self, i: usize) -> Shaped {
        let s = self.s;
        let width = self.text_width(&self.doc.blocks[i]);
        let mut parts = Vec::new();
        let height;

        if let Kind::Row { table, .. } = self.doc.blocks[i].kind {
            self.ensure_table_cols(table, width);
        }
        let Self { fonts, doc, table_cols, .. } = self;
        let b = &doc.blocks[i];
        let marker = match b.marker {
            doc::Marker::Number(n) => {
                let (size, line) = BODY;
                Some(plain_buffer(fonts, &format!("{n}."), size * s, line * s, false, false))
            }
            _ => None,
        };
        match &b.kind {
            Kind::Para | Kind::Heading(_) => {
                let (size, line, bold) = match b.kind {
                    Kind::Heading(l) => {
                        let (a, c) = heading_size(l);
                        (a, c, true)
                    }
                    _ => (BODY.0, BODY.1, false),
                };
                let (buf, line_starts) = rich_buffer(fonts, Rich {
                    text: &b.text,
                    base: 0,
                    spans: &b.spans,
                    size: size * s,
                    line: line * s,
                    mono: false,
                    bold,
                    width: Some(width),
                    align: None,
                });
                let mut h = buffer_height(&buf);
                if matches!(b.kind, Kind::Heading(1 | 2)) {
                    h += 11.0 * s;
                }
                parts.push(Part { buf, x: 0.0, y: 0.0, base: 0, line_starts });
                height = h;
            }
            Kind::Code { lang, state, first, last } => {
                let mut spans = Vec::new();
                highlight::scan(*lang, &b.text, *state, |start, end, token| {
                    spans.push(Span { start: start as u32, end: end as u32, style: token << TOKEN_SHIFT, link: NO_LINK });
                });
                let inner = width - 2.0 * CODE_PAD_X * s;
                let (buf, line_starts) = rich_buffer(fonts, Rich {
                    text: &b.text,
                    base: 0,
                    spans: &spans,
                    size: CODE_TEXT.0 * s,
                    line: CODE_TEXT.1 * s,
                    mono: true,
                    bold: false,
                    width: Some(inner.max(20.0)),
                    align: None,
                });
                let top = if *first { CODE_PAD_Y * s } else { 0.0 };
                let bottom = if *last { CODE_PAD_Y * s } else { 0.0 };
                height = top + buffer_height(&buf) + bottom;
                parts.push(Part { buf, x: CODE_PAD_X * s, y: top, base: 0, line_starts });
            }
            Kind::Rule => height = RULE_H_PX * s,
            Kind::Image { width: w, height: h, .. } => {
                height = image_size((*w, *h), s, width).1 as f32;
                if !b.text.is_empty() {
                    let (buf, line_starts) = rich_buffer(fonts, Rich {
                        text: &b.text,
                        base: 0,
                        spans: &[],
                        size: 14.0 * s,
                        line: 20.0 * s,
                        mono: false,
                        bold: false,
                        width: Some(width - 24.0 * s),
                        align: None,
                    });
                    parts.push(Part { buf, x: 12.0 * s, y: 12.0 * s, base: 0, line_starts });
                }
            }
            Kind::Row { table, row, cells } => {
                let cols = table_cols.get(table).cloned().unwrap_or_default();
                let aligns = &doc.tables[*table as usize].aligns;
                let mut x = 0.0;
                let mut tallest: f32 = 0.0;
                for (c, &(start, end)) in cells.iter().enumerate() {
                    let col = cols.get(c).copied().unwrap_or(100.0 * s);
                    let text = &b.text[start as usize..end as usize];
                    let spans: Vec<Span> =
                        b.spans.iter().filter(|p| p.start >= start && p.end <= end).copied().collect();
                    let inner = (col - 2.0 * CELL_PAD_X * s).max(8.0);
                    let (buf, line_starts) = rich_buffer(fonts, Rich {
                        text,
                        base: start as usize,
                        spans: &spans,
                        size: (BODY.0 - 1.0) * s,
                        line: (BODY.1 - 3.0) * s,
                        mono: false,
                        bold: *row == 0,
                        width: Some(inner),
                        align: aligns.get(c).copied().and_then(cell_align),
                    });
                    tallest = tallest.max(buffer_height(&buf));
                    parts.push(Part {
                        buf,
                        x: x + CELL_PAD_X * s,
                        y: CELL_PAD_Y * s,
                        base: start,
                        line_starts,
                    });
                    x += col;
                }
                height = tallest.max((BODY.1 - 3.0) * s) + 2.0 * CELL_PAD_Y * s + s.max(1.0);
            }
        }
        Shaped { parts, marker, height }
    }

    /// Column widths: fit to content when possible, otherwise share the slack
    /// in proportion to how much each column would like to grow.
    fn ensure_table_cols(&mut self, table: u32, width: f32) {
        if self.table_cols.contains_key(&table) {
            return;
        }
        let s = self.s;
        let Self { fonts, doc, table_cols, .. } = self;
        let t = &doc.tables[table as usize];
        let columns = t.aligns.len();
        let mut longest: Vec<Vec<(usize, &str)>> = vec![Vec::new(); columns];
        let mut longest_word: Vec<&str> = vec![""; columns];
        for r in t.first..t.first + t.rows {
            let b = &doc.blocks[r as usize];
            let Kind::Row { cells, .. } = &b.kind else { continue };
            for (c, &(start, end)) in cells.iter().enumerate().take(columns) {
                let text = &b.text[start as usize..end as usize];
                let chars = text.len();
                let slot = &mut longest[c];
                if slot.len() < 3 {
                    slot.push((chars, text));
                } else if let Some(min) = slot.iter_mut().min_by_key(|e| e.0) {
                    if chars > min.0 {
                        *min = (chars, text);
                    }
                }
                for word in text.split_whitespace() {
                    if word.len() > longest_word[c].len() {
                        longest_word[c] = word;
                    }
                }
            }
        }
        let pad = 2.0 * CELL_PAD_X * s;
        let (size, line) = ((BODY.0 - 1.0) * s, (BODY.1 - 3.0) * s);
        let mut max = vec![pad; columns];
        let mut min = vec![pad; columns];
        for c in 0..columns {
            for (_, text) in &longest[c] {
                let buf = plain_buffer(fonts, text, size, line, true, false);
                max[c] = max[c].max(buffer_width(&buf) * 1.03 + pad + 2.0 * s);
            }
            let buf = plain_buffer(fonts, longest_word[c], size, line, true, false);
            min[c] = (buffer_width(&buf) * 1.03 + pad + 2.0 * s).min(width * 0.5);
            max[c] = max[c].max(min[c]);
        }
        let sum_max: f32 = max.iter().sum();
        let sum_min: f32 = min.iter().sum();
        let cols: Vec<f32> = if sum_max <= width {
            max
        } else if sum_min >= width {
            min.iter().map(|m| m * width / sum_min).collect()
        } else {
            let grow = (width - sum_min) / (sum_max - sum_min).max(1.0);
            (0..columns).map(|c| min[c] + (max[c] - min[c]) * grow).collect()
        };
        table_cols.insert(table, cols.into());
    }

    pub fn table_width(&self, table: u32) -> f32 {
        self.table_cols.get(&table).map_or(0.0, |c| c.iter().sum())
    }

    pub fn table_col_widths(&self, table: u32) -> &[f32] {
        self.table_cols.get(&table).map_or(&[], |c| c)
    }

    /// Makes sure everything near the viewport is laid out and positions agree.
    pub fn prepare(&mut self) {
        if self.heights.len() != self.doc.blocks.len() {
            self.relayout(true);
        }
        if self.ys_dirty {
            self.rebuild_ys();
        }
        self.clamp_scroll();
        let n = self.doc.blocks.len();
        if n == 0 {
            return;
        }
        let over = self.h as f64 * 0.5;
        for _ in 0..6 {
            let mut changed = false;
            let top = self.block_at(self.scroll);
            let mut y = self.ys[top];
            let mut i = top;
            while i < n && y < self.scroll + self.h as f64 + over {
                changed |= self.ensure_shaped(i) != 0.0;
                y += self.heights[i] as f64;
                i += 1;
            }
            let mut j = top;
            let mut budget = over;
            while j > 0 && budget > 0.0 {
                j -= 1;
                let delta = self.ensure_shaped(j) as f64;
                if delta != 0.0 {
                    self.scroll += delta;
                    self.target += delta;
                    changed = true;
                }
                budget -= self.heights[j] as f64;
            }
            if self.ys_dirty {
                self.rebuild_ys();
            }
            if self.stick_end {
                self.scroll = self.max_scroll();
                self.target = self.scroll;
            }
            self.clamp_scroll();
            if !changed {
                break;
            }
        }
        self.request_images();
        self.evict();
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        self.scroll = self.scroll.clamp(0.0, max);
        self.target = self.target.clamp(0.0, max);
    }

    fn evict(&mut self) {
        if self.shaped.len() > MAX_SHAPED {
            let lo = self.block_at(self.scroll - self.h as f64 * 1.5) as u32;
            let hi = self.block_at(self.scroll + self.h as f64 * 2.5) as u32;
            self.shaped.retain(|&i, _| i >= lo && i <= hi);
        }
    }

    fn request_images(&mut self) {
        self.images.poll();
        let h = self.h as f64;
        let lo = self.block_at(self.scroll - h);
        let hi = self.block_at(self.scroll + 2.0 * h);
        for i in lo..=hi {
            let b = &self.doc.blocks[i];
            if let Kind::Image { path, width, height } = &b.kind {
                let (w, h) = image_size((*width, *height), self.s, self.text_width(b));
                self.images.request(i as u32, path, w, h);
            }
        }
        let keep_lo = self.block_at(self.scroll - 2.0 * h) as u32;
        let keep_hi = self.block_at(self.scroll + 3.0 * h) as u32 + 1;
        self.images.evict(keep_lo..keep_hi, self.block_at(self.scroll) as u32);
    }

    // ----- scrolling -----

    pub fn animating(&self) -> bool {
        (self.target - self.scroll).abs() > 0.5 || self.fling != 0.0 || (self.selecting && self.pointer_outside())
    }

    fn pointer_outside(&self) -> bool {
        self.pointer.1 < 0.0 || self.pointer.1 > self.h as f32
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        if self.fling != 0.0 {
            self.scroll = (self.scroll + self.fling * dt).clamp(0.0, self.max_scroll());
            self.target = self.scroll;
            self.fling *= (-FLING_FRICTION * dt).exp();
            let at_edge = self.scroll <= 0.0 || self.scroll >= self.max_scroll();
            if self.fling.abs() < FLING_MIN_SPEED * self.s as f64 || at_edge {
                self.fling = 0.0;
            }
            return;
        }
        if self.selecting && self.pointer_outside() {
            let over = if self.pointer.1 < 0.0 { self.pointer.1 } else { self.pointer.1 - self.h as f32 };
            self.target += over as f64 * dt * 8.0;
            self.scroll = self.target;
            let (x, y) = self.pointer;
            if let Some(pos) = self.hit(x, y.clamp(0.0, self.h as f32 - 1.0)).map(|h| h.pos) {
                if let Some(sel) = &mut self.sel {
                    sel.1 = pos;
                }
            }
        } else {
            let d = self.target - self.scroll;
            if d.abs() <= 0.5 {
                self.scroll = self.target;
            } else {
                self.scroll += d * (1.0 - (-SMOOTHING * dt).exp());
            }
        }
    }

    fn scroll_animated(&mut self, delta: f64) {
        self.stick_end = false;
        self.fling = 0.0;
        // Only restart the clock from rest: resetting it mid-animation starves
        // the next frame's step whenever input arrives faster than frames.
        if (self.target - self.scroll).abs() <= 0.5 {
            self.last_tick = Instant::now();
        }
        self.target = (self.target + delta).clamp(0.0, self.max_scroll());
        self.needs_redraw = true;
    }

    fn scroll_instant(&mut self, y: f64) {
        self.stick_end = false;
        self.fling = 0.0;
        self.scroll = y.clamp(0.0, self.max_scroll());
        self.target = self.scroll;
        self.needs_redraw = true;
    }

    pub fn scroll_lines(&mut self, lines: f64) {
        self.scroll_animated(lines * SCROLL_STEP * self.s as f64 * self.scroll_speed);
    }

    pub fn scroll_pages(&mut self, pages: f64) {
        self.scroll_animated(pages * (self.h as f64 * 0.9));
    }

    pub fn scroll_pixels(&mut self, dy: f64) {
        self.stick_end = false;
        self.fling = 0.0;
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll());
        self.target = self.scroll;
        self.needs_redraw = true;
    }

    pub fn wheel(&mut self, dy: f64, is_pixels: bool, x: f32) {
        if self.toc_open && x < self.sidebar_px() {
            let delta = if is_pixels { dy as f32 } else { dy as f32 * SCROLL_STEP as f32 * self.win_scale };
            let row = TOC_ROW * self.win_scale;
            let content = self.doc.toc.len() as f32 * row;
            let max = (content + row - self.h as f32).max(0.0);
            self.toc_scroll = (self.toc_scroll + delta).clamp(0.0, max);
            self.needs_redraw = true;
        } else if is_pixels {
            let dy = dy * TOUCHPAD_GAIN * self.scroll_speed;
            self.track_touch(dy);
            self.scroll_pixels(dy);
        } else {
            self.scroll_lines(dy);
        }
    }

    fn track_touch(&mut self, dy: f64) {
        let now = Instant::now();
        let speed = match self.touch {
            Some((speed, at)) => {
                let dt = now.duration_since(at).as_secs_f64().max(0.001);
                if dt > FLING_MAX_AGE.as_secs_f64() { dy / dt } else { speed * 0.6 + dy / dt * 0.4 }
            }
            None => 0.0,
        };
        self.touch = Some((speed, now));
    }

    /// The fingers lifted: carry on with the recent velocity, unless they had
    /// already stopped moving.
    pub fn touch_ended(&mut self) {
        if let Some((speed, at)) = self.touch.take() {
            if at.elapsed() < FLING_MAX_AGE && speed.abs() > FLING_MIN_SPEED * self.s as f64 {
                self.fling = speed;
                self.last_tick = Instant::now();
                self.needs_redraw = true;
            }
        }
    }

    pub fn go_top(&mut self) {
        self.scroll_instant(0.0);
    }

    pub fn go_end(&mut self) {
        self.scroll_instant(self.max_scroll());
        self.stick_end = true;
    }

    pub fn jump_to_block(&mut self, i: usize) {
        if i >= self.doc.blocks.len() {
            return;
        }
        if self.ys_dirty {
            self.rebuild_ys();
        }
        let top = self.ys[i] + (gap(&self.doc.blocks, i) * self.s) as f64;
        self.scroll_instant(top - (20.0 * self.s) as f64);
    }

    /// Scrolls just enough to show a text position, unless it's already visible.
    fn reveal(&mut self, block: usize, off: u32) {
        self.ensure_shaped(block);
        if self.ys_dirty {
            self.rebuild_ys();
        }
        let mut y = self.ys[block] + (gap(&self.doc.blocks, block) * self.s) as f64;
        if let Some(shaped) = self.shaped.get(&(block as u32)) {
            let part = shaped.parts.iter().rev().find(|p| p.base <= off).or(shaped.parts.first());
            if let Some(part) = part {
                y += part.y as f64;
                let rel = off - part.base.min(off);
                let line = part.line_starts.partition_point(|&s| s <= rel).saturating_sub(1);
                let start = part.line_starts.get(line).copied().unwrap_or(0);
                for run in part.buf.layout_runs() {
                    if run.line_i == line && run.glyphs.first().is_none_or(|g| g.start as u32 + start <= rel) {
                        y = self.ys[block] + (gap(&self.doc.blocks, block) * self.s) as f64
                            + part.y as f64 + run.line_top as f64;
                    }
                }
            }
        }
        let margin = 60.0 * self.s as f64;
        if y < self.scroll + margin || y > self.scroll + self.h as f64 - margin {
            self.scroll_instant(y - self.h as f64 * 0.3);
        } else {
            self.needs_redraw = true;
        }
    }

    // ----- appearance -----

    pub fn toggle_theme(&mut self) {
        self.dark = !self.dark;
        self.needs_redraw = true;
    }

    pub fn toggle_toc(&mut self) {
        if self.doc.toc.is_empty() && !self.toc_open {
            self.set_status("This document has no headings");
            return;
        }
        self.toc_open = !self.toc_open;
        self.toc_current = None;
        if !self.toc_open {
            self.toc_bufs = HashMap::new();
        }
        self.relayout(false);
    }

    pub fn zoom_by(&mut self, factor: f32) {
        self.set_zoom((self.zoom * factor).clamp(0.5, 3.0));
    }

    pub fn zoom_reset(&mut self) {
        self.set_zoom(1.0);
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom;
        self.s = self.win_scale * zoom;
        self.relayout(true);
    }

    /// Index of the section at the top of the viewport.
    pub fn current_toc(&mut self) -> Option<usize> {
        let top = self.block_at(self.scroll + 40.0 * self.s as f64) as u32;
        let current = self.doc.toc.partition_point(|e| e.block <= top).checked_sub(1);
        if current != self.toc_current {
            self.toc_current = current;
            if let Some(c) = current {
                let row = TOC_ROW * self.win_scale;
                let y = c as f32 * row;
                if y < self.toc_scroll + row || y > self.toc_scroll + self.h as f32 - 2.0 * row {
                    let max = (self.doc.toc.len() as f32 * row + row - self.h as f32).max(0.0);
                    self.toc_scroll = (y - self.h as f32 / 3.0).clamp(0.0, max);
                }
            }
        }
        current
    }

    // ----- hit testing and selection -----

    pub fn hit(&self, sx: f32, sy: f32) -> Option<Hit> {
        if self.doc.blocks.is_empty() {
            return None;
        }
        let dy = sy as f64 + self.scroll;
        let i = self.block_at(dy);
        let b = &self.doc.blocks[i];
        let pos0 = Pos { block: i as u32, off: 0 };
        let miss = Hit { pos: pos0, link: None, over_text: false };
        let Some(shaped) = self.shaped.get(&(i as u32)) else { return Some(miss) };
        let top = self.ys[i] + (gap(&self.doc.blocks, i) * self.s) as f64;
        let bx = self.block_x(b);
        if let Kind::Image { .. } = b.kind {
            let link = b.spans.first().filter(|s| s.style & LINK != 0).map(|s| s.link);
            return Some(Hit { pos: pos0, link, over_text: false });
        }
        let part = shaped
            .parts
            .iter()
            .rev()
            .find(|p| sx >= bx + p.x - (CELL_PAD_X * self.s))
            .or(shaped.parts.first());
        let Some(part) = part else { return Some(miss) };
        let lx = sx - (bx + part.x);
        let ly = (dy - top) as f32 - part.y;
        let mut chosen = None;
        for run in part.buf.layout_runs() {
            let bottom = run.line_top + run.line_height;
            chosen = Some(run);
            if ly < bottom {
                break;
            }
        }
        let Some(run) = chosen else { return Some(miss) };
        let start = part.base + part.line_starts.get(run.line_i).copied().unwrap_or(0);
        let inside_y = ly >= run.line_top && ly < run.line_top + run.line_height;
        let mut link = None;
        let mut over_text = false;
        let mut off = run.glyphs.last().map_or(start, |g| start + g.end as u32);
        let mut found = false;
        for g in run.glyphs {
            if !found && lx < g.x + g.w / 2.0 {
                off = start + g.start as u32;
                found = true;
            }
            if inside_y && lx >= g.x && lx < g.x + g.w {
                over_text = true;
                link = meta_link(g.metadata);
            }
        }
        Some(Hit { pos: Pos { block: i as u32, off }, link, over_text })
    }

    pub fn pointer_moved(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        if self.dragging_scrollbar {
            self.drag_scrollbar(y);
            return;
        }
        if self.selecting {
            if let Some(hit) = self.hit(x, y) {
                if let Some(sel) = &mut self.sel {
                    if sel.1 != hit.pos {
                        sel.1 = hit.pos;
                        self.needs_redraw = true;
                    }
                }
            }
            if self.pointer_outside() {
                self.last_tick = Instant::now();
                self.needs_redraw = true;
            }
            return;
        }
        let in_side = x < self.sidebar_px();
        let on_bar = x >= self.w as f32 - SCROLLBAR_HIT * self.win_scale;
        let (cursor, link) = if in_side {
            (Cursor::Link, None)
        } else if on_bar {
            (Cursor::Default, None)
        } else {
            match self.hit(x, y) {
                Some(h) if h.link.is_some() => (Cursor::Link, h.link),
                Some(h) if h.over_text => (Cursor::Text, None),
                _ => (Cursor::Default, None),
            }
        };
        self.cursor = cursor;
        if link != self.hover_link {
            self.hover_link = link;
            self.needs_redraw = true;
        }
    }

    pub fn pointer_down(&mut self, x: f32, y: f32) {
        self.pointer = (x, y);
        self.fling = 0.0;
        if x < self.sidebar_px() {
            let row = TOC_ROW * self.win_scale;
            let top = 12.0 * self.win_scale;
            let idx = ((y + self.toc_scroll - top) / row).floor();
            if idx >= 0.0 {
                if let Some(entry) = self.doc.toc.get(idx as usize) {
                    let block = entry.block as usize;
                    self.jump_to_block(block);
                }
            }
            return;
        }
        if x >= self.w as f32 - SCROLLBAR_HIT * self.win_scale {
            self.dragging_scrollbar = true;
            self.drag_scrollbar(y);
            return;
        }
        let now = Instant::now();
        let clicks = match self.last_click {
            Some((t, lx, ly, n))
                if now.duration_since(t) < Duration::from_millis(400)
                    && (lx - x).abs() < 6.0
                    && (ly - y).abs() < 6.0 =>
            {
                n % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some((now, x, y, clicks));
        let Some(hit) = self.hit(x, y) else { return };
        self.press = Some(Press { x, y, link: hit.link });
        match clicks {
            2 => {
                let (a, b) = self.word_at(hit.pos);
                self.sel = Some((a, b));
            }
            3 => {
                let block = hit.pos.block;
                let len = self.doc.blocks[block as usize].text.len() as u32;
                self.sel = Some((Pos { block, off: 0 }, Pos { block, off: len }));
            }
            _ => {
                self.sel = Some((hit.pos, hit.pos));
                self.selecting = true;
            }
        }
        self.needs_redraw = true;
    }

    pub fn pointer_up(&mut self, x: f32, y: f32) {
        self.selecting = false;
        self.dragging_scrollbar = false;
        let press = self.press.take();
        if let Some((a, b)) = self.sel {
            if a == b {
                self.sel = None;
                self.needs_redraw = true;
            }
        }
        if let Some(Press { x: px, y: py, link: Some(link) }) = press {
            if (px - x).abs() < 5.0 && (py - y).abs() < 5.0 && self.sel.is_none() {
                self.follow_link(link);
            }
        }
    }

    fn drag_scrollbar(&mut self, y: f32) {
        let frac = (y as f64 / self.h.max(1) as f64).clamp(0.0, 1.0);
        self.scroll_instant(frac * self.max_scroll());
    }

    fn word_at(&self, pos: Pos) -> (Pos, Pos) {
        let text = &self.doc.blocks[pos.block as usize].text;
        let off = (pos.off as usize).min(text.len());
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let start = text[..off].char_indices().rev().find(|(_, c)| !is_word(*c)).map_or(0, |(i, c)| i + c.len_utf8());
        let end = text[off..].char_indices().find(|(_, c)| !is_word(*c)).map_or(text.len(), |(i, _)| off + i);
        (Pos { block: pos.block, off: start as u32 }, Pos { block: pos.block, off: end as u32 })
    }

    pub fn select_all(&mut self) {
        let n = self.doc.blocks.len();
        if n > 0 {
            let end = Pos { block: n as u32 - 1, off: self.doc.blocks[n - 1].text.len() as u32 };
            self.sel = Some((Pos { block: 0, off: 0 }, end));
            self.needs_redraw = true;
        }
    }

    pub fn clear_selection(&mut self) {
        if self.sel.take().is_some() {
            self.needs_redraw = true;
        }
    }

    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let (a, b) = self.sel?;
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection()?;
        let mut out = String::new();
        for i in a.block..=b.block {
            let block = &self.doc.blocks[i as usize];
            let text = &block.text;
            let start = if i == a.block { (a.off as usize).min(text.len()) } else { 0 };
            let end = if i == b.block { (b.off as usize).min(text.len()) } else { text.len() };
            if i > a.block {
                let joined = matches!(block.kind, Kind::Code { first: false, .. })
                    || matches!(block.kind, Kind::Row { row, .. } if row > 0)
                    || block.tight;
                out.push_str(if joined { "\n" } else { "\n\n" });
            }
            if let Some(slice) = text.get(start..end) {
                out.push_str(slice);
            }
        }
        (!out.is_empty()).then_some(out)
    }

    pub fn copy(&mut self) {
        let Some(text) = self.selected_text() else { return };
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let copied = self.clipboard.as_mut().is_some_and(|c| c.set_text(text).is_ok());
        self.set_status(if copied { "Copied" } else { "Could not access the clipboard" });
    }

    // ----- search -----

    pub fn open_search(&mut self) {
        match &mut self.search {
            Some(s) => s.editing = true,
            None => {
                self.search = Some(Search { query: String::new(), editing: true, matches: Vec::new(), current: 0 });
            }
        }
        self.needs_redraw = true;
    }

    pub fn close_search(&mut self) {
        self.search = None;
        self.needs_redraw = true;
    }

    pub fn search_editing(&self) -> bool {
        self.search.as_ref().is_some_and(|s| s.editing)
    }

    pub fn search_input(&mut self, text: &str) {
        if let Some(s) = &mut self.search {
            s.query.extend(text.chars().filter(|c| !c.is_control()));
        }
        self.run_search(true);
    }

    pub fn search_backspace(&mut self) {
        if let Some(s) = &mut self.search {
            s.query.pop();
        }
        self.run_search(true);
    }

    pub fn search_commit(&mut self) {
        if let Some(s) = &mut self.search {
            s.editing = false;
        }
        self.needs_redraw = true;
    }

    /// Recomputes matches; optionally jumps to the first one past the viewport.
    pub fn run_search(&mut self, jump: bool) {
        let top = self.block_at(self.scroll) as u32;
        let Some(s) = &mut self.search else { return };
        s.matches = find_matches(&self.doc, &s.query);
        s.current = s.matches.partition_point(|m| m.0 < top);
        if s.current >= s.matches.len() {
            s.current = 0;
        }
        self.needs_redraw = true;
        if jump {
            self.reveal_current_match();
        }
    }

    pub fn search_step(&mut self, forward: bool) {
        let Some(s) = &mut self.search else { return };
        let n = s.matches.len();
        if n == 0 {
            return;
        }
        s.current = if forward { (s.current + 1) % n } else { (s.current + n - 1) % n };
        self.reveal_current_match();
    }

    fn reveal_current_match(&mut self) {
        let hit = self.search.as_ref().and_then(|s| s.matches.get(s.current).copied());
        if let Some((block, start, _)) = hit {
            self.reveal(block as usize, start);
        }
    }

    // ----- documents and links -----

    fn follow_link(&mut self, link: u32) {
        let Some(url) = self.doc.links.get(link as usize).map(|s| s.to_string()) else { return };
        self.open_link(&url);
    }

    pub fn open_link(&mut self, url: &str) {
        if let Some(fragment) = url.strip_prefix('#') {
            self.jump_to_anchor(fragment);
            return;
        }
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            open_external(url);
            self.set_status(format!("Opening {url}"));
            return;
        }
        if has_scheme(url) {
            self.set_status(format!("Not opening link: {url}"));
            return;
        }
        let (file, fragment) = match url.split_once('#') {
            Some((file, fragment)) => (file, Some(fragment)),
            None => (url, None),
        };
        let base = self.base_dir();
        let path = doc::resolve(base.as_deref(), file);
        if !is_markdown(&path) {
            self.set_status(format!("Not opening non-Markdown file: {file}"));
            return;
        }
        match load_file(&path) {
            Ok(new_doc) => {
                let (block, offset) = self.position();
                let path_old = self.path.take();
                let doc_old = std::mem::replace(&mut self.doc, new_doc);
                let kept = if path_old.is_none() { Some(doc_old) } else { None };
                self.history.push(History { path: path_old, kept, block, offset });
                self.path = Some(path);
                self.doc_changed(0, 0.0);
                if let Some(fragment) = fragment {
                    self.jump_to_anchor(fragment);
                }
            }
            Err(err) => self.set_status(format!("Cannot open {file}: {err}")),
        }
    }

    pub fn go_back(&mut self) {
        let Some(entry) = self.history.pop() else {
            self.set_status("Nothing to go back to");
            return;
        };
        let new_doc = match (&entry.path, entry.kept) {
            (_, Some(doc)) => doc,
            (Some(path), None) => match load_file(path) {
                Ok(doc) => doc,
                Err(err) => {
                    self.set_status(format!("Cannot open {}: {err}", path.display()));
                    return;
                }
            },
            (None, None) => return,
        };
        self.doc = new_doc;
        self.path = entry.path;
        self.doc_changed(entry.block, entry.offset);
    }

    /// Re-reads the file, staying at the same place in the document.
    pub fn reload(&mut self) {
        let Some(path) = self.path.clone() else { return };
        match load_file(&path) {
            Ok(doc) => {
                let (block, offset) = self.position();
                self.doc = doc;
                self.doc_changed(block, offset);
            }
            // Saves can leave the file briefly missing; keep showing what we have.
            Err(err) => self.set_status(format!("Cannot reload: {err}")),
        }
    }

    fn position(&self) -> (usize, f64) {
        self.anchor()
    }

    fn doc_changed(&mut self, block: usize, ratio: f64) {
        self.images.reset();
        self.sel = None;
        self.selecting = false;
        self.hover_link = None;
        self.toc_current = None;
        self.toc_scroll = 0.0;
        self.stick_end = false;
        self.scroll = 0.0;
        self.target = 0.0;
        self.heights.clear();
        self.ys.clear();
        self.relayout(true);
        self.restore_anchor(block, ratio);
        self.run_search(false);
        trim_heap();
    }

    fn jump_to_anchor(&mut self, fragment: &str) {
        let wanted = fragment.to_string();
        let block = self.doc.anchors.get(wanted.as_str()).copied().or_else(|| {
            let slug = doc::slugify(&wanted);
            self.doc.anchors.get(slug.as_str()).copied()
        });
        match block {
            Some(block) => self.jump_to_block(block as usize),
            None => self.set_status(format!("No such section: #{fragment}")),
        }
    }

    fn base_dir(&self) -> Option<PathBuf> {
        match &self.path {
            Some(p) => p.parent().map(Path::to_path_buf),
            None => std::env::current_dir().ok(),
        }
    }
}

pub struct Hit {
    pub pos: Pos,
    pub link: Option<u32>,
    pub over_text: bool,
}

pub fn load_file(path: &Path) -> std::io::Result<Doc> {
    let bytes = std::fs::read(path)?;
    let src = String::from_utf8_lossy(&bytes);
    Ok(doc::parse(&src, path.parent()))
}

fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["md", "markdown", "mdown", "mkd", "mdx", "txt"].iter().any(|m| m.eq_ignore_ascii_case(e))
    })
}

fn has_scheme(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else { return false };
    !scheme.is_empty()
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

fn open_external(url: &str) {
    let child = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn fold(c: char) -> char {
    if c.is_ascii() { c.to_ascii_lowercase() } else { c.to_lowercase().next().unwrap_or(c) }
}

pub fn find_matches(doc: &Doc, query: &str) -> Vec<(u32, u32, u32)> {
    let needle: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    let Some(&first) = needle.first() else { return out };
    for (i, block) in doc.blocks.iter().enumerate() {
        let text = &*block.text;
        let mut skip_until = 0;
        for (start, c) in text.char_indices() {
            if start < skip_until || fold(c) != first {
                continue;
            }
            let mut chars = text[start..].char_indices();
            let mut end = start;
            let mut matched = true;
            for &want in &needle {
                match chars.next() {
                    Some((at, got)) if fold(got) == want => end = start + at + got.len_utf8(),
                    _ => {
                        matched = false;
                        break;
                    }
                }
            }
            if matched {
                out.push((i as u32, start as u32, end as u32));
                skip_until = end;
                if out.len() >= MAX_MATCHES {
                    return out;
                }
            }
        }
    }
    out
}

/// Hands freed heap pages back to the OS after a big allocation burst.
pub fn trim_heap() {
    // SAFETY: malloc_trim has no preconditions.
    unsafe { libc::malloc_trim(0) };
}

pub fn token_style(style: u16) -> u16 {
    (style >> TOKEN_SHIFT) & 0xf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(src: &str) -> View {
        let mut v = View::new(doc::parse(src, None), None, true, Arc::new(|| {}));
        v.resize(1000, 800, 1.0);
        v.prepare();
        v
    }

    fn long_doc() -> String {
        (0..400).map(|i| format!("## Section {i}\n\nParagraph {i} with some words in it.\n\n")).collect()
    }

    #[test]
    fn empty_document_is_safe() {
        let mut v = view("");
        v.scroll_lines(5.0);
        v.tick();
        v.prepare();
        assert!(v.hit(10.0, 10.0).is_none());
        v.select_all();
        assert!(v.selected_text().is_none());
        v.go_end();
        v.prepare();
    }

    #[test]
    fn only_blocks_near_the_viewport_are_shaped() {
        let v = view(&long_doc());
        assert!(v.shaped.len() < 100, "shaped {} of {}", v.shaped.len(), v.doc.blocks.len());
        assert!(v.shaped.contains_key(&0));
        assert!(!v.shaped.contains_key(&700));
    }

    #[test]
    fn go_end_lands_exactly_on_the_real_bottom() {
        let mut v = view(&long_doc());
        v.go_end();
        v.prepare();
        assert_eq!(v.scroll, v.max_scroll());
        assert!(v.shaped.contains_key(&(v.doc.blocks.len() as u32 - 1)));
    }

    #[test]
    fn shaped_cache_stays_bounded_while_scrolling() {
        let mut v = view(&long_doc());
        for i in 0..80 {
            v.scroll_pixels(i as f64 * 200.0);
            v.prepare();
            assert!(v.shaped.len() <= MAX_SHAPED + 40, "shaped {}", v.shaped.len());
        }
    }

    #[test]
    fn scrolling_up_keeps_visible_content_stable() {
        let mut v = view(&long_doc());
        v.jump_to_block(400);
        v.prepare();
        let block = v.block_at(v.scroll);
        let offset = v.scroll - v.ys[block];
        // Measuring blocks above the viewport must shift scroll by the same amount.
        v.prepare();
        assert_eq!(v.block_at(v.scroll), block);
        assert!((v.scroll - v.ys[block] - offset).abs() < 1.0);
    }

    #[test]
    fn resize_keeps_the_same_block_on_top() {
        let mut v = view(&long_doc());
        v.jump_to_block(300);
        v.prepare();
        let block = v.block_at(v.scroll);
        v.resize(500, 800, 1.0);
        v.prepare();
        assert!(v.block_at(v.scroll).abs_diff(block) <= 1);
    }

    #[test]
    fn wheel_input_does_not_starve_the_scroll_animation() {
        let mut v = view(&long_doc());
        v.scroll_lines(1.0);
        v.last_tick = Instant::now() - Duration::from_millis(16);
        let clock = v.last_tick;
        v.scroll_lines(1.0);
        assert_eq!(v.last_tick, clock);
        v.tick();
        assert!(v.scroll > 30.0, "moved only {}", v.scroll);
    }

    #[test]
    fn touchpad_flick_keeps_moving_then_stops() {
        let mut v = view(&long_doc());
        for _ in 0..5 {
            v.wheel(20.0, true, 500.0);
            std::thread::sleep(Duration::from_millis(4));
        }
        let lifted = v.scroll;
        assert_eq!(lifted, 250.0, "gain applies to touchpad deltas");
        v.touch_ended();
        assert!(v.animating());
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(10));
            v.tick();
            if !v.animating() {
                break;
            }
        }
        assert!(v.scroll > lifted + 100.0, "coasted to {}", v.scroll);
        assert!(!v.animating(), "momentum must die out");
    }

    #[test]
    fn lifting_a_resting_finger_does_not_fling() {
        let mut v = view(&long_doc());
        v.wheel(20.0, true, 500.0);
        v.wheel(20.0, true, 500.0);
        std::thread::sleep(FLING_MAX_AGE + Duration::from_millis(20));
        v.touch_ended();
        assert!(!v.animating());
    }

    #[test]
    fn any_other_scroll_cancels_momentum() {
        let mut v = view(&long_doc());
        v.fling = 1000.0;
        v.go_top();
        assert_eq!(v.fling, 0.0);
        v.fling = 1000.0;
        v.pointer_down(500.0, 100.0);
        assert_eq!(v.fling, 0.0);
    }

    #[test]
    fn hit_finds_text_and_links() {
        let v = view("plain [link](https://example.com) tail\n");
        let run_y = TOP_PAD + 10.0;
        let x = v.content_x + 1.0;
        let plain = v.hit(x, run_y).unwrap();
        assert!(plain.over_text && plain.link.is_none());
        assert_eq!(plain.pos.off, 0);
        let part = &v.shaped[&0].parts[0];
        let run = part.buf.layout_runs().next().unwrap();
        let glyph = run.glyphs.iter().find(|g| g.start == 6).unwrap();
        let on_link = v.hit(v.content_x + glyph.x + 1.0, run_y).unwrap();
        assert_eq!(on_link.link, Some(0));
    }

    #[test]
    fn selection_copies_block_text_with_sensible_joins() {
        let mut v = view("one\n\ntwo\n\n- a\n- b\n\n| x | y |\n|---|---|\n| 1 | 2 |\n");
        v.select_all();
        let text = v.selected_text().unwrap();
        assert_eq!(text, "one\n\ntwo\n\na\nb\n\nx\ty\n1\t2");
    }

    #[test]
    fn partial_selection_respects_offsets_in_either_direction() {
        let mut v = view("hello world\n");
        v.sel = Some((Pos { block: 0, off: 11 }, Pos { block: 0, off: 6 }));
        assert_eq!(v.selected_text().unwrap(), "world");
        v.prepare();
    }

    #[test]
    fn double_click_word_bounds_handle_unicode() {
        let v = view("héllo wörld\n");
        let (a, b) = v.word_at(Pos { block: 0, off: 1 });
        assert_eq!((a.off, b.off), (0, 6));
    }

    #[test]
    fn search_is_case_insensitive_and_non_overlapping() {
        let d = doc::parse("Foo fOO\n\nÉTÉ été\n\naaa\n", None);
        assert_eq!(find_matches(&d, "foo").len(), 2);
        assert_eq!(find_matches(&d, "été").len(), 2);
        assert_eq!(find_matches(&d, "aa").len(), 1);
        assert!(find_matches(&d, "").is_empty());
        assert_eq!(find_matches(&d, "é")[0], (1, 0, 2));
    }

    #[test]
    fn search_steps_and_wraps() {
        let mut v = view(&long_doc());
        v.open_search();
        v.search_input("Section 399");
        assert_eq!(v.search.as_ref().unwrap().matches.len(), 1);
        v.prepare();
        let block = v.search.as_ref().unwrap().matches[0].0 as usize;
        assert!(v.ys[block] >= v.scroll && v.ys[block] < v.scroll + v.h as f64);
        v.search_step(true);
        assert_eq!(v.search.as_ref().unwrap().current, 0);
        v.search_backspace();
        assert!(v.search.as_ref().unwrap().matches.len() > 1);
    }

    #[test]
    fn anchors_jump_and_bad_anchors_report() {
        let mut v = view(&long_doc());
        v.open_link("#section-200");
        v.prepare();
        let block = v.doc.anchors["section-200"] as usize;
        let top = v.ys[block] + gap(&v.doc.blocks, block) as f64;
        assert!(top >= v.scroll && top < v.scroll + v.h as f64);
        v.open_link("#nope");
        assert!(v.status_text().unwrap().contains("nope"));
    }

    #[test]
    fn dangerous_or_non_markdown_links_are_refused() {
        let mut v = view("x");
        for url in ["javascript:alert(1)", "file:///etc/passwd", "ftp://x/y", "steam://run/1"] {
            v.open_link(url);
            assert!(v.status_text().unwrap().starts_with("Not opening"), "{url}");
        }
        v.open_link("./run.sh");
        assert!(v.status_text().unwrap().contains("non-Markdown"));
    }

    #[test]
    fn links_between_files_navigate_and_come_back() {
        let dir = std::env::temp_dir().join(format!("markdonk-nav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.md");
        let b = dir.join("b file.md");
        std::fs::write(&a, "# A\n\n[go](b%20file.md#deep)\n").unwrap();
        std::fs::write(&b, "# B\n\n## Deep\n").unwrap();
        let mut v = View::new(load_file(&a).unwrap(), Some(a.clone()), true, Arc::new(|| {}));
        v.resize(800, 600, 1.0);
        v.prepare();
        v.open_link("b%20file.md#deep");
        assert_eq!(v.path.as_deref(), Some(b.as_path()));
        assert_eq!(&*v.doc.blocks[0].text, "B");
        v.prepare();
        v.go_back();
        assert_eq!(v.path.as_deref(), Some(a.as_path()));
        assert_eq!(&*v.doc.blocks[0].text, "A");
        v.open_link("missing.md");
        assert!(v.status_text().unwrap().contains("Cannot open"));
        assert_eq!(v.path.as_deref(), Some(a.as_path()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reload_keeps_position_and_survives_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("markdonk-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("r.md");
        std::fs::write(&f, long_doc()).unwrap();
        let mut v = View::new(load_file(&f).unwrap(), Some(f.clone()), true, Arc::new(|| {}));
        v.resize(800, 600, 1.0);
        v.prepare();
        v.jump_to_block(300);
        v.prepare();
        let block = v.block_at(v.scroll);
        v.reload();
        v.prepare();
        assert!(v.block_at(v.scroll).abs_diff(block) <= 1);
        std::fs::remove_file(&f).unwrap();
        v.reload();
        assert!(v.status_text().unwrap().contains("Cannot reload"));
        assert_eq!(v.doc.blocks.len(), 800);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn punctuation_after_bold_is_not_bold() {
        let v = view("**bold**, plain\n");
        let part = &v.shaped[&0].parts[0];
        let run = part.buf.layout_runs().next().unwrap();
        let comma = run.glyphs.iter().find(|g| g.start == 4).unwrap();
        assert_eq!(comma.font_weight, Weight::NORMAL);
        assert_eq!(meta_style(comma.metadata), 0);
        let d = run.glyphs.iter().find(|g| g.start == 3).unwrap();
        assert_eq!(d.font_weight, Weight::BOLD);
    }

    #[test]
    fn tables_fit_the_content_width() {
        let long = "word ".repeat(200);
        let mut v = view(&format!("| a | b |\n|---|---|\n| {long} | {long} |\n"));
        v.prepare();
        assert!(v.table_width(0) <= v.content_w + 0.5);
        assert!(v.shaped[&1].height > 200.0);
    }

    #[test]
    fn non_ascii_outside_embedded_fonts_loads_system_fonts_once() {
        let mut v = view("日本語のテキスト and an emoji 🎉\n");
        v.prepare();
        assert!(v.fonts.fs.db().faces().count() > 7);
    }

    #[test]
    fn embedded_coverage_does_not_scan_system_fonts() {
        let v = view("Café — naïve → ✓ Ελληνικά Русский\n");
        assert_eq!(v.fonts.fs.db().faces().count(), 7);
    }

    #[test]
    fn zoom_scales_layout_and_is_bounded() {
        let mut v = view(&long_doc());
        let before = v.heights[1];
        v.zoom_by(2.0);
        v.prepare();
        assert!(v.heights[1] > before * 1.5);
        for _ in 0..20 {
            v.zoom_by(2.0);
        }
        assert_eq!(v.s, 3.0);
        v.zoom_reset();
        assert_eq!(v.s, 1.0);
    }

    #[test]
    fn toc_toggle_keeps_content_and_refuses_without_headings() {
        let mut v = view("no headings\n");
        v.toggle_toc();
        assert!(!v.toc_open);
        let mut v = view(&long_doc());
        v.toggle_toc();
        assert!(v.toc_open);
        v.prepare();
        v.pointer_down(10.0, 12.0 + TOC_ROW * 5.5);
        v.prepare();
        assert_eq!(v.block_at(v.scroll + 40.0), v.doc.toc[5].block as usize);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release frame_times -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn frame_times() {
        let path = std::env::var("MARKDONK_BENCH").unwrap();
        let t = Instant::now();
        let doc = load_file(Path::new(&path)).unwrap();
        println!("parse {:?}, {} blocks", t.elapsed(), doc.blocks.len());
        let (w, h) = (3840, 2320);
        let mut v = View::new(doc, None, true, Arc::new(|| {}));
        v.resize(w, h, 2.0);
        let mut px = vec![0u32; (w * h) as usize];
        let t = Instant::now();
        v.draw(&mut px, w, h);
        println!("first frame {:?}", t.elapsed());
        let (mut total, mut worst) = (Duration::ZERO, Duration::ZERO);
        for _ in 0..300 {
            v.scroll_pixels(120.0);
            let t = Instant::now();
            v.draw(&mut px, w, h);
            let e = t.elapsed();
            total += e;
            worst = worst.max(e);
        }
        println!("scrolling: avg {:?}, worst {:?}", total / 300, worst);
    }
}
