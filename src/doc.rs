//! Turns Markdown into a flat list of compact blocks. Nesting is flattened into
//! per-block indent data so the view can lay out any block on its own.

use crate::highlight;
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const BOLD: u16 = 1;
pub const ITALIC: u16 = 2;
pub const CODE: u16 = 4;
pub const STRIKE: u16 = 8;
pub const LINK: u16 = 16;
pub const ALERT: u16 = 32;
/// Syntax token kinds live in bits 8..12 of a style.
pub const TOKEN_SHIFT: u16 = 8;

pub const NO_LINK: u32 = u32::MAX;
const CODE_CHUNK_LINES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: u16,
    pub link: u32,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    Para,
    Heading(u8),
    /// One chunk of a code block; long blocks are split so they shape lazily.
    Code { lang: u8, state: u8, first: bool, last: bool },
    Rule,
    Image { path: Box<Path>, width: u32, height: u32 },
    Row { table: u32, row: u32, cells: Box<[(u32, u32)]> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Marker {
    None,
    Bullet(u8),
    Number(u32),
    Task(bool),
}

#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    pub text: Box<str>,
    pub spans: Box<[Span]>,
    /// Number of enclosing lists and block quotes.
    pub level: u8,
    /// Bit n is set when enclosing level n is a block quote.
    pub quotes: u16,
    pub marker: Marker,
    pub tight: bool,
}

pub struct Table {
    pub aligns: Box<[Alignment]>,
    pub first: u32,
    pub rows: u32,
}

pub struct TocEntry {
    pub block: u32,
    pub level: u8,
}

#[derive(Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub links: Vec<Box<str>>,
    pub tables: Vec<Table>,
    pub toc: Vec<TocEntry>,
    pub anchors: HashMap<Box<str>, u32>,
}

enum Container {
    Quote,
    List(Option<u64>),
    Footnote,
}

struct RowBuilder {
    table: u32,
    row: u32,
    cells: Vec<(u32, u32)>,
    cell_start: usize,
}

#[derive(Default, PartialEq)]
enum HtmlSkip {
    #[default]
    None,
    Comment,
    Raw(&'static str),
}

struct Builder<'a> {
    doc: Doc,
    base: Option<&'a Path>,
    text: String,
    spans: Vec<Span>,
    bold: u32,
    italic: u32,
    strike: u32,
    links: Vec<u32>,
    containers: Vec<Container>,
    marker: Marker,
    para_open: bool,
    code: Option<(u8, String)>,
    row: Option<RowBuilder>,
    heading: Option<(u8, Option<String>)>,
    image: Option<(String, String)>,
    anchors: Vec<Box<str>>,
    footnotes: HashMap<String, u32>,
    slugs: HashMap<String, u32>,
    html_skip: HtmlSkip,
    /// The next block opens an outermost list, so it isn't tight to what's above.
    list_opening: bool,
}

pub fn parse(src: &str, base: Option<&Path>) -> Doc {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_HEADING_ATTRIBUTES;
    let mut b = Builder {
        doc: Doc::default(),
        base,
        text: String::new(),
        spans: Vec::new(),
        bold: 0,
        italic: 0,
        strike: 0,
        links: Vec::new(),
        containers: Vec::new(),
        marker: Marker::None,
        para_open: false,
        code: None,
        row: None,
        heading: None,
        image: None,
        anchors: Vec::new(),
        footnotes: HashMap::new(),
        slugs: HashMap::new(),
        html_skip: HtmlSkip::None,
        list_opening: false,
    };
    for event in Parser::new_ext(src, options) {
        b.event(event);
    }
    b.flush();
    b.doc.blocks.shrink_to_fit();
    b.doc.links.shrink_to_fit();
    b.doc
}

