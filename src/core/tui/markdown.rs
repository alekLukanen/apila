use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::TAB_WIDTH;

/// Drawn in place of a character too wide for the line it has to go on.
const TOO_WIDE: &str = "\u{fffd}";

/// A run of text and the style it is drawn in.
type Piece = (String, Style);

/// `text` as markdown, hard wrapped so no line is wider than `width` and the
/// reader can scroll it by whole lines. Markers are styled, not drawn.
pub fn markdown_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut renderer = Renderer::new(width.max(1));
    for event in Parser::new_ext(text, options) {
        renderer.event(event);
    }
    renderer.finish()
}

/// `text` taken as it is, wrapped the way [`markdown_lines`] wraps prose.
pub fn plain_lines(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    wrap(&[(text.to_string(), style)], &[], &[], width, false)
}

/// What the text being rendered sits inside, outermost first.
enum Container {
    Quote,
    /// The number the next item takes, for an ordered list.
    List(Option<u64>),
    /// `used` once the marker has been drawn; later lines are only indented.
    Item {
        marker: String,
        used: bool,
    },
}

#[derive(Default)]
enum Raw {
    #[default]
    None,
    Code(String),
    Html(String),
}

struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    containers: Vec<Container>,
    styles: Vec<Style>,
    /// The inline text of the block being built.
    pieces: Vec<Piece>,
    /// Code and html blocks are collected whole and drawn verbatim.
    raw: Raw,
    links: Vec<String>,
    table: Vec<Vec<Piece>>,
    row: Vec<Vec<Piece>>,
    head: bool,
    blank_pending: bool,
}

