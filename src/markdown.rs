//! Markdown → render model → RTF.
//!
//! Parsing and RTF generation are platform-independent so they run on the
//! loader thread and are tested on Linux. The Windows preview only streams
//! the resulting RTF into a RichEdit control.

use std::fmt::Write;
use std::path::Path;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::preview::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

/// Markdown is formatted only when the decoded source is at most this many
/// UTF-8 bytes. Larger files keep the existing plain-text preview.
pub const MARKDOWN_MAX_BYTES: usize = DEFAULT_MAX_BYTES;
/// And at most this many source lines, matching the EDIT line cap.
pub const MARKDOWN_MAX_LINES: usize = DEFAULT_MAX_LINES;

/// True for `.md` and `.markdown`, case-insensitive. `.mdx` is not Markdown.
pub fn is_markdown_path(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
}

/// Whether the decoded source is small enough to lay out as Markdown.
pub fn should_render_markdown(source_bytes: usize, source_lines: usize) -> bool {
    source_bytes <= MARKDOWN_MAX_BYTES && source_lines <= MARKDOWN_MAX_LINES
}

/// Inline style flags for one run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MdStyle {
    pub emphasis: bool,
    pub strong: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

/// A contiguous span of text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdRun {
    pub text: String,
    pub style: MdStyle,
}

/// Block-level kind. Quote state is a flag on [`MdBlock`] so it composes
/// with lists, code, and headings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlockKind {
    Paragraph,
    Heading {
        level: u8,
    },
    ListItem {
        marker: String,
    },
    Code,
    /// Raw HTML, shown as literal monospace text (never executed).
    Html,
    Rule,
}

/// One block in the render model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdBlock {
    pub kind: MdBlockKind,
    /// Nest depth from lists and quotes, capped.
    pub indent: u8,
    pub quote: bool,
    pub runs: Vec<MdRun>,
}

/// Plain-data Markdown document produced on the loader thread.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MdDocument {
    pub blocks: Vec<MdBlock>,
}

const MAX_INDENT: u8 = 16;
const TWIPS_PER_INDENT: i32 = 360;
/// Extra left indent for a quote, and the padding added to a code block.
const QUOTE_EXTRA_TWIPS: i32 = 240;
const CODE_EXTRA_TWIPS: i32 = 180;
/// Deepest `\li` the writer emits. Indent is clamped to [`MAX_INDENT`] first,
/// so a 10,000-deep quote or list cannot push the margin off the page.
const MAX_LEFT_INDENT_TWIPS: i32 =
    (MAX_INDENT as i32) * TWIPS_PER_INDENT + QUOTE_EXTRA_TWIPS + CODE_EXTRA_TWIPS;
const BODY_FS: u16 = 27;
const CODE_FS: u16 = 22;

/// Parse CommonMark (plus strikethrough and tables) into the render model.
///
/// Raw HTML is kept as literal text. Images become `[image: alt]` (or
/// `[image]` when the alt is empty). Links keep their inner text and a
/// link style flag; they are not clickable. Tables are flattened to
/// aligned monospace rows.
pub fn parse_markdown(source: &str) -> MdDocument {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(source, options);
    convert_events(parser)
}

/// Parse `source` and emit RTF. `notice` is an optional truncation line
/// shown first, in muted italic.
pub fn render_markdown(source: &str, notice: Option<&str>) -> String {
    document_to_rtf(&parse_markdown(source), notice)
}

/// Heading font size in RTF half-points. Distinct for H1–H6.
pub fn heading_half_points(level: u8) -> u16 {
    match level {
        1 => 48,
        2 => 40,
        3 => 34,
        4 => 30,
        5 => 28,
        _ => BODY_FS,
    }
}

/// Escape text for RTF: braces, backslashes, tabs, non-ASCII as `\uN?`.
/// C0 controls other than tab and newline are dropped. Newlines become `\line`.
pub fn escape_rtf(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    push_rtf_text(&mut out, text);
    out
}

/// Convert a render model to ANSI RTF with `\uN?` Unicode escapes.
pub fn document_to_rtf(doc: &MdDocument, notice: Option<&str>) -> String {
    let mut out = String::with_capacity(256 + doc.blocks.len().saturating_mul(64));
    out.push_str(
        r"{\rtf1\ansi\ansicpg1252\deff0\nouicompat
{\fonttbl{\f0\fswiss\fcharset0 Segoe UI;}{\f1\fmodern\fcharset0 Cascadia Mono;}{\f2\fmodern\fcharset0 Consolas;}}
{\colortbl ;\red32\green32\blue32;\red0\green99\blue177;\red96\green96\blue96;\red245\green245\blue245;}
\uc1\pard\f0\fs27\cf1
",
    );
    if let Some(notice) = notice.filter(|text| !text.is_empty()) {
        out.push_str("\\pard\\widctlpar\\sa200\\li0\\f0\\fs22\\cf3\\i ");
        push_rtf_text(&mut out, notice);
        out.push_str("\\i0\\par\n");
    }
    for block in &doc.blocks {
        push_block(&mut out, block);
    }
    out.push('}');
    debug_assert!(
        out.is_ascii(),
        "RTF must be ASCII; non-ASCII should be \\uN? escapes"
    );
    out
}

