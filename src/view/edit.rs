//! Inline editing. The block under the caret shows raw Markdown; everything else
//! stays rendered. The source text is only held in memory while editing, and the
//! document is re-parsed when the caret leaves a block, not on every keystroke.

use super::*;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write as _;

const BLINK: Duration = Duration::from_millis(530);
const MAX_UNDO: usize = 2000;
const COALESCE: Duration = Duration::from_millis(800);
const QUIT_WINDOW: Duration = Duration::from_secs(3);

struct Change {
    pos: usize,
    old: String,
    new: String,
    caret_before: usize,
    caret_after: usize,
    at: Instant,
}

pub struct Edit {
    pub source: String,
    /// Byte range of `source` shown raw in `block`.
    pub active: (usize, usize),
    pub block: usize,
    pub caret: usize,
    goal_x: Option<f32>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    saved_depth: Option<usize>,
    saved_hash: u64,
    crlf: bool,
    changed_on_disk: bool,
    overwrite_armed: bool,
    pub blink_on: bool,
    blink_at: Instant,
}

impl Edit {
    pub fn dirty(&self) -> bool {
        self.saved_depth != Some(self.undo.len())
    }
}

struct RunInfo {
    line: usize,
    start: usize,
    end: usize,
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn floor_boundary(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Writes through a temporary file and rename, so a failed save can't truncate
/// the original. Symlinks are followed, keeping them intact.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path)?;
    let perms = std::fs::metadata(&target)?.permissions();
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.markdonk-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(bytes)?;
        file.set_permissions(perms)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Maps rendered text to a raw offset by walking it through the source in order,
/// which skips markup such as `**`. Links can make this land slightly off.
fn raw_offset(raw: &str, rendered_prefix: &str) -> usize {
    let mut at = 0;
    for c in rendered_prefix.chars().filter(|c| !c.is_whitespace()) {
        if let Some(i) = raw[at..].find(c) {
            at += i + c.len_utf8();
        }
    }
    at
}

impl View {
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn enter_edit(&mut self) {
        let Some(path) = self.path.clone() else {
            self.set_status("Text from stdin can't be edited");
            return;
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.set_status(format!("Cannot edit: {err}"));
                return;
            }
        };
        let saved_hash = hash_bytes(&bytes);
        let Ok(text) = String::from_utf8(bytes) else {
            self.set_status("Cannot edit: the file isn't valid UTF-8");
            return;
        };
        let crlf = text.contains("\r\n");
        let source = if crlf { text.replace("\r\n", "\n") } else { text };
        // Start at the selection if there is one, otherwise the top of the view.
        let block = match self.selection() {
            Some((a, _)) => a.block as usize,
            None => self.block_at(self.scroll + (TOP_PAD * self.s) as f64),
        };
        let caret = self.doc.blocks.get(block).map_or(0, |b| b.src.0 as usize).min(source.len());
        self.edit = Some(Box::new(Edit {
            source,
            active: (0, 0),
            block: 0,
            caret,
            goal_x: None,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_depth: Some(0),
            saved_hash,
            crlf,
            changed_on_disk: false,
            overwrite_armed: false,
            blink_on: true,
            blink_at: Instant::now() + BLINK,
        }));
        self.activate(caret, Some((40.0 * self.s).min(self.h as f32 / 3.0)));
        self.set_status("Editing. Ctrl+S saves, Esc finishes");
    }

    /// Esc: leaves edit mode, but never throws away changes on a stray key press.
    pub fn edit_escape(&mut self) {
        if self.sel.take().is_some() {
            self.needs_redraw = true;
            return;
        }
        if self.edit.as_ref().is_some_and(|e| e.dirty()) {
            self.set_status("Unsaved changes. Ctrl+S saves, Ctrl+Shift+Q discards them");
            return;
        }
        self.leave_edit();
    }

    pub fn edit_discard(&mut self) {
        self.edit = None;
        self.sel = None;
        self.reload();
        self.set_status("Changes discarded");
    }

    fn leave_edit(&mut self) {
        let Some(edit) = self.edit.take() else { return };
        let (block, ratio) = self.anchor();
        let base = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        let doc = doc::parse(&edit.source, base.as_deref());
        drop(edit);
        self.replace_doc(doc);
        self.restore_anchor(block, ratio);
        self.run_search(false);
        trim_heap();
    }

    /// Whether the window may close now. With unsaved changes the first attempt
    /// only warns; a second one within a few seconds goes through.
    pub fn request_quit(&mut self) -> bool {
        if !self.edit.as_ref().is_some_and(|e| e.dirty()) {
            return true;
        }
        if self.quit_armed.is_some_and(|at| at.elapsed() < QUIT_WINDOW) {
            return true;
        }
        self.quit_armed = Some(Instant::now());
        self.set_status("Unsaved changes. Close again to discard them, or press Ctrl+S");
        false
    }

    /// Re-parses the source and opens the block containing `pos` for editing.
    /// `screen_y` keeps the caret at that height; otherwise it's just revealed.
    fn activate(&mut self, pos: usize, screen_y: Option<f32>) {
        let base = self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf));
        let Some(edit) = &mut self.edit else { return };
        let pos = floor_boundary(&edit.source, pos);
        let mut new_doc = doc::parse(&edit.source, base.as_deref());
        let src = &edit.source;
        let covering = new_doc.blocks.iter().find(|b| {
            let (start, end) = (b.src.0 as usize, b.src.1 as usize);
            pos >= start && pos <= end && !src[start..end].trim().is_empty()
        });
        let (start, end) = match covering {
            Some(b) => {
                let (start, end) = (b.src.0 as usize, b.src.1 as usize);
                (start, start + src[start..end].trim_end_matches('\n').len())
            }
            // Blank lines, link definitions and other text with no rendered block.
            None => {
                let start = src[..pos].rfind('\n').map_or(0, |i| i + 1);
                let end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
                (start, end)
            }
        };
        let block = doc::activate(&mut new_doc, (start as u32, end as u32), &src[start..end]);
        edit.active = (start, end);
        edit.block = block;
        edit.caret = pos.clamp(start, end);
        self.replace_doc(new_doc);
        self.ensure_shaped(block);
        if self.ys_dirty {
            self.rebuild_ys();
        }
        match screen_y {
            Some(y) => {
                let caret_y = self.caret_doc_y();
                self.scroll = (caret_y - y as f64).clamp(0.0, self.max_scroll());
                self.target = self.scroll;
            }
            None => self.reveal_caret(),
        }
        if self.search.is_some() {
            self.run_search(false);
        }
        self.touch_caret();
    }

    /// Swaps in a parsed document, moving over decoded images so they don't flash.
    fn replace_doc(&mut self, doc: Doc) {
        let mut pixels = Vec::new();
        for (block, p) in self.images.take_ready() {
            if let Some(Kind::Image { path, .. }) = self.doc.blocks.get(block as usize).map(|b| &b.kind) {
                pixels.push((path.clone(), p));
            }
        }
        self.images.reset();
        self.doc = doc;
        for (i, b) in self.doc.blocks.iter().enumerate() {
            if let Kind::Image { path, .. } = &b.kind {
                if let Some(at) = pixels.iter().position(|(q, _)| q == path) {
                    self.images.insert_ready(i as u32, pixels.swap_remove(at).1);
                }
            }
        }
        self.sel = None;
        self.selecting = false;
        self.hover_link = None;
        self.toc_current = None;
        self.stick_end = false;
        self.fling = 0.0;
        self.scroll = 0.0;
        self.target = 0.0;
        self.heights.clear();
        self.ys.clear();
        self.relayout(true);
    }

    fn touch_caret(&mut self) {
        if let Some(e) = &mut self.edit {
            e.blink_on = true;
            e.blink_at = Instant::now() + BLINK;
        }
        self.needs_redraw = true;
    }

    pub fn blink_due(&self) -> Option<Instant> {
        self.edit.as_ref().map(|e| e.blink_at)
    }

    pub fn tick_blink(&mut self) {
        if let Some(e) = &mut self.edit {
            if Instant::now() >= e.blink_at {
                e.blink_on = !e.blink_on;
                e.blink_at = Instant::now() + BLINK;
                self.needs_redraw = true;
            }
        }
    }

    fn runs(&self) -> Vec<RunInfo> {
        let Some(edit) = &self.edit else { return Vec::new() };
        let Some(part) = self.shaped.get(&(edit.block as u32)).and_then(|s| s.parts.first()) else {
            return Vec::new();
        };
        part.buf
            .layout_runs()
            .map(|r| RunInfo {
                line: r.line_i,
                start: r.glyphs.first().map_or(0, |g| g.start),
                end: r.glyphs.last().map_or(0, |g| g.end),
            })
            .collect()
    }

    /// The caret's run index, its x within the line, and its line's top and height.
    pub fn caret_layout(&self) -> Option<(usize, f32, f32, f32)> {
        let edit = self.edit.as_ref()?;
        let shaped = self.shaped.get(&(edit.block as u32))?;
        let part = shaped.parts.first()?;
        let rel = (edit.caret.clamp(edit.active.0, edit.active.1) - edit.active.0) as u32;
        let line = part.line_starts.partition_point(|&s| s <= rel).saturating_sub(1);
        let col = (rel - part.line_starts.get(line).copied().unwrap_or(0)) as usize;
        let runs: Vec<_> = part.buf.layout_runs().collect();
        let on_line: Vec<usize> = (0..runs.len()).filter(|&i| runs[i].line_i == line).collect();
        let index = on_line
            .iter()
            .copied()
            .find(|&i| runs[i].glyphs.last().is_some_and(|g| col < g.end))
            .or(on_line.last().copied())?;
        let run = &runs[index];
        let x = run
            .glyphs
            .iter()
            .find(|g| col >= g.start && col < g.end)
            .map(|g| g.x)
            .or(run.glyphs.last().map(|g| g.x + g.w))
            .unwrap_or(0.0);
        Some((index, x, run.line_top, run.line_height))
    }

    fn caret_doc_y(&self) -> f64 {
        let Some(edit) = &self.edit else { return 0.0 };
        let top = self.ys.get(edit.block).copied().unwrap_or(0.0)
            + (gap(&self.doc.blocks, edit.block) * self.s) as f64;
        let part_y = self.shaped.get(&(edit.block as u32)).and_then(|s| s.parts.first()).map_or(0.0, |p| p.y);
        top + part_y as f64 + self.caret_layout().map_or(0.0, |c| c.2 as f64)
    }

    fn reveal_caret(&mut self) {
        let Some(block) = self.edit.as_ref().map(|e| e.block) else { return };
        self.ensure_shaped(block);
        if self.ys_dirty {
            self.rebuild_ys();
        }
        let y = self.caret_doc_y();
        let line = self.caret_layout().map_or(24.0, |c| c.3) as f64;
        let margin = (48.0 * self.s) as f64;
        if y < self.scroll + margin {
            self.scroll_instant(y - margin);
        } else if y + line > self.scroll + self.h as f64 - margin {
            self.scroll_instant(y + line + margin - self.h as f64);
        }
    }

    fn after_caret_move(&mut self, extend: bool, before: usize) {
        let Some(edit) = &self.edit else { return };
        let (block, base, caret) = (edit.block as u32, edit.active.0, edit.caret);
        if extend {
            let anchor = match self.sel {
                Some((a, _)) => a,
                None => Pos { block, off: (before - base) as u32 },
            };
            self.sel = Some((anchor, Pos { block, off: (caret - base) as u32 }));
        } else {
            self.sel = None;
        }
        self.reveal_caret();
        self.touch_caret();
    }

    pub fn caret_left(&mut self, word: bool, extend: bool) {
        let Some(edit) = &mut self.edit else { return };
        let before = edit.caret;
        if !extend && self.sel.is_some() {
            let (a, _) = self.selection_ordered();
            edit_set(&mut self.edit, a);
            self.after_caret_move(false, before);
            return;
        }
        let head = &edit.source[edit.active.0..edit.caret];
        if head.is_empty() {
            self.move_to_previous_block();
            return;
        }
        let step = if word { previous_word(head) } else { head.char_indices().next_back().map_or(0, |(i, _)| i) };
        edit.caret = edit.active.0 + step;
        edit.goal_x = None;
        self.after_caret_move(extend, before);
    }

    pub fn caret_right(&mut self, word: bool, extend: bool) {
        let Some(edit) = &mut self.edit else { return };
        let before = edit.caret;
        if !extend && self.sel.is_some() {
            let (_, b) = self.selection_ordered();
            edit_set(&mut self.edit, b);
            self.after_caret_move(false, before);
            return;
        }
        let tail = &edit.source[edit.caret..edit.active.1];
        if tail.is_empty() {
            self.move_to_next_block();
            return;
        }
        let step = if word { next_word(tail) } else { tail.chars().next().map_or(0, char::len_utf8) };
        edit.caret += step;
        edit.goal_x = None;
        self.after_caret_move(extend, before);
    }

    /// Absolute source offsets of the selection, in order.
    fn selection_ordered(&self) -> (usize, usize) {
        let Some(edit) = &self.edit else { return (0, 0) };
        let base = edit.active.0;
        match self.selection() {
            Some((a, b)) => (base + a.off as usize, base + b.off as usize),
            None => (edit.caret, edit.caret),
        }
    }

    pub fn caret_vertical(&mut self, down: bool, extend: bool) {
        let Some((index, x, _, _)) = self.caret_layout() else { return };
        let runs = self.runs();
        let Some(edit) = &mut self.edit else { return };
        let before = edit.caret;
        let goal = *edit.goal_x.get_or_insert(x);
        let target = if down { index + 1 } else { index.wrapping_sub(1) };
        if target >= runs.len() {
            if extend {
                return;
            }
            if down { self.move_to_next_block() } else { self.move_to_previous_block() }
            return;
        }
        let run = &runs[target];
        let block = edit.block as u32;
        let part = self.shaped.get(&block).and_then(|s| s.parts.first());
        let start = part.and_then(|p| p.line_starts.get(run.line).copied()).unwrap_or(0) as usize;
        let mut col = run.end;
        if let Some(r) = part.and_then(|p| p.buf.layout_runs().nth(target)) {
            for g in r.glyphs {
                if goal < g.x + g.w / 2.0 {
                    col = g.start;
                    break;
                }
            }
        }
        edit.caret = edit.active.0 + start + col;
        self.after_caret_move(extend, before);
    }

    pub fn caret_line_edge(&mut self, end: bool, extend: bool) {
        let Some((index, ..)) = self.caret_layout() else { return };
        let runs = self.runs();
        let Some(run) = runs.get(index) else { return };
        let Some(edit) = &mut self.edit else { return };
        let before = edit.caret;
        let part = self.shaped.get(&(edit.block as u32)).and_then(|s| s.parts.first());
        let start = part.and_then(|p| p.line_starts.get(run.line).copied()).unwrap_or(0) as usize;
        edit.caret = edit.active.0 + start + if end { run.end } else { run.start };
        edit.goal_x = None;
        self.after_caret_move(extend, before);
    }

    fn move_to_next_block(&mut self) {
        let Some(edit) = &self.edit else { return };
        let end = edit.active.1;
        let rest = &edit.source[end..];
        let skipped = rest.len() - rest.trim_start_matches(['\n', ' ', '\t']).len();
        if end + skipped >= edit.source.len() {
            return;
        }
        let target = end + skipped;
        let y = self.caret_screen_y();
        self.activate(target, y);
    }

    fn move_to_previous_block(&mut self) {
        let Some(edit) = &self.edit else { return };
        let head = edit.source[..edit.active.0].trim_end();
        if head.is_empty() && edit.active.0 == 0 {
            return;
        }
        let target = head.len();
        let y = self.caret_screen_y();
        self.activate(target, y);
    }

    fn caret_screen_y(&self) -> Option<f32> {
        let y = self.caret_doc_y() - self.scroll;
        (y >= 0.0 && y <= self.h as f64).then_some(y as f32)
    }

    /// Replaces `range` of the source, recording it for undo.
    fn change(&mut self, range: (usize, usize), text: &str, typing: bool) {
        let Some(edit) = &mut self.edit else { return };
        let old = edit.source[range.0..range.1].to_string();
        let caret_before = edit.caret;
        let caret_after = range.0 + text.len();
        let now = Instant::now();
        let merged = typing && old.is_empty() && !text.contains(char::is_whitespace) && {
            let at_saved = edit.saved_depth == Some(edit.undo.len());
            match edit.undo.last_mut() {
                Some(last)
                    if !at_saved
                        && now.duration_since(last.at) < COALESCE
                        && last.old.is_empty()
                        && last.pos + last.new.len() == range.0 =>
                {
                    last.new.push_str(text);
                    last.caret_after = caret_after;
                    last.at = now;
                    true
                }
                _ => false,
            }
        };
        if !merged {
            if edit.saved_depth.is_some_and(|d| d > edit.undo.len()) {
                edit.saved_depth = None;
            }
            edit.undo.push(Change { pos: range.0, old, new: text.to_string(), caret_before, caret_after, at: now });
            if edit.undo.len() > MAX_UNDO {
                edit.undo.remove(0);
                edit.saved_depth = edit.saved_depth.and_then(|d| d.checked_sub(1));
            }
        }
        edit.redo.clear();
        self.apply(range, text, caret_after);
    }

    fn apply(&mut self, range: (usize, usize), text: &str, caret: usize) {
        let Some(edit) = &mut self.edit else { return };
        edit.source.replace_range(range.0..range.1, text);
        edit.caret = caret;
        edit.goal_x = None;
        edit.overwrite_armed = false;
        self.sel = None;
        let inside = range.0 >= edit.active.0 && range.1 <= edit.active.1;
        if inside {
            edit.active.1 = edit.active.1 - (range.1 - range.0) + text.len();
        }
        if inside && caret >= edit.active.0 && caret <= edit.active.1 {
            let block = edit.block;
            let raw: Box<str> = edit.source[edit.active.0..edit.active.1].into();
            self.doc.blocks[block].text = raw;
            self.doc.blocks[block].src.1 = edit.active.1 as u32;
            self.shaped.remove(&(block as u32));
            self.reveal_caret();
            if self.search.is_some() {
                self.run_search(false);
            }
            self.touch_caret();
        } else {
            let y = self.caret_screen_y();
            self.activate(caret, y);
        }
    }

    fn selected_range(&self) -> Option<(usize, usize)> {
        let (a, b) = self.selection_ordered();
        (self.sel.is_some() && a != b).then_some((a, b))
    }

    pub fn edit_insert(&mut self, text: &str) {
        let Some(edit) = &self.edit else { return };
        let caret = edit.caret;
        let range = self.selected_range().unwrap_or((caret, caret));
        self.change(range, text, true);
    }

    pub fn edit_paste(&mut self) {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let text = self.clipboard.as_mut().and_then(|c| c.get_text().ok());
        match text {
            Some(text) => {
                let text = text.replace("\r\n", "\n");
                let Some(edit) = &self.edit else { return };
                let caret = edit.caret;
                let range = self.selected_range().unwrap_or((caret, caret));
                self.change(range, &text, false);
            }
            None => self.set_status("Nothing to paste"),
        }
    }

    pub fn edit_cut(&mut self) {
        if let Some(range) = self.selected_range() {
            self.copy();
            self.change(range, "", false);
        }
    }

    pub fn edit_backspace(&mut self, word: bool) {
        if let Some(range) = self.selected_range() {
            self.change(range, "", false);
            return;
        }
        let Some(edit) = &self.edit else { return };
        let caret = edit.caret;
        let head = &edit.source[..caret];
        if head.is_empty() {
            return;
        }
        let start = if word { previous_word(head) } else { head.char_indices().next_back().map_or(0, |(i, _)| i) };
        // At the block's edge this also joins it to the block above.
        self.change((start, caret), "", false);
    }

    pub fn edit_delete(&mut self, word: bool) {
        if let Some(range) = self.selected_range() {
            self.change(range, "", false);
            return;
        }
        let Some(edit) = &self.edit else { return };
        let caret = edit.caret;
        let tail = &edit.source[caret..];
        if tail.is_empty() {
            return;
        }
        let step = if word { next_word(tail) } else { tail.chars().next().map_or(0, char::len_utf8) };
        self.change((caret, caret + step), "", false);
    }

    pub fn edit_select_block(&mut self) {
        let Some(edit) = &mut self.edit else { return };
        let block = edit.block as u32;
        let len = (edit.active.1 - edit.active.0) as u32;
        edit.caret = edit.active.1;
        self.sel = Some((Pos { block, off: 0 }, Pos { block, off: len }));
        self.touch_caret();
    }

    pub fn edit_undo(&mut self) {
        let Some(edit) = &mut self.edit else { return };
        let Some(c) = edit.undo.pop() else { return };
        let (range, text, caret) = ((c.pos, c.pos + c.new.len()), c.old.clone(), c.caret_before);
        edit.redo.push(c);
        self.apply(range, &text, caret);
    }

    pub fn edit_redo(&mut self) {
        let Some(edit) = &mut self.edit else { return };
        let Some(c) = edit.redo.pop() else { return };
        let (range, text, caret) = ((c.pos, c.pos + c.old.len()), c.new.clone(), c.caret_after);
        edit.undo.push(c);
        self.apply(range, &text, caret);
    }

    pub fn edit_save(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let Some(edit) = &mut self.edit else { return };
        if edit.changed_on_disk && !edit.overwrite_armed {
            edit.overwrite_armed = true;
            self.set_status("The file changed on disk. Press Ctrl+S again to overwrite it");
            return;
        }
        let bytes = if edit.crlf { edit.source.replace('\n', "\r\n").into_bytes() } else { edit.source.clone().into_bytes() };
        match write_atomic(&path, &bytes) {
            Ok(()) => {
                edit.saved_hash = hash_bytes(&bytes);
                edit.saved_depth = Some(edit.undo.len());
                edit.changed_on_disk = false;
                edit.overwrite_armed = false;
                self.quit_armed = None;
                self.set_status("Saved");
            }
            Err(err) => self.set_status(format!("Could not save: {err}")),
        }
    }

    /// The file watcher fired while editing. Our own saves are recognised by
    /// content; outside changes are loaded if nothing here would be lost.
    pub(super) fn edit_disk_changed(&mut self) {
        let Some(path) = self.path.clone() else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        let hash = hash_bytes(&bytes);
        let Some(edit) = &mut self.edit else { return };
        if hash == edit.saved_hash {
            return;
        }
        if edit.dirty() {
            edit.changed_on_disk = true;
            self.set_status("The file changed on disk. Ctrl+S would overwrite those changes");
            return;
        }
        let Ok(text) = String::from_utf8(bytes) else { return };
        edit.crlf = text.contains("\r\n");
        edit.source = if edit.crlf { text.replace("\r\n", "\n") } else { text };
        edit.saved_hash = hash;
        edit.undo.clear();
        edit.redo.clear();
        edit.saved_depth = Some(0);
        let caret = edit.caret.min(edit.source.len());
        let y = self.caret_screen_y();
        self.activate(caret, y);
        self.set_status("Reloaded from disk");
    }

    /// A click while editing. Inside the raw block it places the caret; on
    /// rendered text it opens that block with the caret near the click.
    pub(super) fn edit_click(&mut self, hit: &Hit, clicks: u32, y: f32) {
        let Some(edit) = &self.edit else { return };
        let active_block = edit.block as u32;
        if hit.pos.block != active_block {
            let b = &self.doc.blocks[hit.pos.block as usize];
            let (start, end) = (b.src.0 as usize, b.src.1 as usize);
            let raw = &edit.source[start..end];
            let prefix = b.text.get(..hit.pos.off as usize).unwrap_or("");
            let target = start + raw_offset(raw, prefix);
            self.activate(target, Some(y));
            return;
        }
        let pos = self.edit_clamp_drag(hit.pos);
        match clicks {
            2 => {
                let (a, b) = self.word_at(pos);
                self.sel = Some((a, b));
                self.edit_clamp_drag(b);
            }
            3 => self.edit_select_block(),
            _ => {
                self.sel = Some((pos, pos));
                self.selecting = true;
            }
        }
        self.touch_caret();
    }

    /// Keeps a drag selection inside the raw block and moves the caret with it.
    pub(super) fn edit_clamp_drag(&mut self, pos: Pos) -> Pos {
        let Some(edit) = &mut self.edit else { return pos };
        let block = edit.block as u32;
        let len = (edit.active.1 - edit.active.0) as u32;
        let off = match pos.block.cmp(&block) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Greater => len,
            std::cmp::Ordering::Equal => pos.off.min(len),
        };
        edit.caret = edit.active.0 + off as usize;
        edit.goal_x = None;
        edit.blink_on = true;
        Pos { block, off }
    }
}