impl Builder<'_> {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some((_, code)) = &mut self.code {
                    code.extend(text.chars().filter(|&c| c != '\r'));
                } else {
                    self.push_autolinked(&text);
                }
            }
            Event::Code(text) => self.push(&text, CODE),
            Event::SoftBreak => self.push(" ", 0),
            Event::HardBreak => self.push("\n", 0),
            Event::Rule => {
                self.flush();
                self.emit(Kind::Rule, String::new(), Vec::new());
            }
            Event::TaskListMarker(done) => self.marker = Marker::Task(done),
            Event::Html(html) => self.html(&html, true),
            Event::InlineHtml(html) => self.html(&html, false),
            Event::FootnoteReference(label) => {
                let n = self.footnote_number(&label);
                let link = self.add_link(format!("#fn:{label}"));
                self.links.push(link);
                self.push(&format!("[{n}]"), 0);
                self.links.pop();
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.flush();
                self.para_open = true;
            }
            Tag::Heading { level, id, .. } => {
                self.flush();
                self.heading = Some((heading_level(level), id.map(|s| s.to_string())));
            }
            Tag::BlockQuote(kind) => {
                self.flush();
                self.containers.push(Container::Quote);
                if let Some(kind) = kind {
                    let title = match kind {
                        BlockQuoteKind::Note => "Note",
                        BlockQuoteKind::Tip => "Tip",
                        BlockQuoteKind::Important => "Important",
                        BlockQuoteKind::Warning => "Warning",
                        BlockQuoteKind::Caution => "Caution",
                    };
                    self.push(title, BOLD | ALERT);
                    self.flush();
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(name) => highlight::lang_id(&name),
                    CodeBlockKind::Indented => 0,
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(first) => {
                self.flush();
                self.list_opening = !self.containers.iter().any(|c| matches!(c, Container::List(_)));
                self.containers.push(Container::List(first));
            }
            Tag::Item => {
                self.flush();
                let depth = self.containers.iter().filter(|c| matches!(c, Container::List(_))).count();
                self.marker = match self.containers.last_mut() {
                    Some(Container::List(Some(next))) => {
                        let n = *next;
                        *next += 1;
                        Marker::Number(n.min(u32::MAX as u64) as u32)
                    }
                    _ => Marker::Bullet(depth.saturating_sub(1) as u8),
                };
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                let n = self.footnote_number(&label);
                self.anchors.push(format!("fn:{label}").into());
                self.containers.push(Container::Footnote);
                self.marker = Marker::Number(n);
            }
            Tag::Table(aligns) => {
                self.flush();
                self.doc.tables.push(Table {
                    aligns: aligns.into(),
                    first: self.doc.blocks.len() as u32,
                    rows: 0,
                });
            }
            Tag::TableHead | Tag::TableRow => {
                let table = self.doc.tables.len() as u32 - 1;
                let row = self.doc.tables[table as usize].rows;
                self.row = Some(RowBuilder { table, row, cells: Vec::new(), cell_start: 0 });
            }
            Tag::TableCell => {
                if let Some(row) = &mut self.row {
                    if !row.cells.is_empty() {
                        self.text.push('\t');
                    }
                    row.cell_start = self.text.len();
                }
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                let link = self.add_link(dest_url.to_string());
                self.links.push(link);
            }
            Tag::Image { dest_url, .. } => {
                self.image = Some((dest_url.to_string(), String::new()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.para_open = false;
            }
            TagEnd::Heading(_) => self.finish_heading(),
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::FootnoteDefinition => {
                self.flush();
                self.containers.pop();
            }
            TagEnd::Item => {
                self.flush();
                if self.marker != Marker::None {
                    self.emit(Kind::Para, String::new(), Vec::new());
                }
            }
            TagEnd::CodeBlock => self.finish_code(),
            TagEnd::TableCell => {
                if let Some(row) = &mut self.row {
                    row.cells.push((row.cell_start as u32, self.text.len() as u32));
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(row) = self.row.take() {
                    self.doc.tables[row.table as usize].rows += 1;
                    let text = std::mem::take(&mut self.text);
                    let spans = std::mem::take(&mut self.spans);
                    let kind = Kind::Row { table: row.table, row: row.row, cells: row.cells.into() };
                    self.emit(kind, text, spans);
                }
            }
            TagEnd::Emphasis => self.italic -= 1,
            TagEnd::Strong => self.bold -= 1,
            TagEnd::Strikethrough => self.strike -= 1,
            TagEnd::Link => {
                self.links.pop();
            }
            TagEnd::Image => {
                if let Some((dest, alt)) = self.image.take() {
                    self.finish_image(&dest, &alt);
                }
            }
            _ => {}
        }
    }

    fn style(&self, extra: u16) -> u16 {
        let mut style = extra;
        if self.bold > 0 {
            style |= BOLD;
        }
        if self.italic > 0 {
            style |= ITALIC;
        }
        if self.strike > 0 {
            style |= STRIKE;
        }
        if !self.links.is_empty() {
            style |= LINK;
        }
        style
    }

    fn push(&mut self, text: &str, extra: u16) {
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(text);
            return;
        }
        let style = self.style(extra);
        let link = self.links.last().copied().unwrap_or(NO_LINK);
        self.push_styled(text, style, link);
    }

    fn push_styled(&mut self, text: &str, style: u16, link: u32) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len() as u32;
        self.text.push_str(text);
        let end = self.text.len() as u32;
        if style == 0 && link == NO_LINK {
            return;
        }
        if let Some(last) = self.spans.last_mut() {
            if last.end == start && last.style == style && last.link == link {
                last.end = end;
                return;
            }
        }
        self.spans.push(Span { start, end, style, link });
    }

    /// Pushes text, turning bare http(s) URLs into links as GitHub does.
    fn push_autolinked(&mut self, text: &str) {
        if !self.links.is_empty() || self.image.is_some() {
            self.push(text, 0);
            return;
        }
        let mut rest = text;
        while let Some(at) = rest.find("http://").into_iter().chain(rest.find("https://")).min() {
            let tail = &rest[at..];
            let len = tail.find(|c: char| c.is_whitespace() || "<>\"'".contains(c)).unwrap_or(tail.len());
            let url = tail[..len].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']']);
            self.push(&rest[..at], 0);
            if url.len() > "https://".len() {
                let link = self.add_link(url.to_string());
                self.links.push(link);
                self.push(url, 0);
                self.links.pop();
            } else {
                self.push(url, 0);
            }
            rest = &tail[url.len()..];
        }
        self.push(rest, 0);
    }

    fn add_link(&mut self, url: String) -> u32 {
        self.doc.links.push(url.into());
        self.doc.links.len() as u32 - 1
    }

    fn footnote_number(&mut self, label: &str) -> u32 {
        let next = self.footnotes.len() as u32 + 1;
        *self.footnotes.entry(label.to_string()).or_insert(next)
    }

    fn inline_block_allowed(&self) -> bool {
        self.row.is_none() && self.heading.is_none()
    }

    fn finish_image(&mut self, dest: &str, alt: &str) {
        if self.inline_block_allowed() && !is_remote(dest) {
            let path = resolve(self.base, dest);
            let size = image::ImageReader::open(&path)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.into_dimensions().ok());
            if let Some((width, height)) = size {
                self.flush();
                let text = alt.to_string();
                let spans = match self.links.last() {
                    Some(&link) => vec![Span { start: 0, end: text.len() as u32, style: LINK, link }],
                    None => Vec::new(),
                };
                self.emit(Kind::Image { path: path.into(), width, height }, text, spans);
                return;
            }
        }
        // Remote or missing images become a labelled link; nothing is fetched.
        let label = if alt.is_empty() {
            format!("[image: {dest}]")
        } else {
            format!("[image: {alt}]")
        };
        let link = self.links.last().copied().unwrap_or_else(|| self.add_link(dest.to_string()));
        let style = self.style(0) | LINK;
        self.push_styled(&label, style, link);
    }

    fn finish_heading(&mut self) {
        let Some((level, id)) = self.heading.take() else { return };
        let text = std::mem::take(&mut self.text);
        let spans = std::mem::take(&mut self.spans);
        let slug = id.unwrap_or_else(|| self.unique_slug(&text));
        self.anchors.push(slug.into());
        self.doc.toc.push(TocEntry { block: self.doc.blocks.len() as u32, level });
        self.emit(Kind::Heading(level), text, spans);
    }

    fn unique_slug(&mut self, text: &str) -> String {
        let slug = slugify(text);
        let seen = self.slugs.entry(slug.clone()).or_insert(0);
        *seen += 1;
        if *seen == 1 { slug } else { format!("{slug}-{}", *seen - 1) }
    }

    fn finish_code(&mut self) {
        let Some((lang, mut code)) = self.code.take() else { return };
        code.truncate(code.trim_end_matches('\n').len());
        let lines: Vec<&str> = code.split('\n').collect();
        let chunks = lines.len().div_ceil(CODE_CHUNK_LINES).max(1);
        let mut state = 0;
        for (i, chunk) in lines.chunks(CODE_CHUNK_LINES).enumerate() {
            let text = chunk.join("\n");
            let kind = Kind::Code { lang, state, first: i == 0, last: i + 1 == chunks };
            state = highlight::scan(lang, &text, state, |_, _, _| {});
            self.emit(kind, text, Vec::new());
        }
    }

    /// Ends the running paragraph, if any text has accumulated.
    fn flush(&mut self) {
        if self.row.is_some() || self.heading.is_some() || self.code.is_some() {
            return;
        }
        let trimmed = self.text.trim_end().len();
        if self.text[..trimmed].trim_start().is_empty() {
            self.text.clear();
            self.spans.clear();
            return;
        }
        self.text.truncate(trimmed);
        let mut spans = std::mem::take(&mut self.spans);
        spans.retain_mut(|s| {
            s.end = s.end.min(trimmed as u32);
            s.start < s.end
        });
        let text = std::mem::take(&mut self.text);
        self.emit(Kind::Para, text, spans);
    }

    fn emit(&mut self, kind: Kind, text: String, spans: Vec<Span>) {
        let index = self.doc.blocks.len() as u32;
        for anchor in self.anchors.drain(..) {
            self.doc.anchors.insert(anchor, index);
        }
        let mut quotes = 0u16;
        for (i, c) in self.containers.iter().take(16).enumerate() {
            if matches!(c, Container::Quote) {
                quotes |= 1 << i;
            }
        }
        let tight = kind == Kind::Para
            && !self.para_open
            && matches!(self.containers.last(), Some(Container::List(_)))
            && !self.list_opening;
        self.list_opening = false;
        self.doc.blocks.push(Block {
            kind,
            text: text.into(),
            spans: spans.into(),
            level: self.containers.len().min(255) as u8,
            quotes,
            marker: std::mem::replace(&mut self.marker, Marker::None),
            tight,
        });
    }

    /// Handles raw HTML: `<br>` and `<img>` work, other tags are dropped while
    /// the text between them stays visible.
    fn html(&mut self, html: &str, block: bool) {
        let mut rest = html;
        loop {
            match self.html_skip {
                HtmlSkip::None => {}
                HtmlSkip::Comment => match rest.find("-->") {
                    Some(end) => {
                        rest = &rest[end + 3..];
                        self.html_skip = HtmlSkip::None;
                    }
                    None => return,
                },
                HtmlSkip::Raw(close) => match find_ignore_case(rest, close) {
                    Some(end) => {
                        rest = &rest[end + close.len()..];
                        self.html_skip = HtmlSkip::None;
                    }
                    None => return,
                },
            }
            let Some(open) = rest.find('<') else {
                self.html_text(rest, block);
                return;
            };
            self.html_text(&rest[..open], block);
            rest = &rest[open..];
            if let Some(after) = rest.strip_prefix("<!--") {
                self.html_skip = HtmlSkip::Comment;
                rest = after;
                continue;
            }
            let Some(close) = rest.find('>') else { return };
            let tag = &rest[1..close];
            rest = &rest[close + 1..];
            self.html_tag(tag, block);
        }
    }

    fn html_text(&mut self, text: &str, block: bool) {
        let decoded = decode_entities(text);
        if !block {
            self.push(&decoded, 0);
            return;
        }
        let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with(['\n', ' ']) {
            self.push(" ", 0);
        }
        self.push(&collapsed, 0);
    }

    fn html_tag(&mut self, tag: &str, block: bool) {
        let name_end = tag.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        match name.as_str() {
            "br" => self.push("\n", 0),
            "img" => {
                if let Some(src) = html_attr(tag, "src") {
                    let alt = html_attr(tag, "alt").unwrap_or_default();
                    self.finish_image(&src, &alt);
                }
            }
            "script" => self.html_skip = HtmlSkip::Raw("</script>"),
            "style" => self.html_skip = HtmlSkip::Raw("</style>"),
            _ if block && is_block_tag(name.trim_start_matches('/')) => self.flush(),
            _ => {}
        }
    }
}

