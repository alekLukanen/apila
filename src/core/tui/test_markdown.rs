use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

use crate::core::memory::store::LISTED_BODY_CHARS;

use super::markdown::markdown_lines;

fn text(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

/// The span holding `needle`, from the line it is on.
fn span_with<'a>(lines: &'a [Line], needle: &str) -> &'a ratatui::text::Span<'a> {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains(needle))
        .unwrap_or_else(|| panic!("`{}` is drawn", needle))
}

fn assert_fits(lines: &[Line], width: usize) {
    for line in lines {
        assert!(line.width() <= width, "{:?} is wider than {}", line, width);
    }
}

#[test]
fn markers_are_styled_rather_than_drawn() {
    let lines = markdown_lines("## Steps\n\nRun *this* and **that** with `cargo`.", 80);
    let shown = text(&lines).join("\n");

    assert!(!shown.contains("##"), "{shown}");
    assert!(!shown.contains('*'), "{shown}");
    assert!(!shown.contains('`'), "{shown}");
    assert_eq!(text(&lines)[0], "Steps");
    // a heading is followed by a blank line
    assert_eq!(text(&lines)[1], "");

    let heading = span_with(&lines, "Steps");
    assert_eq!(heading.style.fg, Some(Color::Cyan));
    assert!(heading.style.add_modifier.contains(Modifier::BOLD));
    assert!(heading.style.add_modifier.contains(Modifier::UNDERLINED));
    assert!(span_with(&lines, "this")
        .style
        .add_modifier
        .contains(Modifier::ITALIC));
    assert!(span_with(&lines, "that")
        .style
        .add_modifier
        .contains(Modifier::BOLD));
    assert_eq!(span_with(&lines, "cargo").style.fg, Some(Color::Yellow));
}

#[test]
fn a_code_block_is_kept_verbatim_with_its_tabs_expanded() {
    let lines = markdown_lines("```\nfn main() {\n\tlet  x = 1;\n}\n```", 80);

    assert_eq!(
        text(&lines),
        vec!["│ fn main() {", "│         let  x = 1;", "│ }"]
    );
}

#[test]
fn a_long_code_line_wraps_onto_gutter_lines() {
    let lines = markdown_lines(&format!("```\n{}\n```", "x".repeat(25)), 12);

    assert_eq!(
        text(&lines),
        vec!["│ xxxxxxxxxx", "│ xxxxxxxxxx", "│ xxxxx"]
    );
}

#[test]
fn list_items_get_a_hanging_indent() {
    let lines = markdown_lines("- first item that wraps\n  - nested\n1. one\n2. two", 14);

    assert_eq!(
        text(&lines),
        vec![
            "• first item",
            "  that wraps",
            "  • nested",
            "",
            "1. one",
            "2. two",
        ]
    );
}

#[test]
fn a_task_list_shows_its_boxes() {
    let lines = markdown_lines("- [x] done\n- [ ] todo", 40);

    assert_eq!(text(&lines), vec!["• [x] done", "• [ ] todo"]);
}

#[test]
fn a_quote_and_a_rule_are_drawn() {
    let lines = markdown_lines("> quoted\n\n---\n\nafter", 10);

    assert_eq!(
        text(&lines),
        vec!["▎ quoted", "", "──────────", "", "after"]
    );
}

#[test]
fn a_link_keeps_its_url() {
    let lines = markdown_lines("see [the docs](https://example.com/a)", 80);

    assert_eq!(text(&lines), vec!["see the docs (https://example.com/a)"]);
    let link = span_with(&lines, "the docs");
    assert_eq!(link.style.fg, Some(Color::Blue));
    assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn an_image_is_shown_as_its_alt_text_and_url() {
    let lines = markdown_lines("![a cat](cat.png)", 80);

    assert_eq!(text(&lines), vec!["[image: a cat] (cat.png)"]);
}

#[test]
fn a_table_is_drawn_as_joined_rows() {
    let lines = markdown_lines("| a | b |\n|---|---|\n| 1 | 2 |", 80);

    assert_eq!(text(&lines), vec!["a | b", "1 | 2"]);
    assert!(span_with(&lines, "a")
        .style
        .add_modifier
        .contains(Modifier::BOLD));
}

#[test]
fn html_is_shown_as_text() {
    let lines = markdown_lines("<div>\nhi\n</div>", 80);

    assert_eq!(text(&lines), vec!["<div>", "hi", "</div>"]);
}

#[test]
fn an_empty_body_draws_nothing() {
    assert!(markdown_lines("", 40).is_empty());
}

#[test]
fn no_line_is_wider_than_asked_for() {
    let body = "# A heading long enough to wrap around\n\n\
                > a quote that goes on and on and on\n\n\
                - an item\n  - a nested item with a lot to say about it\n\n\
                ```\n\tcode that runs long past the edge of the reader\n```\n\n\
                averyveryverylongwordwithnowheretobreak and then some";
    for width in 1..40 {
        assert_fits(&markdown_lines(body, width), width);
    }
}

#[test]
fn a_word_that_ends_exactly_at_the_edge_stays_on_its_line() {
    assert_eq!(text(&markdown_lines("ab cd ef", 5)), vec!["ab cd", "ef"]);
}

#[test]
fn an_empty_item_too_deep_for_the_width_stays_within_it() {
    for width in 1..4 {
        assert_fits(
            &markdown_lines(
                "-

> -", width,
            ),
            width,
        );
    }
}

#[test]
fn wide_characters_never_overflow_the_line() {
    let body = "漢字の文章がここにあります 😀😀😀 and ascii\n\n```\n漢字漢字漢字\n```";
    for width in 1..20 {
        assert_fits(&markdown_lines(body, width), width);
    }
    // a wide character that would land on the last column moves down a line
    assert_eq!(text(&markdown_lines("a漢", 2)), vec!["a", "漢"]);
}

#[test]
fn control_characters_are_not_drawn() {
    let lines = markdown_lines("bell\u{7} and \u{1b}[31mred", 80);

    assert_eq!(text(&lines), vec!["bell and [31mred"]);
}

#[test]
fn width_zero_still_finishes() {
    let lines = markdown_lines("# hi\n\nsome text\n\n```\ncode\n```", 0);
    assert!(!lines.is_empty());
}

#[test]
fn malformed_input_does_not_panic() {
    for body in [
        "**unclosed",
        "[link](",
        "```\nno end",
        "> > > - 1. ```",
        "| a |\n|",
        "<div",
        "\u{0}\u{7f}\u{9b}",
    ] {
        for width in [0, 1, 3, 40] {
            markdown_lines(body, width);
        }
    }
}

#[test]
fn every_word_survives_in_order() {
    let body = "# Title\n\nThe *quick* brown **fox** jumps over `the` lazy dog.\n\n\
                - one\n- two\n\n> three four\n\n```\nfive six\n```";
    let shown = text(&markdown_lines(body, 7)).join(" ");
    let words: Vec<&str> = shown
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();

    assert_eq!(
        words,
        vec![
            "Title", "The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog", "one",
            "two", "three", "four", "five", "six",
        ]
    );
}

#[test]
fn a_large_body_renders_quickly() {
    let body = "word ".repeat(LISTED_BODY_CHARS / 5);
    let started = Instant::now();
    let lines = markdown_lines(&body, 60);

    assert!(started.elapsed() < Duration::from_secs(2));
    assert_fits(&lines, 60);
}