impl Renderer {
    fn new(width: usize) -> Renderer {
        Renderer {
            width,
            out: Vec::new(),
            containers: Vec::new(),
            styles: Vec::new(),
            pieces: Vec::new(),
            raw: Raw::None,
            links: Vec::new(),
            table: Vec::new(),
            row: Vec::new(),
            head: false,
            blank_pending: false,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        self.out
    }

    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |style, patch| style.patch(*patch))
    }

    fn push_text(&mut self, text: &str) {
        match &mut self.raw {
            Raw::Code(buffer) | Raw::Html(buffer) => buffer.push_str(text),
            Raw::None => {
                let style = self.style();
                self.pieces.push((text.to_string(), style));
            }
        }
    }

    fn push_styled(&mut self, text: &str, style: Style) {
        let style = self.style().patch(style);
        self.pieces.push((text.to_string(), style));
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => self.push_styled(&text, Style::default().yellow()),
            Event::Html(text)
            | Event::InlineHtml(text)
            | Event::FootnoteReference(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => self.push_text(&text),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => {
                self.flush();
                self.separate();
                let (first, rest) = self.prefixes();
                let used = prefix_width(fit(&first, self.width));
                let rule = "─".repeat(self.width - used);
                self.out.extend(wrap(
                    &[(rule, Style::default().dark_gray())],
                    &first,
                    &rest,
                    self.width,
                    true,
                ));
                self.mark_used();
                self.blank_pending = true;
            }
            Event::TaskListMarker(done) => self.push_text(if done { "[x] " } else { "[ ] " }),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                let mut style = Style::default().bold().cyan();
                if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                    style = style.underlined();
                }
                self.styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.containers.push(Container::Quote);
                self.styles.push(Style::default().dim());
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.raw = Raw::Code(String::new());
            }
            Tag::HtmlBlock => {
                self.flush();
                self.raw = Raw::Html(String::new());
            }
            Tag::List(first) => {
                self.flush();
                self.containers.push(Container::List(first));
            }
            Tag::Item => {
                self.flush();
                let marker = self.next_marker();
                self.containers.push(Container::Item {
                    marker,
                    used: false,
                });
            }
            Tag::Table(_) => {
                self.flush();
                self.table.clear();
            }
            Tag::TableHead => {
                self.head = true;
                self.row.clear();
            }
            Tag::TableRow => self.row.clear(),
            Tag::TableCell => self.pieces.clear(),
            Tag::Emphasis => self.styles.push(Style::default().italic()),
            Tag::Strong => self.styles.push(Style::default().bold()),
            Tag::Strikethrough => self.styles.push(Style::default().crossed_out()),
            Tag::Link { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.styles.push(Style::default().blue().underlined());
            }
            Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.push_text("[image: ");
            }
            // footnotes, definition lists and the like keep their text unstyled
            _ => self.styles.push(Style::default()),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.blank_pending = true;
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.styles.pop();
                self.blank_pending = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.styles.pop();
                self.containers.pop();
                self.blank_pending = true;
            }
            TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                match std::mem::take(&mut self.raw) {
                    Raw::Code(text) => self.raw_block(&text, true),
                    Raw::Html(text) => self.raw_block(&text, false),
                    Raw::None => return,
                }
                self.blank_pending = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.containers.pop();
                // a tight list nested in an item runs straight on
                if !self
                    .containers
                    .iter()
                    .any(|container| matches!(container, Container::Item { .. }))
                {
                    self.blank_pending = true;
                }
            }
            TagEnd::Item => {
                // an empty item still shows its marker
                if self.pieces.is_empty()
                    && matches!(
                        self.containers.last(),
                        Some(Container::Item { used: false, .. })
                    )
                {
                    self.pieces.push((String::new(), Style::default()));
                }
                self.flush();
                self.containers.pop();
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.pieces);
                self.row.push(cell);
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let mut line = Vec::new();
                let head = std::mem::take(&mut self.head);
                for (index, cell) in std::mem::take(&mut self.row).into_iter().enumerate() {
                    if index > 0 {
                        line.push((" | ".to_string(), Style::default().dark_gray()));
                    }
                    line.extend(
                        cell.into_iter()
                            .map(|(text, style)| (text, if head { style.bold() } else { style })),
                    );
                }
                self.table.push(line);
            }
            TagEnd::Table => {
                self.separate();
                let (first, rest) = self.prefixes();
                for row in std::mem::take(&mut self.table) {
                    self.out
                        .extend(wrap(&row, &first, &rest, self.width, false));
                }
                self.mark_used();
                self.blank_pending = true;
            }
            TagEnd::Link => {
                self.styles.pop();
                self.close_link();
            }
            TagEnd::Image => {
                self.push_text("]");
                self.close_link();
            }
            _ => {
                self.styles.pop();
            }
        }
    }

    /// The innermost list's marker for its next item, counting it off when
    /// the list is numbered.
    fn next_marker(&mut self) -> String {
        let list = self
            .containers
            .iter_mut()
            .rev()
            .find_map(|container| match container {
                Container::List(next) => Some(next),
                _ => None,
            });
        match list {
            Some(Some(number)) => {
                let marker = format!("{}. ", number);
                *number += 1;
                marker
            }
            _ => "• ".to_string(),
        }
    }

    fn close_link(&mut self) {
        if let Some(url) = self.links.pop().filter(|url| !url.is_empty()) {
            self.push_styled(&format!(" ({})", url), Style::default().dark_gray());
        }
    }

    /// A blank line between blocks, once there is something above to keep
    /// apart from.
    fn separate(&mut self) {
        if std::mem::take(&mut self.blank_pending) && !self.out.is_empty() {
            self.out.push(Line::from(""));
        }
    }

    /// Draws the inline text built so far as one block.
    fn flush(&mut self) {
        if self.pieces.is_empty() {
            return;
        }
        let pieces = std::mem::take(&mut self.pieces);
        self.separate();
        let (first, rest) = self.prefixes();
        let lines = wrap(&pieces, &first, &rest, self.width, false);
        // a block of nothing but an empty item still shows its marker
        if lines.is_empty() {
            self.out.push(Line::from(to_spans(fit(&first, self.width))));
        }
        self.out.extend(lines);
        self.mark_used();
    }

    fn raw_block(&mut self, text: &str, gutter: bool) {
        self.separate();
        let (mut first, mut rest) = self.prefixes();
        if gutter {
            let bar = ("│ ".to_string(), Style::default().dark_gray());
            first.push(bar.clone());
            rest.push(bar);
        }
        let style = if gutter {
            Style::default().dim()
        } else {
            self.style()
        };
        let text = text.strip_suffix('\n').unwrap_or(text);
        for (index, line) in text.split('\n').enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let prefix = if index == 0 { &first } else { &rest };
            let lines = wrap(
                &[(line.to_string(), style)],
                prefix,
                &rest,
                self.width,
                true,
            );
            if lines.is_empty() {
                self.out.push(Line::from(to_spans(fit(prefix, self.width))));
            }
            self.out.extend(lines);
        }
        self.mark_used();
    }

    /// The prefix of a block's first line, and of the lines after it.
    fn prefixes(&self) -> (Vec<Piece>, Vec<Piece>) {
        let (mut first, mut rest) = (Vec::new(), Vec::new());
        for container in &self.containers {
            match container {
                Container::Quote => {
                    let bar = ("▎ ".to_string(), Style::default().dark_gray());
                    first.push(bar.clone());
                    rest.push(bar);
                }
                Container::List(_) => {}
                Container::Item { marker, used } => {
                    let indent = " ".repeat(marker.width());
                    if *used {
                        first.push((indent.clone(), Style::default()));
                    } else {
                        first.push((marker.clone(), Style::default().cyan()));
                    }
                    rest.push((indent, Style::default()));
                }
            }
        }
        (first, rest)
    }

    fn mark_used(&mut self) {
        for container in &mut self.containers {
            if let Container::Item { used, .. } = container {
                *used = true;
            }
        }
    }
}

