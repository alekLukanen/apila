use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{backend::TestBackend, Terminal};

use crate::core::openrouter::types::Message;
use crate::core::test_support::{
    agent_dir, project_dir, project_with_server, wait_until, write_directive,
};

use super::app::{Args, TUIApp};

/// Presses a key, the way the event loop would.
fn press(app: &mut TUIApp, code: KeyCode) {
    app.handle_key(KeyEvent::from(code)).expect("handle key");
}

fn app(dir: &Path) -> TUIApp {
    TUIApp::new(Args::new(dir.to_string_lossy().to_string())).expect("build app")
}

/// Draws every screen the detail window can show, catching layout mistakes
/// that would otherwise only surface as a panic in a live terminal.
#[test]
fn every_detail_screen_renders() {
    let dir = project_dir("render");
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");

    // no directories in the project, so no agents
    let mut empty = app(&dir);
    term.draw(|frame| empty.draw(frame)).expect("draw empty");

    // a configured agent, drawn before it is started
    let (dir, _server) = project_with_server(
        "render-session",
        "Here is the plan.\n\nIt wraps across several lines so the transcript \
         has something long enough to fold.",
    );
    let worker = agent_dir(&dir, "worker", true);
    write_directive(&worker, "Review the parser.\n");

    let mut app = app(&dir);
    term.draw(|frame| app.draw(frame))
        .expect("draw configuring");

    // <Enter> runs the agent and steps into its chat
    press(&mut app, KeyCode::Enter);
    wait_until("the agent's reply on screen", || {
        term.draw(|frame| app.draw(frame)).expect("draw session");
        format!("{}", term.backend()).contains("Here is the plan.")
    });

    // a half typed reply is drawn in the composer
    for c in "a half typed reply".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    term.draw(|frame| app.draw(frame)).expect("draw composing");

    // an opening turn out of DIRECTIVE.md is not the user talking
    let screen = format!("{}", term.backend());
    assert!(screen.contains("directive"));
    assert!(!screen.contains("you"));
    assert!(screen.contains("a half typed reply"));
}

#[test]
fn agents_are_listed_without_the_user_doing_anything() {
    let dir = project_dir("auto-load");
    for name in ["builder", "reviewer"] {
        agent_dir(&dir, name, true);
    }

    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);
    term.draw(|frame| app.draw(frame)).expect("draw list");

    let screen = format!("{}", term.backend());
    // the agent's row in the list, found by the status dot in front of it
    let row = |name: &str| {
        let marker = format!("\u{25cf} {}", name);
        screen
            .lines()
            .find(|line| line.contains(&marker))
            .unwrap_or_else(|| panic!("`{}` is in the list", name))
            .to_string()
    };

    // both agents are listed, and the first is selected so the detail window
    // has something to show
    assert!(row("builder").contains(">\u{25cf} builder"));
    assert!(!row("reviewer").contains('>'));
}

// Wrapping //////////////////////////
//////////////////////////////////////

#[test]
fn wrapping_keeps_the_authors_own_line_breaks() {
    let lines = TUIApp::wrap("one two\nthree", 20);

    assert_eq!(lines, vec!["one two".to_string(), "three".to_string()]);
}

/// Every line that comes back has to fit, or a caller counting lines is
/// counting something other than what gets drawn.
#[test]
fn a_run_of_characters_with_nothing_to_wrap_on_is_still_broken_up() {
    let lines = TUIApp::wrap(&"a".repeat(25), 10);

    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.chars().count() <= 10), "{:?}", lines);
    assert_eq!(lines.concat(), "a".repeat(25));
}

#[test]
fn wrapping_never_returns_a_line_wider_than_it_was_asked_for() {
    let text = "short ".to_string() + &"z".repeat(40) + " end";

    for width in 1..12 {
        let lines = TUIApp::wrap(&text, width);
        assert!(
            lines.iter().all(|line| line.chars().count() <= width),
            "width {} produced {:?}",
            width,
            lines
        );
    }
}

/// A zero width is what an area too small to draw in reports, and it must not
/// leave the wrap looking for a fit that cannot exist.
#[test]
fn wrapping_to_no_width_at_all_still_finishes() {
    let lines = TUIApp::wrap("abc def", 0);

    assert!(lines.iter().all(|line| line.chars().count() <= 1), "{:?}", lines);
}

