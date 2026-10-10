use std::collections::HashMap;

use scraper::{ElementRef, Html, Node, Selector};

use crate::core::tools::webpage::display_lines;

/// Elements whose contents are never part of what a reader sees.
const HIDDEN_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "head", "iframe", "canvas", "object",
];

/// Elements that start and end a line of the text view.
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "caption",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "legend",
    "main",
    "nav",
    "ol",
    "option",
    "p",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "tr",
    "ul",
];

/// Bytes looked at to decide whether a page is html.
const HTML_SNIFF_BYTES: usize = 8_192;

const MAX_OUTLINE_HEADINGS: usize = 40;
const MAX_OUTLINE_PATTERNS: usize = 15;
const MAX_OUTLINE_ITEMS: usize = 10;

/// An element class must appear at least this often to be listed as repeated.
const MIN_REPEATS: usize = 3;

/// The longest sample of an element's text the outline shows.
const MAX_SAMPLE_CHARS: usize = 80;

/// Only html pages get the text view by default; anything else reads raw.
pub fn looks_like_html(text: &str) -> bool {
    let mut end = text.len().min(HTML_SNIFF_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let start = text[..end].to_ascii_lowercase();
    ["<!doctype html", "<html", "<body"]
        .iter()
        .any(|tag| start.contains(tag))
}

/// The page as a reader sees it, one block per line, without scripts, styles
/// or markup.
pub fn text_lines(document: &Html) -> Vec<String> {
    let mut renderer = TextRenderer::default();
    renderer.render(vec![Visit::Element(document.root_element())]);
    renderer.finish()
}

/// Splits rendered lines the way raw lines are, so every part has a number.
pub fn split_long(lines: Vec<String>) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| display_lines(line))
        .map(str::to_string)
        .collect()
}

/// One element's text on a single line, without the element's own `#` or `-`.
/// A script or style is shown as its source, since that is what was selected.
pub fn element_text(element: ElementRef) -> String {
    let name = element.value().name();
    if HIDDEN_TAGS.contains(&name) {
        return collapse(&element.text().collect::<String>());
    }
    let mut renderer = TextRenderer::default();
    if name == "a" {
        renderer.render(vec![Visit::Element(element)]);
    } else {
        let mut stack = Vec::new();
        push_children(&mut stack, element);
        renderer.render(stack);
    }
    renderer.finish().join(" / ")
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A step of the text view's walk. The walk keeps its own stack so that
/// deeply nested pages cannot overflow the thread's.
enum Visit<'a> {
    Text(&'a str),
    Element(ElementRef<'a>),
    Flush,
    EndPrefixed,
    EndLink {
        href: &'a str,
        flushes: usize,
        from: usize,
    },
}

fn push_children<'a>(stack: &mut Vec<Visit<'a>>, element: ElementRef<'a>) {
    for child in element.children().rev() {
        match child.value() {
            Node::Text(text) => stack.push(Visit::Text(text)),
            Node::Element(_) => stack.extend(ElementRef::wrap(child).map(Visit::Element)),
            _ => {}
        }
    }
}

#[derive(Default)]
struct TextRenderer {
    lines: Vec<String>,
    line: String,
    // a heading or list marker waiting for the first line of text under it
    prefix: String,
    // counts every flush, so a link can tell whether its text stayed inline
    flushes: usize,
}

impl TextRenderer {
    fn finish(mut self) -> Vec<String> {
        self.flush();
        self.lines
    }

    fn flush(&mut self) {
        let line = self.line.trim();
        if !line.is_empty() {
            self.lines.push(format!("{}{}", self.prefix, line));
            self.prefix.clear();
        }
        self.line.clear();
        self.flushes += 1;
    }

    fn space(&mut self) {
        if !self.line.is_empty() && !self.line.ends_with(' ') {
            self.line.push(' ');
        }
    }

    fn text(&mut self, text: &str) {
        if text.starts_with(char::is_whitespace) {
            self.space();
        }
        let mut words = text.split_whitespace();
        if let Some(first) = words.next() {
            self.line.push_str(first);
            for word in words {
                self.line.push(' ');
                self.line.push_str(word);
            }
            if text.ends_with(char::is_whitespace) {
                self.space();
            }
        }
    }

    fn render<'a>(&mut self, mut stack: Vec<Visit<'a>>) {
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Text(text) => self.text(text),
                Visit::Element(element) => self.element(element, &mut stack),
                Visit::Flush => self.flush(),
                Visit::EndPrefixed => {
                    self.flush();
                    self.prefix.clear();
                }
                Visit::EndLink {
                    href,
                    flushes,
                    from,
                } => self.end_link(href, flushes, from),
            }
        }
    }

    fn element<'a>(&mut self, element: ElementRef<'a>, stack: &mut Vec<Visit<'a>>) {
        let name = element.value().name();
        if HIDDEN_TAGS.contains(&name) {
            return;
        }
        match name {
            "br" | "hr" => self.flush(),
            "pre" => self.pre(element),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = (name.as_bytes()[1] - b'0') as usize;
                self.prefixed(&format!("{} ", "#".repeat(level)), element, stack);
            }
            "li" => self.prefixed("- ", element, stack),
            "td" | "th" => {
                if !self.line.trim().is_empty() {
                    self.line.truncate(self.line.trim_end().len());
                    self.line.push_str(" | ");
                }
                push_children(stack, element);
            }
            "a" => self.link(element, stack),
            _ if BLOCK_TAGS.contains(&name) => {
                self.flush();
                stack.push(Visit::Flush);
                push_children(stack, element);
            }
            _ => push_children(stack, element),
        }
    }

    /// Starts the element's first line of text with `prefix`, so a list item
    /// that opens with a heading still reads `- ## title`.
    fn prefixed<'a>(&mut self, prefix: &str, element: ElementRef<'a>, stack: &mut Vec<Visit<'a>>) {
        self.flush();
        self.prefix.push_str(prefix);
        stack.push(Visit::EndPrefixed);
        push_children(stack, element);
    }

    /// Preformatted text keeps its own line breaks.
    fn pre(&mut self, element: ElementRef) {
        self.flush();
        let text: String = element.text().collect();
        for line in text.lines() {
            if !line.trim().is_empty() {
                let prefix = std::mem::take(&mut self.prefix);
                self.lines.push(format!("{}{}", prefix, line.trim_end()));
            }
        }
    }

    /// Inline links read `[text](url)`. A link wrapped around blocks, such as
    /// a whole card, gets its url on a line after them.
    fn link<'a>(&mut self, element: ElementRef<'a>, stack: &mut Vec<Visit<'a>>) {
        let href = element.attr("href").map(str::trim).filter(|href| {
            !href.is_empty()
                && !href.starts_with('#')
                && !href.to_ascii_lowercase().starts_with("javascript:")
        });
        if let Some(href) = href {
            stack.push(Visit::EndLink {
                href,
                flushes: self.flushes,
                from: self.line.len(),
            });
        }
        push_children(stack, element);
    }

    fn end_link(&mut self, href: &str, flushes: usize, from: usize) {
        if self.flushes != flushes {
            self.flush();
            self.lines.push(format!("(link: {})", href));
            return;
        }

        let segment = self.line.split_off(from);
        let label = segment.trim();
        if label.is_empty() {
            self.line.push_str(&segment);
            return;
        }
        if segment.starts_with(' ') {
            self.space();
        }
        self.line.push_str(&format!("[{}]({})", label, href));
        if segment.ends_with(' ') {
            self.space();
        }
    }
}

