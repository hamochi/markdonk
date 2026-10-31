//! Paints the view into the window buffer.

use crate::canvas::Canvas;
use crate::doc::{ALERT, CODE, Kind, LINK, Marker, STRIKE};
use crate::fonts::Fonts;
use crate::highlight;
use crate::view::{Part, RULE_H_PX, SIDEBAR, TOC_ROW, View, gap, image_size, meta_style, plain_buffer, token_style};
use cosmic_text::Buffer;

#[derive(Clone, Copy)]
pub struct Theme {
    bg: u32,
    text: u32,
    muted: u32,
    heading: u32,
    link: u32,
    border: u32,
    code_bg: u32,
    inline_bg: u32,
    inline_fg: u32,
    row_alt: u32,
    head_bg: u32,
    quote_bar: u32,
    select: u32,
    match_all: u32,
    match_now: u32,
    side_bg: u32,
    side_now: u32,
    bar: u32,
    pill_bg: u32,
    pill_fg: u32,
    alert: u32,
    tokens: [u32; 7],
}

const DARK: Theme = Theme {
    bg: 0x0f1216,
    text: 0xd5dae1,
    muted: 0x8b95a3,
    heading: 0xf1f4f8,
    link: 0x6cb0ff,
    border: 0x2c333c,
    code_bg: 0x181c22,
    inline_bg: 0x242a32,
    inline_fg: 0xe3e7ec,
    row_alt: 0x151920,
    head_bg: 0x1a1f26,
    quote_bar: 0x3a434e,
    select: 0x3f7fd0,
    match_all: 0xc99a2e,
    match_now: 0xf08a24,
    side_bg: 0x14181d,
    side_now: 0x222a33,
    bar: 0x4a5461,
    pill_bg: 0x262c34,
    pill_fg: 0xe3e7ec,
    alert: 0x6cb0ff,
    tokens: [0xd5dae1, 0x7a8594, 0x9fd28a, 0xe5b567, 0xf08fa0, 0x74cfd9, 0x8fb8ff],
};

const LIGHT: Theme = Theme {
    bg: 0xffffff,
    text: 0x24292f,
    muted: 0x626b76,
    heading: 0x11151a,
    link: 0x0a63d1,
    border: 0xd5dae0,
    code_bg: 0xf5f6f8,
    inline_bg: 0xeceef1,
    inline_fg: 0x24292f,
    row_alt: 0xf7f8fa,
    head_bg: 0xf0f2f4,
    quote_bar: 0xcdd3da,
    select: 0x4b93ee,
    match_all: 0xf4d65c,
    match_now: 0xf59a33,
    side_bg: 0xf5f6f8,
    side_now: 0xe3e8ee,
    bar: 0xa8b0ba,
    pill_bg: 0x2c333c,
    pill_fg: 0xf5f6f8,
    alert: 0x0a63d1,
    tokens: [0x24292f, 0x6a737d, 0x1a7f37, 0x9a6700, 0xcf222e, 0x0b7285, 0x6639ba],
};

fn glyph_color(t: &Theme, base: u32, style: u16) -> u32 {
    let token = token_style(style) as usize;
    if token != 0 {
        return t.tokens[token.min(highlight::FUNC as usize)];
    }
    if style & ALERT != 0 {
        t.alert
    } else if style & LINK != 0 {
        t.link
    } else if style & CODE != 0 {
        t.inline_fg
    } else {
        base
    }
}

/// A byte range of the block's text to tint behind the glyphs.
struct Tint {
    start: u32,
    end: u32,
    color: u32,
    alpha: u32,
}

fn draw_glyph(c: &mut Canvas, fonts: &mut Fonts, key: cosmic_text::CacheKey, x: i32, y: i32, color: u32) {
    let Some(g) = fonts.glyph(key) else { return };
    if g.width == 0 || g.height == 0 {
        return;
    }
    let (gx, gy) = (x + g.left, y - g.top);
    if g.color {
        c.rgba_bytes(gx, gy, g.width, g.height, &g.data);
    } else {
        c.mask(gx, gy, g.width, g.height, &g.data, color);
    }
}