fn convert_events<'a>(parser: impl Iterator<Item = Event<'a>>) -> MdDocument {
    let mut conv = Converter::default();
    for event in parser {
        conv.feed(event);
    }
    conv.finish()
}

#[derive(Default)]
struct Converter {
    blocks: Vec<MdBlock>,
    emphasis: u8,
    strong: u8,
    strike: u8,
    link: u8,
    quote_depth: u8,
    list_stack: Vec<ListState>,
    pending_marker: Option<String>,
    current_kind: Option<MdBlockKind>,
    current_runs: Vec<MdRun>,
    code_buf: Option<String>,
    html_buf: Option<String>,
    image_alt: Option<String>,
    table: Option<TableBuilder>,
}

struct ListState {
    next: Option<u64>,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
}

impl Converter {
    fn style(&self) -> MdStyle {
        MdStyle {
            emphasis: self.emphasis > 0,
            strong: self.strong > 0,
            strike: self.strike > 0,
            code: false,
            link: self.link > 0,
        }
    }

    fn indent(&self) -> u8 {
        let lists = u8::try_from(self.list_stack.len()).unwrap_or(u8::MAX);
        self.quote_depth.saturating_add(lists).min(MAX_INDENT)
    }

    fn feed(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => self.push_inline_code(&text),
            Event::Html(text) | Event::InlineHtml(text) => self.push_html_text(&text),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush_open();
                self.push_block(MdBlockKind::Rule, Vec::new());
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push_inline_code(&text),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush_open();
                self.current_kind = Some(MdBlockKind::Paragraph);
            }
            Tag::Heading { level, .. } => {
                self.flush_open();
                self.current_kind = Some(MdBlockKind::Heading { level: level as u8 });
            }
            Tag::BlockQuote(_) => {
                self.flush_open();
                self.quote_depth = self.quote_depth.saturating_add(1);
            }
            Tag::CodeBlock(_) => {
                self.flush_open();
                self.code_buf = Some(String::new());
            }
            Tag::HtmlBlock => {
                self.flush_open();
                self.html_buf = Some(String::new());
            }
            Tag::List(start) => {
                self.flush_open();
                self.list_stack.push(ListState { next: start });
            }
            Tag::Item => {
                self.flush_open();
                self.pending_marker = Some(self.next_marker());
            }
            Tag::Emphasis => self.emphasis = self.emphasis.saturating_add(1),
            Tag::Strong => self.strong = self.strong.saturating_add(1),
            Tag::Strikethrough => self.strike = self.strike.saturating_add(1),
            Tag::Link { .. } => self.link = self.link.saturating_add(1),
            Tag::Image { .. } => self.image_alt = Some(String::new()),
            Tag::Table(_) => {
                self.flush_open();
                self.table = Some(TableBuilder::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.current_row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    table.current_cell.clear();
                }
            }
            Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush_open(),
            TagEnd::BlockQuote(_) => {
                self.flush_open();
                self.quote_depth = self.quote_depth.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some(text) = self.code_buf.take() {
                    let run = MdRun {
                        text,
                        style: MdStyle {
                            code: true,
                            ..MdStyle::default()
                        },
                    };
                    self.push_block(MdBlockKind::Code, vec![run]);
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(text) = self.html_buf.take() {
                    let run = MdRun {
                        text,
                        style: MdStyle {
                            code: true,
                            ..MdStyle::default()
                        },
                    };
                    self.push_block(MdBlockKind::Html, vec![run]);
                }
            }
            TagEnd::List(_) => {
                self.flush_open();
                self.list_stack.pop();
            }
            TagEnd::Item => {
                if self.pending_marker.is_some() {
                    self.flush_open();
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::Image => {
                if let Some(alt) = self.image_alt.take() {
                    let label = if alt.is_empty() {
                        "[image]".to_string()
                    } else {
                        format!("[image: {alt}]")
                    };
                    self.push_text(&label);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    let text = format_table(&table.rows);
                    if !text.is_empty() {
                        let run = MdRun {
                            text,
                            style: MdStyle {
                                code: true,
                                ..MdStyle::default()
                            },
                        };
                        self.push_block(MdBlockKind::Code, vec![run]);
                    }
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    if !table.current_row.is_empty() {
                        let row = std::mem::take(&mut table.current_row);
                        table.rows.push(row);
                    }
                }
            }
            TagEnd::TableCell => {
                if let Some(table) = self.table.as_mut() {
                    let cell = std::mem::take(&mut table.current_cell);
                    table.current_row.push(cell);
                }
            }
            TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn next_marker(&mut self) -> String {
        match self.list_stack.last_mut() {
            Some(ListState { next: Some(n) }) => {
                let marker = format!("{n}.");
                *n = n.saturating_add(1);
                marker
            }
            _ => "\u{2022}".to_string(),
        }
    }

    fn push_inline_code(&mut self, text: &str) {
        if self.divert_plain(text) {
            return;
        }
        self.ensure_paragraph();
        self.push_run(
            text,
            MdStyle {
                code: true,
                link: self.link > 0,
                ..MdStyle::default()
            },
        );
    }

    fn push_html_text(&mut self, text: &str) {
        if let Some(buf) = self.html_buf.as_mut() {
            buf.push_str(text);
            return;
        }
        if self.divert_plain(text) {
            return;
        }
        self.ensure_paragraph();
        self.push_run(
            text,
            MdStyle {
                code: true,
                ..MdStyle::default()
            },
        );
    }

    fn push_text(&mut self, text: &str) {
        if self.divert_plain(text) {
            return;
        }
        if text.is_empty() {
            return;
        }
        self.ensure_paragraph();
        self.push_run(text, self.style());
    }

    fn divert_plain(&mut self, text: &str) -> bool {
        if let Some(alt) = self.image_alt.as_mut() {
            alt.push_str(text);
            return true;
        }
        if let Some(code) = self.code_buf.as_mut() {
            code.push_str(text);
            return true;
        }
        if let Some(html) = self.html_buf.as_mut() {
            html.push_str(text);
            return true;
        }
        if let Some(table) = self.table.as_mut() {
            table.current_cell.push_str(text);
            return true;
        }
        false
    }

    fn ensure_paragraph(&mut self) {
        if self.current_kind.is_none()
            && self.code_buf.is_none()
            && self.html_buf.is_none()
            && self.image_alt.is_none()
            && self.table.is_none()
        {
            self.current_kind = Some(MdBlockKind::Paragraph);
        }
    }

    fn push_run(&mut self, text: &str, style: MdStyle) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.current_runs.last_mut() {
            if last.style == style {
                last.text.push_str(text);
                return;
            }
        }
        self.current_runs.push(MdRun {
            text: text.to_string(),
            style,
        });
    }

    fn flush_open(&mut self) {
        if let Some(kind) = self.current_kind.take() {
            let runs = std::mem::take(&mut self.current_runs);
            self.push_block(kind, runs);
        } else if !self.current_runs.is_empty() {
            let runs = std::mem::take(&mut self.current_runs);
            self.push_block(MdBlockKind::Paragraph, runs);
        }
    }

    fn push_block(&mut self, mut kind: MdBlockKind, runs: Vec<MdRun>) {
        if let Some(marker) = self.pending_marker.take() {
            if matches!(kind, MdBlockKind::Paragraph) {
                kind = MdBlockKind::ListItem { marker };
            } else {
                self.blocks.push(MdBlock {
                    kind: MdBlockKind::ListItem { marker },
                    indent: self.indent(),
                    quote: self.quote_depth > 0,
                    runs: Vec::new(),
                });
            }
        }
        if runs.is_empty() && matches!(kind, MdBlockKind::Paragraph) {
            return;
        }
        self.blocks.push(MdBlock {
            kind,
            indent: self.indent(),
            quote: self.quote_depth > 0,
            runs,
        });
    }

    fn finish(mut self) -> MdDocument {
        self.flush_open();
        if let Some(text) = self.code_buf.take() {
            let run = MdRun {
                text,
                style: MdStyle {
                    code: true,
                    ..MdStyle::default()
                },
            };
            self.push_block(MdBlockKind::Code, vec![run]);
        }
        if let Some(text) = self.html_buf.take() {
            let run = MdRun {
                text,
                style: MdStyle {
                    code: true,
                    ..MdStyle::default()
                },
            };
            self.push_block(MdBlockKind::Html, vec![run]);
        }
        MdDocument {
            blocks: self.blocks,
        }
    }
}

fn format_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 {
            out.push('\n');
        }
        push_table_row(&mut out, row, &widths, cols);
        if ri == 0 && rows.len() > 1 {
            out.push('\n');
            for (i, width) in widths.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                for _ in 0..*width {
                    out.push('-');
                }
            }
        }
    }
    out
}