fn edit_set(edit: &mut Option<Box<Edit>>, caret: usize) {
    if let Some(e) = edit {
        e.caret = caret.clamp(e.active.0, e.active.1);
        e.goal_x = None;
    }
}

/// Byte offset where the word before the end of `text` starts.
fn previous_word(text: &str) -> usize {
    let trimmed = text.trim_end();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    match trimmed.chars().next_back() {
        None => 0,
        Some(c) if is_word(c) => trimmed.rfind(|c| !is_word(c)).map_or(0, |i| i + trimmed[i..].chars().next().map_or(0, char::len_utf8)),
        Some(c) => trimmed.len() - c.len_utf8(),
    }
}

/// Byte length of the word at the start of `text`, plus following whitespace.
fn next_word(text: &str) -> usize {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let word = match text.chars().next() {
        Some(c) if is_word(c) => text.find(|c| !is_word(c)).unwrap_or(text.len()),
        Some(c) => c.len_utf8(),
        None => 0,
    };
    word + (text[word..].len() - text[word..].trim_start().len())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
        file: PathBuf,
        view: View,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn open(name: &str, text: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("markdonk-edit-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.md");
        std::fs::write(&file, text).unwrap();
        let mut view = View::new(load_file(&file).unwrap(), Some(file.clone()), true, Arc::new(|| {}));
        view.resize(1000, 800, 1.0);
        view.prepare();
        view.enter_edit();
        view.prepare();
        Fixture { dir, file, view }
    }

    fn raw(v: &View) -> &str {
        let e = v.edit.as_ref().unwrap();
        &e.source[e.active.0..e.active.1]
    }

    fn source(v: &View) -> &str {
        &v.edit.as_ref().unwrap().source
    }

    fn caret(v: &View) -> usize {
        v.edit.as_ref().unwrap().caret
    }

    const DOC: &str = "# Title\n\nFirst **bold** para.\n\n- a\n- b\n\nLast\n";

    #[test]
    fn the_active_block_shows_raw_markdown_and_others_stay_rendered() {
        let f = open("raw", DOC);
        assert_eq!(raw(&f.view), "# Title");
        let e = f.view.edit.as_ref().unwrap();
        assert_eq!(f.view.doc.blocks[e.block].kind, Kind::Source);
        assert_eq!(&*f.view.doc.blocks[e.block].text, "# Title");
        assert!(f.view.doc.blocks.iter().any(|b| &*b.text == "First bold para."));
    }

    #[test]
    fn moving_past_a_block_renders_it_and_opens_the_next() {
        let mut f = open("nav", DOC);
        f.view.caret_vertical(true, false);
        f.view.prepare();
        assert_eq!(raw(&f.view), "First **bold** para.");
        assert_eq!(f.view.doc.blocks[0].kind, Kind::Heading(1));
        f.view.caret_vertical(true, false);
        f.view.prepare();
        assert_eq!(raw(&f.view), "- a\n- b");
        f.view.caret_vertical(false, false);
        f.view.prepare();
        assert_eq!(raw(&f.view), "First **bold** para.");
    }

    #[test]
    fn typing_only_changes_the_source_where_the_caret_is() {
        let mut f = open("type", "abc\n\ndef\n");
        f.view.caret_right(false, false);
        f.view.edit_insert("X");
        f.view.prepare();
        assert_eq!(source(&f.view), "aXbc\n\ndef\n");
        assert_eq!(raw(&f.view), "aXbc");
        assert_eq!(caret(&f.view), 2);
    }

    #[test]
    fn edited_blocks_render_after_leaving_them() {
        let mut f = open("render", "plain\n\nnext\n");
        f.view.edit_insert("# ");
        f.view.caret_vertical(true, false);
        f.view.prepare();
        assert_eq!(f.view.doc.blocks[0].kind, Kind::Heading(1));
        assert_eq!(&*f.view.doc.blocks[0].text, "plain");
    }

    #[test]
    fn enter_and_empty_lines_keep_a_working_caret() {
        let mut f = open("enter", "one\n");
        f.view.caret_line_edge(true, false);
        f.view.edit_insert("\n");
        f.view.prepare();
        assert_eq!(source(&f.view), "one\n\n");
        assert_eq!(caret(&f.view), 4);
        let (index, x, ..) = f.view.caret_layout().unwrap();
        assert_eq!(index, 1, "the caret is on the new, empty line");
        assert_eq!(x, 0.0);
        f.view.edit_insert("t");
        f.view.prepare();
        assert_eq!(source(&f.view), "one\nt\n");
    }

    #[test]
    fn editing_an_empty_file_works() {
        let mut f = open("empty", "");
        assert!(f.view.caret_layout().is_some());
        f.view.edit_insert("# H");
        f.view.prepare();
        f.view.edit_save();
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "# H");
    }

    #[test]
    fn backspace_at_a_blocks_start_joins_it_to_the_one_above() {
        let mut f = open("join", "one\n\ntwo\n");
        f.view.caret_vertical(true, false);
        f.view.prepare();
        f.view.edit_backspace(false);
        f.view.prepare();
        assert_eq!(source(&f.view), "one\ntwo\n");
        assert_eq!(raw(&f.view), "one\ntwo");
        f.view.edit_backspace(false);
        f.view.prepare();
        assert_eq!(source(&f.view), "onetwo\n");
    }

    #[test]
    fn multibyte_text_never_splits_a_character() {
        let mut f = open("utf8", "åäö 🎉 x\n");
        for _ in 0..5 {
            f.view.caret_right(false, false);
        }
        f.view.prepare();
        assert_eq!(caret(&f.view), "åäö 🎉".len());
        f.view.edit_backspace(false);
        f.view.prepare();
        assert_eq!(source(&f.view), "åäö  x\n");
        f.view.caret_left(true, false);
        assert_eq!(caret(&f.view), 0);
        f.view.edit_delete(false);
        assert_eq!(source(&f.view), "äö  x\n");
    }

    #[test]
    fn undo_and_redo_restore_text_and_clean_state() {
        let mut f = open("undo", "abc\n\ndef\n");
        f.view.edit_insert("X");
        f.view.edit_insert("Y");
        assert!(f.view.edit.as_ref().unwrap().dirty());
        f.view.edit_undo();
        f.view.prepare();
        assert_eq!(source(&f.view), "abc\n\ndef\n");
        assert!(!f.view.edit.as_ref().unwrap().dirty(), "typing coalesces, undone back to saved");
        f.view.edit_redo();
        f.view.prepare();
        assert_eq!(source(&f.view), "XYabc\n\ndef\n");
    }

    #[test]
    fn undo_reaches_changes_made_in_another_block() {
        let mut f = open("undo2", "abc\n\ndef\n");
        f.view.edit_insert("X ");
        f.view.caret_vertical(true, false);
        f.view.prepare();
        assert_eq!(raw(&f.view), "def");
        f.view.edit_undo();
        f.view.prepare();
        assert_eq!(source(&f.view), "abc\n\ndef\n");
        assert_eq!(raw(&f.view), "abc");
    }

    #[test]
    fn save_writes_the_file_and_escape_requires_it() {
        let mut f = open("save", "abc\n");
        f.view.edit_insert("X ");
        f.view.prepare();
        f.view.edit_escape();
        assert!(f.view.editing(), "Esc must not drop unsaved changes");
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "abc\n");
        f.view.edit_save();
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "X abc\n");
        f.view.edit_escape();
        f.view.prepare();
        assert!(!f.view.editing());
        assert_eq!(&*f.view.doc.blocks[0].text, "X abc");
    }

    #[test]
    fn save_keeps_crlf_line_endings_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let mut f = open("crlf", "a\r\n\r\nb\r\n");
        std::fs::set_permissions(&f.file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(source(&f.view), "a\n\nb\n");
        f.view.edit_insert("X ");
        f.view.edit_save();
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "X a\r\n\r\nb\r\n");
        assert_eq!(std::fs::metadata(&f.file).unwrap().permissions().mode() & 0o777, 0o640);
        let left: Vec<_> = std::fs::read_dir(&f.dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");
    }

    #[test]
    fn saving_through_a_symlink_keeps_the_link() {
        let mut f = open("link", "abc\n");
        let link = f.dir.join("link.md");
        std::os::unix::fs::symlink(&f.file, &link).unwrap();
        write_atomic(&link, b"new\n").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "new\n");
        f.view.prepare();
    }

    #[test]
    fn outside_changes_never_overwrite_silently() {
        let mut f = open("conflict", "abc\n");
        f.view.edit_insert("mine ");
        std::fs::write(&f.file, "theirs\n").unwrap();
        f.view.reload();
        assert!(f.view.status_text().unwrap().contains("changed on disk"));
        assert!(source(&f.view).starts_with("mine"));
        f.view.edit_save();
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "theirs\n", "the first save only warns");
        f.view.edit_save();
        assert_eq!(std::fs::read_to_string(&f.file).unwrap(), "mine abc\n");
    }

    #[test]
    fn our_own_save_does_not_count_as_an_outside_change() {
        let mut f = open("own", "abc\n");
        f.view.edit_insert("X ");
        f.view.edit_save();
        f.view.edit_insert("Y ");
        f.view.reload();
        assert!(source(&f.view).starts_with("X Y"));
        assert!(!f.view.edit.as_ref().unwrap().changed_on_disk);
    }

    #[test]
    fn a_clean_buffer_follows_outside_changes() {
        let mut f = open("follow", "abc\n");
        std::fs::write(&f.file, "theirs\n").unwrap();
        f.view.reload();
        f.view.prepare();
        assert_eq!(source(&f.view), "theirs\n");
    }

    #[test]
    fn non_utf8_and_stdin_documents_refuse_to_edit() {
        let dir = std::env::temp_dir().join(format!("markdonk-edit-{}-bad", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bad.md");
        std::fs::write(&file, [0xff, 0xfe, b'a']).unwrap();
        let mut v = View::new(load_file(&file).unwrap(), Some(file), true, Arc::new(|| {}));
        v.resize(800, 600, 1.0);
        v.enter_edit();
        assert!(!v.editing());
        let mut v = View::new(doc::parse("x", None), None, true, Arc::new(|| {}));
        v.resize(800, 600, 1.0);
        v.enter_edit();
        assert!(!v.editing());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_with_unsaved_changes_takes_two_attempts() {
        let mut f = open("quit", "abc\n");
        assert!(f.view.request_quit(), "clean documents close right away");
        f.view.edit_insert("X");
        assert!(!f.view.request_quit());
        assert!(f.view.request_quit());
    }

    #[test]
    fn selection_replaces_and_stays_inside_the_block() {
        let mut f = open("sel", "hello world\n\nnext\n");
        for _ in 0..5 {
            f.view.caret_right(false, true);
        }
        f.view.edit_insert("bye");
        f.view.prepare();
        assert_eq!(source(&f.view), "bye world\n\nnext\n");
        f.view.edit_select_block();
        assert_eq!(f.view.selected_text().unwrap(), "bye world");
        let block = f.view.edit.as_ref().unwrap().block as u32;
        let clamped = f.view.edit_clamp_drag(Pos { block: block + 5, off: 2 });
        assert_eq!(clamped, Pos { block, off: 9 });
    }

    #[test]
    fn clicking_rendered_text_lands_near_the_same_word() {
        assert_eq!(raw_offset("First **bold** para.", "First bo"), "First **bo".len());
        assert_eq!(raw_offset("- a\n- b", "ab"), "- a\n- b".len());
        assert_eq!(raw_offset("abc", ""), 0);
    }

    #[test]
    fn clicking_another_block_opens_it() {
        let mut f = open("click", DOC);
        let i = f.view.doc.blocks.iter().position(|b| &*b.text == "Last").unwrap();
        let hit = Hit { pos: Pos { block: i as u32, off: 2 }, link: None, over_text: true };
        f.view.edit_click(&hit, 1, 200.0);
        f.view.prepare();
        assert_eq!(raw(&f.view), "Last");
        assert_eq!(caret(&f.view), DOC.find("Last").unwrap() + 2);
    }

    #[test]
    fn word_motion_stops_at_word_edges() {
        assert_eq!(previous_word("foo bar"), 4);
        assert_eq!(previous_word("foo  "), 0);
        assert_eq!(previous_word("foo."), 3);
        assert_eq!(next_word("foo bar"), 4);
        assert_eq!(next_word(".x"), 1);
        assert_eq!(previous_word(""), 0);
        assert_eq!(next_word(""), 0);
    }

    #[test]
    fn view_mode_holds_no_source_copy() {
        let mut f = open("nosrc", "abc\n");
        f.view.edit_escape();
        assert!(f.view.edit.is_none());
        assert_eq!(f.view.doc.blocks[0].kind, Kind::Para);
    }
}