// Outline ///////////////////////////
//////////////////////////////////////

/// A short map of the page to choose selectors from: title, headings,
/// repeated elements with a sample of each, tables, forms and embedded json.
pub fn outline(document: &Html) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(title) = first(document, "title") {
        let title = collapse(&title.text().collect::<String>());
        if !title.is_empty() {
            out.push(format!("title: {}", title));
        }
    }

    let elements = visible_elements(document.root_element());
    let count = |tag: &str| {
        elements
            .iter()
            .filter(|element| element.value().name() == tag)
            .count()
    };
    out.push(format!(
        "text view: {} lines; links: {}; images: {}; tables: {}; forms: {}",
        split_long(text_lines(document)).len(),
        count("a"),
        count("img"),
        count("table"),
        count("form")
    ));

    let headings: Vec<String> = elements
        .iter()
        .filter_map(|element| {
            let name = element.value().name();
            let level = match name.as_bytes() {
                [b'h', digit @ b'1'..=b'6'] => (digit - b'0') as usize,
                _ => return None,
            };
            let text = collapse(&element.text().collect::<String>());
            (!text.is_empty()).then(|| format!("{}{} {}", "  ".repeat(level), name, text))
        })
        .collect();
    section(&mut out, "headings:", headings, MAX_OUTLINE_HEADINGS);

    section(
        &mut out,
        "repeated elements (selector ×count: first one's text):",
        repeated(&elements),
        MAX_OUTLINE_PATTERNS,
    );

    let tables = elements
        .iter()
        .filter(|element| element.value().name() == "table")
        .map(|table| {
            let rows = table
                .descendants()
                .filter_map(ElementRef::wrap)
                .filter(|row| row.value().name() == "tr")
                .count();
            format!(
                "  {} ({} rows): {}",
                selector_for(*table),
                rows,
                sample(*table)
            )
        })
        .collect();
    section(&mut out, "tables:", tables, MAX_OUTLINE_ITEMS);

    let forms = elements
        .iter()
        .filter(|element| element.value().name() == "form")
        .map(|form| {
            let fields: Vec<&str> = form
                .descendants()
                .filter_map(ElementRef::wrap)
                .filter(|field| matches!(field.value().name(), "input" | "select" | "textarea"))
                .filter_map(|field| field.attr("name"))
                .collect();
            format!(
                "  {} {} {} fields: {}",
                selector_for(*form),
                form.attr("method").unwrap_or("get").to_ascii_lowercase(),
                form.attr("action").unwrap_or("(this page)"),
                if fields.is_empty() {
                    "(none named)".to_string()
                } else {
                    fields.join(", ")
                }
            )
        })
        .collect();
    section(&mut out, "forms:", forms, MAX_OUTLINE_ITEMS);

    let scripts = Selector::parse("script").expect("a valid selector");
    let embedded = document
        .select(&scripts)
        .filter(|script| {
            let kind = script.attr("type").unwrap_or("").to_ascii_lowercase();
            kind.contains("json") && !kind.contains("importmap")
        })
        .map(|script| {
            let selector = match script.attr("id").filter(|id| usable(id)) {
                Some(id) => format!("script#{}", id),
                None => format!(
                    "script[type=\"{}\"]",
                    script.attr("type").unwrap_or_default()
                ),
            };
            let size = script
                .text()
                .map(|part| part.chars().count())
                .sum::<usize>();
            format!("  {} ({} chars)", selector, size)
        })
        .collect();
    section(&mut out, "embedded json:", embedded, MAX_OUTLINE_ITEMS);

    out
}