fn push_table_row(out: &mut String, row: &[String], widths: &[usize], cols: usize) {
    for (i, width) in widths.iter().enumerate().take(cols) {
        if i > 0 {
            out.push_str(" | ");
        }
        let cell = row.get(i).map(String::as_str).unwrap_or("");
        out.push_str(cell);
        let pad = width.saturating_sub(cell.chars().count());
        for _ in 0..pad {
            out.push(' ');
        }
    }
}

fn push_block(out: &mut String, block: &MdBlock) {
    match &block.kind {
        MdBlockKind::Code | MdBlockKind::Html => push_code_block(out, block),
        MdBlockKind::Rule => {
            push_pard(out, block);
            out.push_str("\\par\n");
        }
        MdBlockKind::ListItem { marker } => {
            push_pard(out, block);
            push_rtf_text(out, marker);
            out.push(' ');
            for run in &block.runs {
                push_styled_run(out, run, block);
            }
            out.push_str("\\par\n");
        }
        MdBlockKind::Paragraph | MdBlockKind::Heading { .. } => {
            push_pard(out, block);
            for run in &block.runs {
                push_styled_run(out, run, block);
            }
            out.push_str("\\par\n");
        }
    }
}

fn push_code_block(out: &mut String, block: &MdBlock) {
    let text: String = block.runs.iter().map(|run| run.text.as_str()).collect();
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|line| line.is_empty()) && lines.len() > 1 {
        lines.pop();
    }
    for line in lines {
        push_pard(out, block);
        push_rtf_text(out, line);
        out.push_str("\\par\n");
    }
}