fn draw_part(c: &mut Canvas, fonts: &mut Fonts, t: &Theme, part: &Part, ox: i32, oy: i32, base: u32, tints: &[Tint], s: f32) {
    let h = c.h;
    for run in part.buf.layout_runs() {
        let top = oy + run.line_top as i32;
        let height = run.line_height.ceil() as i32;
        if top + height < 0 {
            continue;
        }
        if top > h {
            break;
        }
        let line_off = part.base + part.line_starts.get(run.line_i).copied().unwrap_or(0);
        let baseline = oy + run.line_y.round() as i32;

        for tint in tints {
            let mut extent: Option<(f32, f32)> = None;
            for g in run.glyphs {
                let (gs, ge) = (line_off + g.start as u32, line_off + g.end as u32);
                if gs < tint.end && ge > tint.start {
                    extent = Some(match extent {
                        Some((a, b)) => (a.min(g.x), b.max(g.x + g.w)),
                        None => (g.x, g.x + g.w),
                    });
                }
            }
            if let Some((a, b)) = extent {
                let x = ox + a.floor() as i32;
                c.blend_rect(x, top, (b - a).ceil() as i32, height, tint.color, tint.alpha);
            }
        }

        for_each_styled_run(run.glyphs, CODE, |a, b| {
            let pad = (2.5 * s) as i32;
            let inset = (run.line_height * 0.13) as i32;
            let (x, w) = (ox + a as i32 - pad, (b - a) as i32 + 2 * pad);
            c.rrect(x, top + inset, w, height - 2 * inset, (4.0 * s) as i32, t.inline_bg);
        });

        for g in run.glyphs {
            let style = meta_style(g.metadata);
            let p = g.physical((ox as f32, baseline as f32), 1.0);
            draw_glyph(c, fonts, p.cache_key, p.x, p.y, glyph_color(t, base, style));
        }

        let thickness = s.round().max(1.0) as i32;
        for_each_styled_run(run.glyphs, LINK, |a, b| {
            c.rect(ox + a as i32, baseline + (2.0 * s) as i32, (b - a).ceil() as i32, thickness, t.link);
        });
        for_each_styled_run(run.glyphs, STRIKE, |a, b| {
            let y = baseline - (run.line_height * 0.2) as i32;
            c.rect(ox + a as i32, y, (b - a).ceil() as i32, thickness, base);
        });
    }
}

/// Calls `f` with the x-extent of each contiguous glyph range carrying `flag`.
fn for_each_styled_run(glyphs: &[cosmic_text::LayoutGlyph], flag: u16, mut f: impl FnMut(f32, f32)) {
    let mut open: Option<(f32, f32)> = None;
    for g in glyphs {
        if meta_style(g.metadata) & flag != 0 {
            open = Some(match open {
                Some((a, _)) => (a, g.x + g.w),
                None => (g.x, g.x + g.w),
            });
        } else if let Some((a, b)) = open.take() {
            f(a, b);
        }
    }
    if let Some((a, b)) = open {
        f(a, b);
    }
}

fn draw_plain(c: &mut Canvas, fonts: &mut Fonts, buf: &Buffer, x: i32, y: i32, color: u32) {
    for run in buf.layout_runs() {
        let baseline = y + run.line_y.round() as i32;
        for g in run.glyphs {
            let p = g.physical((x as f32, baseline as f32), 1.0);
            draw_glyph(c, fonts, p.cache_key, p.x, p.y, color);
        }
    }
}

impl View {
    /// Advances animation, lays out what's visible and paints one frame.
    pub fn draw(&mut self, px: &mut [u32], w: i32, h: i32) {
        self.tick();
        self.prepare();
        self.tick_timers();
        let t = if self.dark { DARK } else { LIGHT };
        let mut c = Canvas::new(px, w, h);
        c.clear(t.bg);

        if self.doc.blocks.is_empty() {
            let s = self.s;
            let buf = plain_buffer(&mut self.fonts, "This document is empty", 16.0 * s, 24.0 * s, false, false);
            draw_plain(&mut c, &mut self.fonts, &buf, self.content_x as i32, (32.0 * s) as i32, t.muted);
        } else {
            let mut i = self.block_at(self.scroll);
            while i < self.doc.blocks.len() && self.ys[i] < self.scroll + h as f64 {
                self.draw_block(&mut c, &t, i);
                i += 1;
            }
        }
        self.draw_scrollbar(&mut c, &t);
        if self.toc_open {
            self.draw_sidebar(&mut c, &t);
        }
        self.draw_search_bar(&mut c, &t);
        self.draw_status(&mut c, &t);
        self.needs_redraw = false;
    }

    fn tints_for(&self, i: u32, t: &Theme) -> Vec<Tint> {
        let mut tints = Vec::new();
        if let Some(search) = &self.search {
            let from = search.matches.partition_point(|m| m.0 < i);
            for (n, m) in search.matches[from..].iter().take_while(|m| m.0 == i).enumerate() {
                let current = from + n == search.current;
                tints.push(Tint {
                    start: m.1,
                    end: m.2,
                    color: if current { t.match_now } else { t.match_all },
                    alpha: if current { 200 } else { 110 },
                });
            }
        }
        if let Some((a, b)) = self.selection() {
            if i >= a.block && i <= b.block && a != b {
                let start = if i == a.block { a.off } else { 0 };
                let end = if i == b.block { b.off } else { u32::MAX };
                tints.push(Tint { start, end, color: t.select, alpha: 100 });
            }
        }
        tints
    }