/// Characters wider than a byte must not be cut in the middle.
#[test]
fn wrapping_breaks_between_characters_rather_than_inside_one() {
    let lines = TUIApp::wrap(&"é".repeat(10), 4);

    assert_eq!(lines.concat(), "é".repeat(10));
    assert!(lines.iter().all(|line| line.chars().count() <= 4), "{:?}", lines);
}

/// The transcript shows the head of a long tool result and says how much more
/// there was. Before the wrap broke long runs up, output with no spaces in it
/// counted as one line and went past the cap whole.
#[test]
fn a_long_tool_result_is_capped_in_the_transcript() {
    let message = Message::tool("call_1", "x".repeat(4_000));

    let lines = TUIApp::message_lines(&message, 40, false);
    let rendered: Vec<String> = lines
        .iter()
        .map(|line| line.spans.iter().map(|span| span.content.to_string()).collect())
        .collect();

    // the label, the lines it kept, the note, and the blank line after
    assert!(rendered.len() < 20, "{} lines", rendered.len());
    assert!(
        rendered.iter().any(|line| line.contains("more lines")),
        "{:?}",
        rendered
    );
}

// Whitespace ////////////////////////
//////////////////////////////////////

/// The transcript carries file contents and command output. A diff or a listing
/// whose spacing has been tidied away is no longer the thing the agent saw.
#[test]
fn wrapping_keeps_a_listings_columns_exactly_as_they_were() {
    let listing = "-rw-r--r--  1 alek alek  4096 Sep  7 20:50 config.json";

    let lines = TUIApp::wrap(listing, 80);

    assert_eq!(lines, vec![listing.to_string()]);
}

#[test]
fn wrapping_keeps_leading_indentation() {
    // eight spaces, then enough to have to wrap at twelve columns
    let lines = TUIApp::wrap("        def f():", 12);

    assert!(
        lines[0].starts_with("        "),
        "indentation was lost: {:?}",
        lines
    );
    assert_eq!(lines.concat(), "        def f():");
}

#[test]
fn wrapping_keeps_blank_lines() {
    let lines = TUIApp::wrap("one\n\ntwo", 20);

    assert_eq!(
        lines,
        vec!["one".to_string(), String::new(), "two".to_string()]
    );
}

/// Nothing is dropped and nothing is added, so what is drawn is what the model
/// was shown.
#[test]
fn wrapping_gives_back_every_character_it_was_given() {
    let text = "aaa bbb ccc ddd";

    for width in 1..20 {
        let lines = TUIApp::wrap(text, width);
        assert_eq!(lines.concat(), text, "width {} changed the text", width);
        assert!(
            lines.iter().all(|line| line.chars().count() <= width),
            "width {} produced {:?}",
            width,
            lines
        );
    }
}

#[test]
fn a_line_is_broken_after_a_space_rather_than_inside_a_word() {
    let lines = TUIApp::wrap("hello world", 8);

    assert_eq!(lines, vec!["hello ".to_string(), "world".to_string()]);
}

/// A tab cannot be placed in a cell, so it has to become the spaces a terminal
/// would have drawn for it or a tab indented file renders ragged.
#[test]
fn tabs_are_expanded_to_the_columns_a_terminal_would_use() {
    assert_eq!(TUIApp::wrap("\tfoo", 40), vec!["        foo".to_string()]);
    // from column one, the next stop is still eight
    assert_eq!(TUIApp::wrap("a\tb", 40), vec!["a       b".to_string()]);
}

#[test]
fn a_carriage_return_at_the_end_of_a_line_is_not_drawn() {
    let lines = TUIApp::wrap("one\r\ntwo", 20);

    assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
}

/// Whitespace is kept, but a run of it with nothing to break on still has to be
/// cut, or a line cap counts something other than what is drawn.
#[test]
fn a_long_run_of_spaces_is_still_broken_up() {
    let lines = TUIApp::wrap(&" ".repeat(25), 10);

    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.chars().count() <= 10), "{:?}", lines);
    assert_eq!(lines.concat(), " ".repeat(25));
}