fn push_pard(out: &mut String, block: &MdBlock) {
    let li = left_indent_twips(block);
    out.push_str("\\pard\\widctlpar");
    match &block.kind {
        MdBlockKind::Heading { level } => {
            let fs = heading_half_points(*level);
            let sb = if *level <= 2 { 280 } else { 160 };
            let _ = write!(out, "\\sb{sb}\\sa160\\li{li}\\f0\\fs{fs}\\b\\cf1 ");
        }
        MdBlockKind::ListItem { .. } => {
            let cf = if block.quote { 3 } else { 1 };
            let _ = write!(out, "\\sa80\\li{li}\\fi-360\\f0\\fs{BODY_FS}\\cf{cf} ");
        }
        MdBlockKind::Code | MdBlockKind::Html => {
            let _ = write!(out, "\\sa40\\li{li}\\f1\\fs{CODE_FS}\\cf1\\highlight4 ");
        }
        MdBlockKind::Rule => {
            let _ = write!(
                out,
                "\\sb160\\sa160\\li{li}\\brdrb\\brdrs\\brdrw15\\brsp80\\cf3 "
            );
        }
        MdBlockKind::Paragraph => {
            let cf = if block.quote { 3 } else { 1 };
            let _ = write!(out, "\\sa160\\li{li}\\f0\\fs{BODY_FS}\\cf{cf} ");
        }
    }
}

fn left_indent_twips(block: &MdBlock) -> i32 {
    let depth = i32::from(block.indent.min(MAX_INDENT));
    let mut li = depth * TWIPS_PER_INDENT;
    if block.quote {
        li += QUOTE_EXTRA_TWIPS;
    }
    let li = match block.kind {
        MdBlockKind::ListItem { .. } => li.max(TWIPS_PER_INDENT),
        MdBlockKind::Code | MdBlockKind::Html => li + CODE_EXTRA_TWIPS,
        _ => li,
    };
    li.min(MAX_LEFT_INDENT_TWIPS)
}

fn push_styled_run(out: &mut String, run: &MdRun, block: &MdBlock) {
    out.push('{');
    if run.style.code {
        let _ = write!(out, "\\f1\\fs{CODE_FS}");
    }
    if run.style.strong {
        out.push_str("\\b");
    }
    if run.style.emphasis {
        out.push_str("\\i");
    }
    if run.style.strike {
        out.push_str("\\strike");
    }
    if run.style.link {
        out.push_str("\\ul\\cf2");
    } else if block.quote && !run.style.code {
        out.push_str("\\cf3");
    }
    out.push(' ');
    push_rtf_text(out, &run.text);
    out.push('}');
}