    fn draw_block(&mut self, c: &mut Canvas, t: &Theme, i: usize) {
        let s = self.s;
        let tints = self.tints_for(i as u32, t);
        let (bx, cw, gap_px, top_f, height) = {
            let b = &self.doc.blocks[i];
            let gap_px = gap(&self.doc.blocks, i) * s;
            (self.block_x(b), self.content_x + self.content_w - self.block_x(b), gap_px, self.ys[i], self.heights[i])
        };
        let y0 = (top_f - self.scroll).round() as i32;
        let y = (top_f + gap_px as f64 - self.scroll).round() as i32;
        let content_h = (height - gap_px).ceil() as i32;
        let x = bx.round() as i32;
        let cw_i = cw.round() as i32;
        let radius = (6.0 * s) as i32;

        // Quote bars run through the gap when the block above is in the same quote.
        let (quotes, level) = (self.doc.blocks[i].quotes, self.doc.blocks[i].level);
        for lvl in 0..level.min(16) {
            if quotes & (1 << lvl) != 0 {
                let shares = i > 0 && self.doc.blocks[i - 1].quotes & (1 << lvl) != 0;
                let start = if shares { y0 } else { y };
                let bar_x = (self.content_x + lvl as f32 * crate::view::INDENT * s + 2.0 * s) as i32;
                c.rect(bar_x, start, (3.0 * s).round() as i32, y + content_h - start, t.quote_bar);
            }
        }
        let base = match self.doc.blocks[i].kind {
            Kind::Heading(_) => t.heading,
            _ if quotes != 0 => t.muted,
            _ => t.text,
        };

        match &self.doc.blocks[i].kind {
            Kind::Code { first, last, .. } => {
                let (first, last) = (*first, *last);
                // Overdraw past the clip so only a chunk's outer corners round off.
                c.set_clip(x, y, x + cw_i, y + content_h);
                let extend_top = if first { 0 } else { 2 * radius };
                let extend_bot = if last { 0 } else { 2 * radius };
                c.rrect(x, y - extend_top, cw_i, content_h + extend_top + extend_bot, radius, t.code_bg);
                c.reset_clip();
            }
            Kind::Rule => {
                c.rect(x, y + ((RULE_H_PX * s) as i32) / 2, cw_i, s.round().max(1.0) as i32, t.border);
            }
            Kind::Row { table, row, .. } => {
                let (table, row) = (*table, *row);
                let rows = self.doc.tables[table as usize].rows;
                let tw = self.table_width(table).round() as i32;
                let line = s.round().max(1.0) as i32;
                let fill = if row == 0 {
                    Some(t.head_bg)
                } else if row % 2 == 0 {
                    Some(t.row_alt)
                } else {
                    None
                };
                if let Some(fill) = fill {
                    c.rect(x, y, tw, content_h, fill);
                }
                c.rect(x, y, tw, line, t.border);
                if row + 1 == rows {
                    c.rect(x, y + content_h - line, tw, line, t.border);
                }
                let mut cx = x as f32;
                c.rect(x, y, line, content_h, t.border);
                for col in self.table_col_widths(table) {
                    cx += col;
                    c.rect(cx.round() as i32 - line, y, line, content_h, t.border);
                }
            }
            Kind::Heading(1 | 2) => {
                c.rect(x, y + content_h - s.round().max(1.0) as i32, cw_i, s.round().max(1.0) as i32, t.border);
            }
            Kind::Image { width, height, .. } => {
                let (iw, ih) = image_size((*width, *height), s, cw);
                let (iw, ih) = (iw as i32, ih as i32);
                if let Some(p) = self.images.ready.get(&(i as u32)) {
                    if p.width as i32 == iw && p.height as i32 == ih {
                        c.argb(x, y, iw, ih, &p.data);
                    } else {
                        // Stale size during a resize or zoom; the correct one is on its way.
                        c.rrect(x, y, iw, ih, radius, t.code_bg);
                    }
                } else {
                    c.rrect(x, y, iw, ih, radius, t.code_bg);
                    let failed = self.images.is_failed(i as u32);
                    if let Some(part) = self.shaped.get(&(i as u32)).and_then(|sh| sh.parts.first()) {
                        if failed {
                            draw_part(c, &mut self.fonts, t, part, x + part.x as i32, y + part.y as i32, t.muted, &[], s);
                        }
                    }
                }
                return;
            }
            _ => {}
        }

        self.draw_marker(c, t, i, x, y);

        let Some(shaped) = self.shaped.get(&(i as u32)) else { return };
        if matches!(self.doc.blocks[i].kind, Kind::Row { .. }) {
            c.set_clip(x, y, x + cw_i, y + content_h);
        }
        for part in &shaped.parts {
            let (ox, oy) = (bx + part.x, y as f32 + part.y);
            draw_part(c, &mut self.fonts, t, part, ox.round() as i32, oy.round() as i32, base, &tints, s);
        }
        c.reset_clip();
    }