fn first<'a>(document: &'a Html, selector: &str) -> Option<ElementRef<'a>> {
    let selector = Selector::parse(selector).expect("a valid selector");
    document.select(&selector).next()
}

/// Every element outside the hidden ones, in document order.
fn visible_elements(root: ElementRef) -> Vec<ElementRef> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(element) = stack.pop() {
        let start = stack.len();
        for child in element.children().filter_map(ElementRef::wrap) {
            if !HIDDEN_TAGS.contains(&child.value().name()) {
                stack.push(child);
            }
        }
        stack[start..].reverse();
        if element != root {
            out.push(element);
        }
    }
    out
}

fn section(out: &mut Vec<String>, heading: &str, mut items: Vec<String>, most: usize) {
    if items.is_empty() {
        return;
    }
    let more = items.len().saturating_sub(most);
    items.truncate(most);
    out.push(heading.to_string());
    out.extend(items);
    if more > 0 {
        out.push(format!("  … and {} more", more));
    }
}

/// Elements sharing a tag and classes, most common first. These are usually
/// the rows of whatever data the page lists.
fn repeated(elements: &[ElementRef]) -> Vec<String> {
    let mut seen: HashMap<String, (usize, usize)> = HashMap::new();
    for (index, element) in elements.iter().enumerate() {
        if let Some(signature) = class_selector(*element) {
            seen.entry(signature).or_insert((0, index)).0 += 1;
        }
    }
    let mut patterns: Vec<(String, usize, usize)> = seen
        .into_iter()
        .filter(|(_, (count, _))| *count >= MIN_REPEATS)
        .map(|(signature, (count, index))| (signature, count, index))
        .collect();
    patterns.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    patterns
        .into_iter()
        .map(|(signature, count, index)| {
            format!("  {} ×{}: {}", signature, count, sample(elements[index]))
        })
        .collect()
}

/// `tag.class.class` from the classes a css selector can name unescaped, or
/// nothing when there are none.
fn class_selector(element: ElementRef) -> Option<String> {
    // the class attribute rather than `classes()`, which loses the source order
    let mut classes: Vec<&str> = Vec::new();
    for class in element.attr("class").unwrap_or("").split_whitespace() {
        if usable(class) && !classes.contains(&class) {
            classes.push(class);
        }
    }
    if classes.is_empty() {
        return None;
    }
    Some(format!("{}.{}", element.value().name(), classes.join(".")))
}

fn selector_for(element: ElementRef) -> String {
    let name = element.value().name();
    match element.attr("id").filter(|id| usable(id)) {
        Some(id) => format!("{}#{}", name, id),
        None => class_selector(element).unwrap_or_else(|| name.to_string()),
    }
}

fn usable(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn sample(element: ElementRef) -> String {
    let text = element_text(element);
    match text.char_indices().nth(MAX_SAMPLE_CHARS) {
        Some((split, _)) => format!("{}…", &text[..split]),
        None if text.is_empty() => "(no text)".to_string(),
        None => text,
    }
}