fn push_rtf_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\t' => out.push_str("\\tab "),
            '\r' => {}
            '\n' => out.push_str("\\line "),
            c if (c as u32) < 0x20 || c == '\u{7F}' => {}
            c if c.is_ascii() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                let n = c.encode_utf16(&mut buf).len();
                for &unit in buf.iter().take(n) {
                    let signed = unit as i16;
                    let _ = write!(out, "\\u{signed}?");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    fn doc(src: &str) -> MdDocument {
        parse_markdown(src)
    }

    fn plain(block: &MdBlock) -> String {
        block.runs.iter().map(|run| run.text.as_str()).collect()
    }

    fn kinds(src: &str) -> Vec<MdBlockKind> {
        doc(src)
            .blocks
            .into_iter()
            .map(|block| block.kind)
            .collect()
    }

    fn sample_markdown(target_bytes: usize) -> String {
        // Long paragraphs keep the line count under the Markdown cap at 64 KiB
        // so the timed path matches `should_render_markdown`.
        let unit = concat!(
            "## Heading\n\n",
            "A paragraph with **bold**, *italic*, ~~strike~~, `code`, ",
            "and a [link](https://example.com). ",
            "More words fill the paragraph so a 64 KiB document stays under ",
            "two thousand lines while still exercising the usual constructs. ",
            "A second sentence with `more code` and ~~strike~~ keeps the parser busy.\n\n",
            "- item with **bold** and a [link](https://example.com)\n",
            "> quote with *emphasis*\n\n",
            "```\nfn sample() { let x = 1; }\n```\n\n",
        );
        let mut out = String::with_capacity(target_bytes.saturating_add(unit.len()));
        while out.len() < target_bytes {
            out.push_str(unit);
        }
        out.truncate(target_bytes);
        out
    }

    #[test]
    fn extension_detection_is_case_insensitive_and_rejects_mdx() {
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("README.MD")));
        assert!(is_markdown_path(Path::new("notes.Md")));
        assert!(is_markdown_path(Path::new("notes.markdown")));
        assert!(is_markdown_path(Path::new("notes.MARKDOWN")));
        assert!(is_markdown_path(&PathBuf::from("dir").join("file.md")));
        assert!(!is_markdown_path(Path::new("notes.mdx")));
        assert!(!is_markdown_path(Path::new("notes.mdown")));
        assert!(!is_markdown_path(Path::new("notes.txt")));
        assert!(!is_markdown_path(Path::new("md")));
        assert!(!is_markdown_path(Path::new("file.md.bak")));
        assert!(!is_markdown_path(Path::new(".gitignore")));
    }

    #[test]
    fn huge_file_fallback_uses_byte_and_line_caps() {
        assert!(should_render_markdown(0, 0));
        assert!(should_render_markdown(
            MARKDOWN_MAX_BYTES,
            MARKDOWN_MAX_LINES
        ));
        assert!(!should_render_markdown(MARKDOWN_MAX_BYTES + 1, 1));
        assert!(!should_render_markdown(1, MARKDOWN_MAX_LINES + 1));
        assert!(!should_render_markdown(
            MARKDOWN_MAX_BYTES + 1,
            MARKDOWN_MAX_LINES + 1
        ));
        assert_eq!(MARKDOWN_MAX_BYTES, 64 * 1024);
        assert_eq!(MARKDOWN_MAX_LINES, 2_000);
    }

    #[test]
    fn headings_have_distinct_levels() {
        let parsed = doc("# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n");
        let levels: Vec<u8> = parsed
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                MdBlockKind::Heading { level } => Some(level),
                _ => None,
            })
            .collect();
        assert_eq!(levels, vec![1, 2, 3, 4, 5, 6]);
        let sizes: Vec<u16> = levels
            .iter()
            .map(|level| heading_half_points(*level))
            .collect();
        for pair in sizes.windows(2) {
            assert!(pair[0] > pair[1], "heading sizes must step down: {sizes:?}");
        }
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\fs48"));
        assert!(rtf.contains("\\fs40"));
        assert!(rtf.contains("\\fs34"));
        assert!(rtf.contains("\\fs30"));
        assert!(rtf.contains("\\fs28"));
        assert!(rtf.contains("\\fs27"));
        assert!(rtf.contains("\\b"));
    }

    #[test]
    fn emphasis_strong_and_strikethrough() {
        let parsed = doc("*em* **strong** ~~strike~~ ***both***");
        let runs: Vec<&MdRun> = parsed.blocks.iter().flat_map(|block| &block.runs).collect();
        let em = runs.iter().find(|run| run.text == "em").unwrap();
        assert!(em.style.emphasis && !em.style.strong);
        let strong = runs.iter().find(|run| run.text == "strong").unwrap();
        assert!(strong.style.strong && !strong.style.emphasis);
        let strike = runs.iter().find(|run| run.text == "strike").unwrap();
        assert!(strike.style.strike);
        let both = runs.iter().find(|run| run.text == "both").unwrap();
        assert!(both.style.emphasis && both.style.strong);
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\i"));
        assert!(rtf.contains("\\b"));
        assert!(rtf.contains("\\strike"));
    }

    #[test]
    fn unordered_ordered_and_nested_lists() {
        let parsed = doc("- a\n- b\n  1. one\n  2. two\n");
        let items: Vec<(u8, String, String)> = parsed
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                MdBlockKind::ListItem { marker } => {
                    Some((block.indent, marker.clone(), plain(block)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(items[0], (1, "\u{2022}".to_string(), "a".to_string()));
        assert_eq!(items[1], (1, "\u{2022}".to_string(), "b".to_string()));
        assert_eq!(items[2].1, "1.");
        assert_eq!(items[2].2, "one");
        assert_eq!(items[3].1, "2.");
        assert_eq!(items[3].2, "two");
        assert!(items[2].0 > items[0].0, "nested list indents further");
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\fi-360"));
        assert!(rtf.contains("\\li360") || rtf.contains("\\li720"));
    }

    #[test]
    fn ordered_list_keeps_start_number() {
        let parsed = doc("3. third\n4. fourth\n");
        let markers: Vec<String> = parsed
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                MdBlockKind::ListItem { marker } => Some(marker.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(markers, vec!["3.".to_string(), "4.".to_string()]);
    }

    #[test]
    fn block_quotes_are_indented_and_flagged() {
        let parsed = doc("> quoted **bold**\n>\n> still\n");
        assert!(parsed.blocks.iter().all(|block| block.quote));
        assert!(parsed.blocks.iter().any(|block| block.indent >= 1));
        let text: String = parsed.blocks.iter().map(plain).collect();
        assert!(text.contains("quoted"));
        assert!(text.contains("still"));
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\cf3"));
        assert!(rtf.contains("\\li"));
    }

    #[test]
    fn inline_and_fenced_code_preserve_whitespace() {
        let parsed = doc("Use `a  b` please.\n\n```\nline1\n  indented\n\tTabbed\n```\n");
        let inline = parsed.blocks[0]
            .runs
            .iter()
            .find(|run| run.style.code)
            .unwrap();
        assert_eq!(inline.text, "a  b");
        let code = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::Code))
            .unwrap();
        assert_eq!(plain(code), "line1\n  indented\n\tTabbed\n");
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\f1"));
        assert!(rtf.contains("\\tab "));
        assert!(rtf.contains("  indented"));
    }

    #[test]
    fn indented_code_block_is_code() {
        let parsed = doc("para\n\n    foo()\n    bar()\n");
        assert!(
            parsed
                .blocks
                .iter()
                .any(|block| matches!(block.kind, MdBlockKind::Code)
                    && plain(block).contains("foo()"))
        );
    }

    #[test]
    fn links_are_styled_text_not_urls() {
        let parsed = doc("See [docs](https://example.com/secret) please.");
        let link = parsed.blocks[0]
            .runs
            .iter()
            .find(|run| run.style.link)
            .unwrap();
        assert_eq!(link.text, "docs");
        let joined = plain(&parsed.blocks[0]);
        assert!(!joined.contains("https://"));
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\ul"));
        assert!(rtf.contains("\\cf2"));
        assert!(!rtf.contains("example.com"));
    }

    #[test]
    fn image_alt_is_shown_and_empty_alt_has_placeholder() {
        let with_alt = doc("![Cat photo](http://example.com/cat.png)");
        let text: String = with_alt.blocks.iter().map(plain).collect();
        assert_eq!(text, "[image: Cat photo]");
        assert!(!text.contains("http://"));
        let empty = doc("![](http://example.com/x.png)");
        let text: String = empty.blocks.iter().map(plain).collect();
        assert_eq!(text, "[image]");
    }

    #[test]
    fn horizontal_rule_and_breaks() {
        let parsed = doc("before\n\n---\n\nafter  \nhard\n");
        assert!(parsed
            .blocks
            .iter()
            .any(|block| matches!(block.kind, MdBlockKind::Rule)));
        let after = parsed
            .blocks
            .iter()
            .find(|block| plain(block).contains("after"))
            .unwrap();
        assert!(plain(after).contains('\n'));
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\brdrb"));
        assert!(rtf.contains("\\line "));
    }

    #[test]
    fn list_in_quote_and_code_in_list() {
        let src = "> - quoted item\n>\n>   ```\n>   nested()\n>   ```\n";
        let parsed = doc(src);
        let item = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::ListItem { .. }))
            .unwrap();
        assert!(item.quote);
        assert!(plain(item).contains("quoted item"));
        let code = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::Code))
            .unwrap();
        assert!(code.quote);
        assert!(code.indent >= item.indent);
        assert!(plain(code).contains("nested()"));
    }

    #[test]
    fn raw_html_is_literal_text() {
        let parsed = doc("Hello <span>world</span>.\n\n<div>\n<script>alert(1)</script>\n</div>\n");
        let inline = parsed.blocks[0]
            .runs
            .iter()
            .find(|run| run.style.code && run.text.contains("<span>"))
            .unwrap();
        assert_eq!(inline.text, "<span>");
        let html = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::Html))
            .unwrap();
        let text = plain(html);
        assert!(text.contains("<div>"));
        assert!(text.contains("<script>alert(1)</script>"));
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\f1"));
        assert!(escape_rtf("<div>").contains("<div>"));
    }

    #[test]
    fn tables_render_as_aligned_monospace() {
        let src = "| A | BB |\n| --- | --- |\n| x | yz |\n";
        let parsed = doc(src);
        let code = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::Code))
            .unwrap();
        let text = plain(code);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("A") && line.contains("BB") && line.contains('|')),
            "{text:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("x") && line.contains("yz") && line.contains('|')),
            "{text:?}"
        );
        assert!(text.contains('-'), "{text:?}");
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.contains("\\f1"));
    }

    #[test]
    fn very_long_line_stays_one_run() {
        let long = "a".repeat(5_000);
        let parsed = doc(&format!("# title\n\n{long}\n"));
        let para = parsed
            .blocks
            .iter()
            .find(|block| matches!(block.kind, MdBlockKind::Paragraph))
            .unwrap();
        assert_eq!(plain(para).len(), 5_000);
        let rtf = document_to_rtf(&parsed, None);
        assert!(rtf.len() > 5_000);
        assert!(should_render_markdown(long.len() + 16, 3));
    }

    #[test]
    fn rtf_escapes_braces_backslashes_unicode_and_controls() {
        assert_eq!(escape_rtf("{\\}"), r"\{\\\}");
        assert_eq!(escape_rtf("a{b}c"), r"a\{b\}c");
        assert_eq!(escape_rtf("path\\file"), r"path\\file");
        assert_eq!(escape_rtf("café"), r"caf\u233?");
        // U+1F600 GRINNING FACE is a surrogate pair in UTF-16.
        let grin = escape_rtf("😀");
        assert_eq!(grin, r"\u-10179?\u-8704?");
        assert_eq!(escape_rtf("a\tb"), r"a\tab b");
        assert_eq!(escape_rtf("a\nb"), r"a\line b");
        assert_eq!(escape_rtf("a\u{0001}b"), "ab");
        assert_eq!(escape_rtf("a\u{007F}b"), "ab");
        let rtf = document_to_rtf(&doc("code `{x}` here"), None);
        assert!(rtf.contains(r"\{x\}"));
        assert!(rtf.is_ascii());
    }

    #[test]
    fn truncation_notice_is_first_rtf_paragraph() {
        let rtf = document_to_rtf(&doc("Hi"), Some("Showing the first 64 KB of 1.2 MB"));
        let notice_at = rtf.find("Showing the first").unwrap();
        let body_at = rtf.find("Hi").unwrap();
        assert!(notice_at < body_at);
        assert!(rtf.contains("\\i"));
        assert!(rtf.contains("\\cf3"));
    }

    #[test]
    fn crlf_source_parses_like_lf() {
        let lf = doc("# Title\r\n\r\nA **bold** line\r\n");
        let unix = doc("# Title\n\nA **bold** line\n");
        assert_eq!(lf.blocks.len(), unix.blocks.len());
        assert_eq!(plain(&lf.blocks[0]), plain(&unix.blocks[0]));
        assert_eq!(plain(&lf.blocks[1]), plain(&unix.blocks[1]));
    }

    #[test]
    fn fenced_code_does_not_wrap_control_into_body_rtf_font() {
        let rtf = render_markdown("hello\n\n```\n{}\n```\n", None);
        assert!(rtf.contains(r"\{\}"));
        assert!(rtf.starts_with(r"{\rtf1"));
        assert!(rtf.ends_with('}'));
    }

    #[test]
    fn markdown_parse_and_model_timing_at_cap() {
        let src = sample_markdown(MARKDOWN_MAX_BYTES);
        assert_eq!(src.len(), MARKDOWN_MAX_BYTES);
        let start = Instant::now();
        let parsed = parse_markdown(&src);
        let rtf = document_to_rtf(&parsed, None);
        let elapsed = start.elapsed();
        println!(
            "markdown parse+model+rtf at cap ({} bytes source, {} blocks, {} rtf bytes): {:?} (Linux parse/model only)",
            src.len(),
            parsed.blocks.len(),
            rtf.len(),
            elapsed
        );
        assert!(!parsed.blocks.is_empty());
        assert!(rtf.starts_with(r"{\rtf1"));
        assert!(
            src.lines().count() <= MARKDOWN_MAX_LINES,
            "cap sample should stay within the line cap, got {}",
            src.lines().count()
        );
        assert!(should_render_markdown(src.len(), src.lines().count()));
    }

    #[test]
    #[ignore]
    fn markdown_parse_and_model_timing_1mib() {
        let src = sample_markdown(1024 * 1024);
        assert_eq!(src.len(), 1024 * 1024);
        let start = Instant::now();
        let parsed = parse_markdown(&src);
        let rtf = document_to_rtf(&parsed, None);
        let elapsed = start.elapsed();
        println!(
            "markdown parse+model+rtf at 1 MiB ({} bytes source, {} blocks, {} rtf bytes): {:?} (Linux parse/model only)",
            src.len(),
            parsed.blocks.len(),
            rtf.len(),
            elapsed
        );
        assert!(!parsed.blocks.is_empty());
        assert!(rtf.starts_with(r"{\rtf1"));
        assert!(!should_render_markdown(src.len(), src.lines().count()));
    }

    #[test]
    fn kinds_cover_paragraph_heading_list_code_rule() {
        let got = kinds("# H\n\npara\n\n- item\n\n```\ncode\n```\n\n---\n");
        assert!(matches!(got[0], MdBlockKind::Heading { level: 1 }));
        assert!(matches!(got[1], MdBlockKind::Paragraph));
        assert!(matches!(got[2], MdBlockKind::ListItem { .. }));
        assert!(matches!(got[3], MdBlockKind::Code));
        assert!(matches!(got[4], MdBlockKind::Rule));
    }

    /// RTF size ceiling for adversarial input. Linear in the source, with room
    /// for the header and one control word per block. Not exponential in depth.
    const RTF_SIZE_FACTOR: usize = 8;
    const RTF_SIZE_SLACK: usize = 4 * 1024;

    fn assert_li_capped(rtf: &str) {
        let bytes = rtf.as_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            let is_control = bytes[i] == b'\\'
                && bytes[i + 1] == b'l'
                && bytes[i + 2] == b'i'
                && bytes.get(i + 3).is_some_and(|b| b.is_ascii_digit())
                && i.checked_sub(1).is_none_or(|prev| bytes[prev] != b'\\');
            if !is_control {
                i += 1;
                continue;
            }
            let mut j = i + 3;
            let mut val: i32 = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                val = val
                    .saturating_mul(10)
                    .saturating_add(i32::from(bytes[j] - b'0'));
                j += 1;
            }
            assert!(
                val <= MAX_LEFT_INDENT_TWIPS,
                "\\li{val} exceeds {MAX_LEFT_INDENT_TWIPS}"
            );
            i = j;
        }
    }

    /// Parse, build the model, and emit RTF on the default test-thread stack.
    fn assert_adversarial(source: &str) {
        assert!(
            source.len() <= MARKDOWN_MAX_BYTES,
            "source is {} bytes",
            source.len()
        );
        let lines = source.lines().count();
        assert!(
            lines <= MARKDOWN_MAX_LINES,
            "source has {lines} lines, cap is {MARKDOWN_MAX_LINES}"
        );
        let doc = parse_markdown(source);
        assert!(doc.blocks.iter().all(|block| block.indent <= MAX_INDENT));
        let rtf = document_to_rtf(&doc, None);
        let limit = source
            .len()
            .saturating_mul(RTF_SIZE_FACTOR)
            .saturating_add(RTF_SIZE_SLACK);
        assert!(
            rtf.len() <= limit,
            "rtf is {} bytes for {} bytes of input (limit {limit})",
            rtf.len(),
            source.len()
        );
        assert!(rtf.starts_with(r"{\rtf1"));
        assert!(rtf.ends_with('}'));
        assert_li_capped(&rtf);
    }

    #[test]
    fn deep_nested_blockquotes_stay_bounded() {
        let mut source = ">".repeat(10_000);
        source.push_str(" quoted\n");
        assert_adversarial(&source);
        let doc = parse_markdown(&source);
        let deepest = doc
            .blocks
            .iter()
            .map(|block| block.indent)
            .max()
            .unwrap_or(0);
        assert_eq!(deepest, MAX_INDENT);
    }

    #[test]
    fn deep_nested_lists_stay_bounded() {
        let mut source = "- ".repeat(10_000);
        source.push_str("item\n");
        assert_adversarial(&source);
        let doc = parse_markdown(&source);
        let deepest = doc
            .blocks
            .iter()
            .map(|block| block.indent)
            .max()
            .unwrap_or(0);
        assert_eq!(deepest, MAX_INDENT);
    }

    #[test]
    fn one_long_line_stays_bounded() {
        let source = "a".repeat(60 * 1024);
        assert_eq!(source.len(), 60 * 1024);
        assert_adversarial(&source);
        let doc = parse_markdown(&source);
        let text: String = doc.blocks.iter().map(plain).collect();
        assert_eq!(text, source);
    }

    #[test]
    fn deep_nested_emphasis_stays_bounded() {
        let mut source = "*".repeat(5_000);
        source.push_str("word");
        source.push_str(&"*".repeat(5_000));
        assert_adversarial(&source);
        let doc = parse_markdown(&source);
        let text: String = doc.blocks.iter().map(plain).collect();
        assert!(text.contains("word"));
    }

    #[test]
    fn unclosed_fence_stays_bounded() {
        let mut source = String::from("Before\n\n```rust\n");
        source.push_str(&"let value = 1;\n".repeat(200));
        assert_eq!(source.matches("```").count(), 1);
        assert_adversarial(&source);
        let doc = parse_markdown(&source);
        assert!(doc
            .blocks
            .iter()
            .any(|block| matches!(block.kind, MdBlockKind::Code)
                && plain(block).contains("let value")));
    }

    #[test]
    fn writer_clamps_indent_past_the_model_cap() {
        let block = MdBlock {
            kind: MdBlockKind::Paragraph,
            indent: u8::MAX,
            quote: true,
            runs: vec![MdRun {
                text: "x".to_string(),
                style: MdStyle::default(),
            }],
        };
        let expected = i32::from(MAX_INDENT) * TWIPS_PER_INDENT + QUOTE_EXTRA_TWIPS;
        assert_eq!(left_indent_twips(&block), expected);
        assert!(expected < i32::from(u8::MAX) * TWIPS_PER_INDENT);
        let rtf = document_to_rtf(
            &MdDocument {
                blocks: vec![block],
            },
            None,
        );
        assert!(rtf.contains(&format!("\\li{expected}")));
        assert_li_capped(&rtf);
    }
}