    fn draw_marker(&mut self, c: &mut Canvas, t: &Theme, i: usize, x: i32, y: i32) {
        let s = self.s;
        let marker = self.doc.blocks[i].marker;
        if marker == Marker::None {
            return;
        }
        let Some(shaped) = self.shaped.get(&(i as u32)) else { return };
        let first = shaped.parts.first().and_then(|p| {
            let py = p.y;
            p.buf.layout_runs().next().map(|r| (py + r.line_top + r.line_height / 2.0, py + r.line_y))
        });
        let (center, baseline) = first.unwrap_or((13.0 * s, 18.0 * s));
        let cy = y as f32 + center;
        let fx = x as f32;
        match marker {
            Marker::None => {}
            Marker::Bullet(depth) => {
                let cx = fx - 13.0 * s;
                let r = 2.6 * s;
                if depth == 0 {
                    c.circle(cx, cy, r, t.muted);
                } else if depth == 1 {
                    c.circle(cx, cy, r, t.muted);
                    c.circle(cx, cy, r - 1.2 * s, t.bg);
                } else {
                    let side = (4.4 * s) as i32;
                    c.rect(cx as i32 - side / 2, cy as i32 - side / 2, side, side, t.muted);
                }
            }
            Marker::Task(done) => {
                let size = (15.0 * s) as i32;
                let bx = (fx - 21.0 * s) as i32;
                let by = cy as i32 - size / 2;
                let radius = (3.0 * s) as i32;
                if done {
                    c.rrect(bx, by, size, size, radius, t.link);
                    let (fx, fy) = (bx as f32, by as f32);
                    let u = size as f32;
                    c.line((fx + u * 0.24, fy + u * 0.52), (fx + u * 0.43, fy + u * 0.71), 1.8 * s, t.bg);
                    c.line((fx + u * 0.43, fy + u * 0.71), (fx + u * 0.77, fy + u * 0.30), 1.8 * s, t.bg);
                } else {
                    let line = s.round().max(1.0) as i32;
                    c.rrect(bx, by, size, size, radius, t.muted);
                    c.rrect(bx + line, by + line, size - 2 * line, size - 2 * line, (radius - line).max(0), t.bg);
                }
            }
            Marker::Number(_) => {
                if let Some(buf) = &shaped.marker {
                    let w = crate::view::buffer_width(buf);
                    let run_baseline = buf.layout_runs().next().map_or(0.0, |r| r.line_y);
                    let mx = (fx - w - 7.0 * s) as i32;
                    let my = y + (baseline - run_baseline) as i32;
                    draw_plain(c, &mut self.fonts, buf, mx, my, t.muted);
                }
            }
        }
    }

    fn draw_scrollbar(&self, c: &mut Canvas, t: &Theme) {
        let total = self.total_height();
        if total <= self.h as f64 {
            return;
        }
        let ws = (self.s).max(1.0);
        let h = self.h as f64;
        let thumb = (h * h / total).max(36.0 * ws as f64);
        let y = (self.scroll / self.max_scroll().max(1.0)) * (h - thumb);
        let w = (6.0 * ws) as i32;
        let x = self.w - w - (3.0 * ws) as i32;
        c.rrect(x, y as i32, w, thumb as i32, w / 2, t.bar);
    }