fn prefix_width(prefix: &[Piece]) -> usize {
    prefix.iter().map(|(text, _)| text.width()).sum()
}

/// A prefix leaving no room for text is dropped rather than overflowing.
fn fit(prefix: &[Piece], width: usize) -> &[Piece] {
    if prefix_width(prefix) >= width {
        &[]
    } else {
        prefix
    }
}

fn to_spans(pieces: &[Piece]) -> Vec<Span<'static>> {
    pieces
        .iter()
        .map(|(text, style)| Span::styled(text.clone(), *style))
        .collect()
}

/// Control characters other than newlines and tabs would draw as glyphs or
/// move the cursor.
fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect()
}

/// One grapheme on a line being built, with its display width.
struct Cell {
    text: String,
    style: Style,
    width: usize,
}

/// Wraps `pieces` to `width`, measured in display columns. Prose breaks after
/// the last space; `hard` keeps every space and cuts wherever the line is full.
fn wrap(
    pieces: &[Piece],
    first: &[Piece],
    rest: &[Piece],
    width: usize,
    hard: bool,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let (first, rest) = (fit(first, width), fit(rest, width));
    let mut wrapper = Wrapper {
        out: Vec::new(),
        line: Vec::new(),
        column: 0,
        last_space: None,
        first,
        rest,
        width,
        hard,
    };
    for (text, style) in pieces {
        for grapheme in sanitize(text).graphemes(true) {
            wrapper.push(grapheme, *style);
        }
    }
    if !wrapper.line.is_empty() {
        wrapper.emit();
    }
    wrapper.out
}

struct Wrapper<'a> {
    out: Vec<Line<'static>>,
    line: Vec<Cell>,
    column: usize,
    /// The index of the space the line can break after.
    last_space: Option<usize>,
    first: &'a [Piece],
    rest: &'a [Piece],
    width: usize,
    hard: bool,
}

impl Wrapper<'_> {
    fn available(&self) -> usize {
        let prefix = if self.out.is_empty() {
            self.first
        } else {
            self.rest
        };
        self.width - prefix_width(prefix)
    }

    fn push(&mut self, grapheme: &str, style: Style) {
        match grapheme {
            "\n" => {
                self.emit();
                return;
            }
            "\t" => {
                let advance = TAB_WIDTH - (self.column % TAB_WIDTH);
                for _ in 0..advance {
                    self.push(" ", style);
                }
                return;
            }
            _ => {}
        }

        let mut width = grapheme.width();
        let mut text = grapheme;
        if width > 0 && self.column + width > self.available() {
            // prose breaks at a space that doesn't fit, so the word before it
            // stays put
            if !self.hard && grapheme == " " {
                self.last_space = None;
            }
            self.break_line();
            // a wrapped line of prose starts on text
            if !self.hard && grapheme == " " && self.line.is_empty() {
                return;
            }
            // the word carried over fills the line by itself
            if self.column + width > self.available() && !self.line.is_empty() {
                self.emit();
            }
        }
        if self.column + width > self.available() {
            text = TOO_WIDE;
            width = TOO_WIDE.width();
        }

        if !self.hard && text == " " && self.line.iter().any(|cell| cell.text != " ") {
            self.last_space = Some(self.line.len());
        }
        self.line.push(Cell {
            text: text.to_string(),
            style,
            width,
        });
        self.column += width;
    }

    /// Ends the full line, carrying the word it was in the middle of onto
    /// the next one.
    fn break_line(&mut self) {
        let carried = match self.last_space {
            Some(index) if !self.hard => self.line.split_off(index + 1),
            _ => Vec::new(),
        };
        if !self.hard {
            while self.line.last().is_some_and(|cell| cell.text == " ") {
                self.line.pop();
            }
        }
        self.emit();
        let carried: Vec<Cell> = carried
            .into_iter()
            .skip_while(|cell| cell.text == " ")
            .collect();
        self.column = carried.iter().map(|cell| cell.width).sum();
        self.line = carried;
    }

    fn emit(&mut self) {
        let prefix = if self.out.is_empty() {
            self.first
        } else {
            self.rest
        };
        let mut text: Vec<Span<'static>> = Vec::new();
        for cell in self.line.drain(..) {
            match text.last_mut() {
                Some(span) if span.style == cell.style => {
                    span.content.to_mut().push_str(&cell.text)
                }
                _ => text.push(Span::styled(cell.text, cell.style)),
            }
        }
        let mut spans = to_spans(prefix);
        spans.extend(text);
        self.out.push(Line::from(spans));
        self.column = 0;
        self.last_space = None;
    }
}