fn is_block_tag(name: &str) -> bool {
    const TAGS: &[&str] = &[
        "address", "article", "aside", "blockquote", "center", "dd", "details", "div", "dl", "dt",
        "figcaption", "figure", "footer", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "li",
        "main", "nav", "ol", "p", "pre", "section", "summary", "table", "tbody", "td", "tfoot",
        "th", "thead", "tr", "ul",
    ];
    TAGS.iter().any(|t| name.eq_ignore_ascii_case(t))
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

pub fn slugify(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => c.to_lowercase().next(),
            _ => None,
        })
        .collect()
}

pub fn is_remote(dest: &str) -> bool {
    dest.contains("://") || dest.starts_with("data:") || dest.starts_with("mailto:")
}

/// Resolves a percent-encoded relative link against the document's directory.
pub fn resolve(base: Option<&Path>, dest: &str) -> PathBuf {
    let path = PathBuf::from(percent_decode(dest));
    match base {
        Some(base) if path.is_relative() => base.join(path),
        _ => path,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn find_ignore_case(hay: &str, needle: &str) -> Option<usize> {
    let n = needle.as_bytes();
    hay.as_bytes().windows(n.len()).position(|w| w.eq_ignore_ascii_case(n))
}

fn html_attr(tag: &str, name: &str) -> Option<String> {
    let mut search = 0;
    while let Some(at) = find_ignore_case(&tag[search..], name) {
        let at = search + at;
        let before_ok = at > 0 && tag.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = tag[at + name.len()..].trim_start();
        if let (true, Some(value)) = (before_ok, rest.strip_prefix('=')) {
            let value = value.trim_start();
            let parsed = match value.chars().next() {
                Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or(""),
                _ => value.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or(""),
            };
            return Some(decode_entities(parsed));
        }
        search = at + name.len();
    }
    None
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(src: &str) -> Doc {
        parse(src, None)
    }

    #[test]
    fn headings_build_toc_and_unique_anchors() {
        let d = doc("# Hello World\n\n## Hello World\n\ntext\n");
        assert_eq!(d.toc.len(), 2);
        assert_eq!(d.anchors["hello-world"], 0);
        assert_eq!(d.anchors["hello-world-1"], 1);
        assert_eq!(d.blocks[1].kind, Kind::Heading(2));
    }

    #[test]
    fn inline_styles_become_spans() {
        let d = doc("a **b** *c* `d` ~~e~~ [f](g)");
        let b = &d.blocks[0];
        assert_eq!(&*b.text, "a b c d e f");
        let styles: Vec<u16> = b.spans.iter().map(|s| s.style).collect();
        assert_eq!(styles, [BOLD, ITALIC, CODE, STRIKE, LINK]);
        assert_eq!(&*d.links[b.spans[4].link as usize], "g");
    }

    #[test]
    fn tight_and_task_lists() {
        let d = doc("- one\n- [x] two\n  - nested\n");
        assert_eq!(d.blocks.len(), 3);
        assert_eq!(d.blocks[0].marker, Marker::Bullet(0));
        assert!(!d.blocks[0].tight);
        assert!(d.blocks[1].tight && d.blocks[2].tight);
        assert_eq!(d.blocks[1].marker, Marker::Task(true));
        assert_eq!(d.blocks[2].marker, Marker::Bullet(1));
        assert_eq!(d.blocks[2].level, 2);
    }

    #[test]
    fn ordered_list_counts_from_its_start() {
        let d = doc("3. a\n4. b\n");
        assert_eq!(d.blocks[0].marker, Marker::Number(3));
        assert_eq!(d.blocks[1].marker, Marker::Number(4));
    }

    #[test]
    fn table_rows_keep_cell_ranges() {
        let d = doc("| a | b |\n|---|:-:|\n| 1 | **2** |\n");
        assert_eq!(d.tables[0].rows, 2);
        assert_eq!(d.tables[0].aligns[1], Alignment::Center);
        let Kind::Row { cells, row, .. } = &d.blocks[1].kind else { panic!() };
        assert_eq!(*row, 1);
        let text = &d.blocks[1].text;
        assert_eq!(&text[cells[0].0 as usize..cells[0].1 as usize], "1");
        assert_eq!(&text[cells[1].0 as usize..cells[1].1 as usize], "2");
        assert_eq!(d.blocks[1].spans[0].style, BOLD);
    }

    #[test]
    fn long_code_blocks_are_chunked_and_carry_state() {
        let body: String = (0..130).map(|i| format!("line {i}\n")).collect();
        let d = doc(&format!("```c\n/* open\n{body}```\n"));
        assert_eq!(d.blocks.len(), 3);
        let Kind::Code { first, last, state, .. } = d.blocks[1].kind else { panic!() };
        assert!(!first && !last);
        assert_eq!(state, 1);
        assert!(matches!(d.blocks[2].kind, Kind::Code { last: true, .. }));
    }

    #[test]
    fn remote_image_is_a_link_and_never_a_block() {
        let d = doc("![badge](https://example.com/x.png)");
        assert_eq!(&*d.blocks[0].text, "[image: badge]");
        assert_eq!(&*d.links[d.blocks[0].spans[0].link as usize], "https://example.com/x.png");
    }

    #[test]
    fn missing_local_image_is_labelled() {
        let d = doc("![pic](nope/missing.png)");
        assert_eq!(&*d.blocks[0].text, "[image: pic]");
    }

    #[test]
    fn html_keeps_text_and_handles_br_and_img() {
        let d = doc("<div align=\"center\">\nHello &amp; bye<br>there\n<script>evil()</script>\n<img src=\"https://x/y.png\" alt=\"logo\">\n</div>\n");
        let all: Vec<&str> = d.blocks.iter().map(|b| &*b.text).collect();
        let joined = all.join("|");
        assert!(joined.contains("Hello & bye\nthere"), "{joined}");
        assert!(joined.contains("[image: logo]"), "{joined}");
        assert!(!joined.contains("evil"), "{joined}");
    }

    #[test]
    fn inline_html_tags_do_not_split_a_paragraph() {
        let d = doc("<div>Raw <b>bold</b> text<br>next</div>\n");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(&*d.blocks[0].text, "Raw bold text\nnext");
    }

    #[test]
    fn adjacent_lists_are_separated() {
        let d = doc("- a\n\n1. b\n");
        assert!(!d.blocks[1].tight);
    }

    #[test]
    fn multiline_html_comment_is_skipped() {
        let d = doc("<!-- hidden\nstill hidden -->\n\nvisible\n");
        let all: Vec<&str> = d.blocks.iter().map(|b| &*b.text).collect();
        assert_eq!(all, ["visible"]);
    }

    #[test]
    fn footnotes_link_both_ways() {
        let d = doc("see[^a]\n\n[^a]: note\n");
        assert_eq!(&*d.blocks[0].text, "see[1]");
        let def = d.anchors["fn:a"] as usize;
        assert_eq!(&*d.blocks[def].text, "note");
        assert_eq!(d.blocks[def].marker, Marker::Number(1));
    }

    #[test]
    fn bare_urls_are_linked_without_trailing_punctuation() {
        let d = doc("see https://example.com/a. Done");
        assert_eq!(&*d.links[0], "https://example.com/a");
        assert_eq!(&*d.blocks[0].text, "see https://example.com/a. Done");
    }

    #[test]
    fn nested_quote_sets_level_bits() {
        let d = doc("> - item\n");
        assert_eq!(d.blocks[0].level, 2);
        assert_eq!(d.blocks[0].quotes, 0b01);
    }

    #[test]
    fn alert_gets_a_title() {
        let d = doc("> [!WARNING]\n> careful\n");
        assert_eq!(&*d.blocks[0].text, "Warning");
        assert_eq!(d.blocks[0].spans[0].style, BOLD | ALERT);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for src in ["<", "<img", "<img src=", "![](", "| a |\n|---|\n", "[^x]", "%", "- ", "<!-- ", "&", "```", "> ", "https://", "\u{0}"] {
            doc(src);
        }
        assert_eq!(percent_decode("a%20b%"), "a b%");
        assert_eq!(percent_decode("%é"), "%é");
    }
}