    fn draw_sidebar(&mut self, c: &mut Canvas, t: &Theme) {
        let current = self.current_toc();
        let ws = self.win_scale();
        let width = (SIDEBAR * ws) as i32;
        c.rect(0, 0, width, self.h, t.side_bg);
        c.rect(width - ws.round().max(1.0) as i32, 0, ws.round().max(1.0) as i32, self.h, t.border);
        c.set_clip(0, 0, width - (8.0 * ws) as i32, self.h);
        let row = TOC_ROW * ws;
        let top = 12.0 * ws;
        let first = ((self.toc_scroll - top) / row).floor().max(0.0) as usize;
        let count = (self.h as f32 / row) as usize + 2;
        if self.toc_bufs.len() > count * 4 {
            self.toc_bufs.clear();
        }
        for idx in first..(first + count).min(self.doc.toc.len()) {
            let y = top + idx as f32 * row - self.toc_scroll;
            let level = self.doc.toc[idx].level;
            let block = self.doc.toc[idx].block;
            if current == Some(idx) {
                c.rrect((6.0 * ws) as i32, y as i32, width - (14.0 * ws) as i32, row as i32, (5.0 * ws) as i32, t.side_now);
            }
            let buf = match self.toc_bufs.remove(&(idx as u32)) {
                Some(buf) => buf,
                None => {
                    let text = self.doc.blocks[block as usize].text.replace('\n', " ");
                    plain_buffer(&mut self.fonts, &text, 13.5 * ws, row, level == 1, false)
                }
            };
            let x = ((14.0 + (level.saturating_sub(1)) as f32 * 12.0) * ws) as i32;
            let color = if current == Some(idx) { t.heading } else { t.muted };
            draw_plain(c, &mut self.fonts, &buf, x, y as i32, color);
            self.toc_bufs.insert(idx as u32, buf);
        }
        c.reset_clip();
    }

    fn draw_search_bar(&mut self, c: &mut Canvas, t: &Theme) {
        let Some(search) = &self.search else { return };
        let ws = self.win_scale();
        let query = if search.query.is_empty() && search.editing { "Search…".to_string() } else { search.query.clone() };
        let count = if search.query.is_empty() {
            String::new()
        } else if search.matches.is_empty() {
            "No matches".to_string()
        } else {
            format!("{}/{}", search.current + 1, search.matches.len())
        };
        let (empty, editing) = (search.query.is_empty(), search.editing);
        let w = (380.0_f32 * ws).min(self.w as f32 - 32.0 * ws) as i32;
        let h = (40.0 * ws) as i32;
        let x = self.w - w - (16.0 * ws) as i32;
        let y = (12.0 * ws) as i32;
        c.rrect(x - 1, y - 1, w + 2, h + 2, (8.0 * ws) as i32, t.border);
        c.rrect(x, y, w, h, (8.0 * ws) as i32, t.code_bg);
        let line = 24.0 * ws;
        let text_y = y + ((h as f32 - line) / 2.0) as i32;
        let count_buf = plain_buffer(&mut self.fonts, &count, 13.0 * ws, line, false, false);
        let count_w = crate::view::buffer_width(&count_buf) as i32;
        let query_buf = plain_buffer(&mut self.fonts, &query, 14.5 * ws, line, false, false);
        let query_w = crate::view::buffer_width(&query_buf) as i32;
        let pad = (12.0 * ws) as i32;
        let right = x + w - pad - count_w - pad;
        // Long queries scroll left so the caret stays visible.
        let tx = (x + pad).min(right - query_w);
        c.set_clip(x + pad, y, right, y + h);
        draw_plain(c, &mut self.fonts, &query_buf, tx, text_y, if empty { t.muted } else { t.text });
        if editing {
            let caret_x = if empty { tx } else { tx + query_w + (1.0 * ws) as i32 };
            c.rect(caret_x, text_y + (3.0 * ws) as i32, ws.round().max(1.0) as i32, (line - 6.0 * ws) as i32, t.link);
        }
        c.reset_clip();
        draw_plain(c, &mut self.fonts, &count_buf, x + w - pad - count_w, text_y, t.muted);
    }

    fn draw_status(&mut self, c: &mut Canvas, t: &Theme) {
        let Some(text) = self.status_text().map(str::to_string) else { return };
        let ws = self.win_scale();
        let buf = plain_buffer(&mut self.fonts, &text, 13.0 * ws, 20.0 * ws, false, false);
        let side = if self.toc_open { (SIDEBAR * ws) as i32 } else { 0 };
        let pad = (10.0 * ws) as i32;
        let max_w = self.w - side - (24.0 * ws) as i32;
        let w = (crate::view::buffer_width(&buf) as i32 + 2 * pad).min(max_w);
        let h = (30.0 * ws) as i32;
        let x = side + (12.0 * ws) as i32;
        let y = self.h - h - (12.0 * ws) as i32;
        c.rrect(x, y, w, h, (6.0 * ws) as i32, t.pill_bg);
        c.set_clip(x + pad, y, x + w - pad, y + h);
        draw_plain(c, &mut self.fonts, &buf, x + pad, y + (5.0 * ws) as i32, t.pill_fg);
        c.reset_clip();
    }
}
