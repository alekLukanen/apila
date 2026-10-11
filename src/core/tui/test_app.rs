use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, layout::Position, Terminal};

use crate::core::memory::recorder::{JobState, MemoryJob, MemoryView};
use crate::core::memory::store::SkillsSaved;
use crate::core::openrouter::types::{Message, Spend};
use std::time::Duration;

use crate::core::test_support::{
    agent_dir, project_dir, project_with_delayed_server, project_with_routes, project_with_server,
    wait_until, write_agent_config_json, write_agent_config_with_skills, write_directive, Route,
    StubOpenRouter, StubReply, TEST_MODEL, TEST_WRITER_MODEL,
};

use super::app::{Args, TUIApp};

/// Presses a key, the way the event loop would.
fn press(app: &mut TUIApp, code: KeyCode) {
    app.handle_key(KeyEvent::from(code)).expect("handle key");
}

fn press_with(app: &mut TUIApp, code: KeyCode, modifiers: KeyModifiers) {
    app.handle_key(KeyEvent::new(code, modifiers))
        .expect("handle key");
}

fn mouse(app: &mut TUIApp, kind: MouseEventKind, at: Position) {
    app.handle_event(Event::Mouse(MouseEvent {
        kind,
        column: at.x,
        row: at.y,
        modifiers: KeyModifiers::NONE,
    }))
    .expect("handle mouse");
}

fn paste(app: &mut TUIApp, text: &str) {
    app.handle_event(Event::Paste(text.to_string()))
        .expect("handle paste");
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

/// Draws a project whose one agent is configured by `agent_config`, returning
/// the agent's model line in the list, the detail pane's `Model:` line, and
/// the whole screen.
fn model_lines(name: &str, agent_config: &str) -> (String, String, String) {
    let dir = project_dir(name);
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_json(&builder, agent_config);

    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);
    term.draw(|frame| app.draw(frame)).expect("draw list");

    let screen = format!("{}", term.backend());
    let lines: Vec<&str> = screen.lines().collect();
    // the model sits on the line under the agent's row in the list
    let row = lines
        .iter()
        .position(|line| line.contains("\u{25cf} builder"))
        .expect("`builder` is in the list");
    let list_line = lines
        .get(row + 1)
        .expect("a line under the row")
        .to_string();
    let detail_line = lines
        .iter()
        .find(|line| line.contains("Model:"))
        .expect("the detail pane names the model")
        .to_string();
    (list_line, detail_line, screen)
}

#[test]
fn the_reasoning_effort_is_shown_after_the_model() {
    let (list_line, detail_line, _) = model_lines(
        "effort-shown",
        r#"{"model": "openai/gpt-4o", "agent_max_iterations": 5, "reasoning": {"effort": "high"}}"#,
    );

    assert!(list_line.contains("openai/gpt-4o (high)"), "{}", list_line);
    assert!(
        detail_line.contains("openai/gpt-4o (high)"),
        "{}",
        detail_line
    );
}

#[test]
fn without_a_reasoning_effort_only_the_model_is_shown() {
    let (list_line, detail_line, screen) = model_lines(
        "effort-unset",
        r#"{"model": "openai/gpt-4o", "agent_max_iterations": 5}"#,
    );

    assert!(list_line.contains("openai/gpt-4o"), "{}", list_line);
    assert!(detail_line.contains("openai/gpt-4o"), "{}", detail_line);
    assert!(!screen.contains("openai/gpt-4o ("), "{}", screen);
}

// Clearing //////////////////////////
//////////////////////////////////////

fn type_text(app: &mut TUIApp, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn type_line(app: &mut TUIApp, text: &str) {
    type_text(app, text);
    press(app, KeyCode::Enter);
}

fn screen(term: &mut Terminal<TestBackend>, app: &mut TUIApp) -> String {
    term.draw(|frame| app.draw(frame)).expect("draw");
    format!("{}", term.backend())
}

#[test]
fn typing_clear_starts_a_new_session_rather_than_sending_it() {
    let (dir, server) = project_with_server("app-clear", "the answer");
    agent_dir(&dir, "worker", true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    // no directive, so <Enter> opens the chat and waits for the user
    press(&mut app, KeyCode::Enter);
    type_line(&mut app, "the question");
    // the reply is on screen a moment before the turn is over
    wait_until("the reply on screen and the agent idle", || {
        let shown = screen(&mut term, &mut app);
        shown.contains("the answer") && shown.contains("State:     idle")
    });

    type_line(&mut app, "/clear");
    assert!(screen(&mut term, &mut app).contains("session cleared"));
    wait_until("the old conversation to go", || {
        !screen(&mut term, &mut app).contains("the answer")
    });
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_clear_typed_mid_turn_is_shown_queued_with_what_follows_it() {
    let (dir, _server) =
        project_with_delayed_server("app-clear-queued", "the answer", Duration::from_millis(300));
    agent_dir(&dir, "worker", true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    press(&mut app, KeyCode::Enter);
    type_line(&mut app, "the question");
    type_line(&mut app, "/clear");
    let shown = screen(&mut term, &mut app);
    assert!(
        shown.contains("clear queued — runs when this turn ends"),
        "{shown}"
    );

    type_line(&mut app, "a fresh start");
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains("next session"), "{shown}");
    assert!(shown.contains("a fresh start"), "{shown}");

    // once the turn ends the new session opens on what was sent after the clear
    wait_until("the new session", || {
        let shown = screen(&mut term, &mut app);
        !shown.contains("the question")
            && shown.contains("a fresh start")
            && shown.contains("the answer")
    });
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
    assert!(
        lines.iter().all(|line| line.chars().count() <= 10),
        "{:?}",
        lines
    );
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

    assert!(
        lines.iter().all(|line| line.chars().count() <= 1),
        "{:?}",
        lines
    );
}

/// Characters wider than a byte must not be cut in the middle.
#[test]
fn wrapping_breaks_between_characters_rather_than_inside_one() {
    let lines = TUIApp::wrap(&"é".repeat(10), 4);

    assert_eq!(lines.concat(), "é".repeat(10));
    assert!(
        lines.iter().all(|line| line.chars().count() <= 4),
        "{:?}",
        lines
    );
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
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.to_string())
                .collect()
        })
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
    assert!(
        lines.iter().all(|line| line.chars().count() <= 10),
        "{:?}",
        lines
    );
    assert_eq!(lines.concat(), " ".repeat(25));
}

// Scrolling /////////////////////////
//////////////////////////////////////

/// Where `text` starts on the last drawn screen, in cells.
fn find(term: &Terminal<TestBackend>, text: &str) -> Option<Position> {
    let buffer = term.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        let mut columns = Vec::new();
        for x in 0..buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            columns.extend(std::iter::repeat_n(x, symbol.len()));
            line.push_str(symbol);
        }
        if let Some(byte) = line.find(text) {
            return Some(Position::new(columns[byte], y));
        }
    }
    None
}

const LAST_LINE: &str = "reply line 60";
const MORE_BELOW: &str = "PgDn or scroll down";

/// A reply far longer than the transcript is tall.
fn long_reply() -> String {
    (1..=60)
        .map(|n| format!("reply line {}", n))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A project with agents named `names`, the first of which has been sent a
/// message and has its long reply on screen.
fn chatting(test: &str, names: &[&str]) -> (Terminal<TestBackend>, TUIApp, StubOpenRouter) {
    let (dir, server) = project_with_server(test, &long_reply());
    for name in names {
        agent_dir(&dir, name, true);
    }
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    press(&mut app, KeyCode::Enter);
    type_line(&mut app, "the question");
    wait_until("the long reply on screen and the agent idle", || {
        let shown = screen(&mut term, &mut app);
        shown.contains(LAST_LINE) && shown.contains("State:     idle")
    });
    (term, app, server)
}

#[test]
fn the_wheel_scrolls_the_transcript_and_back_down_follows_again() {
    let (mut term, mut app, _server) = chatting("app-wheel", &["worker"]);
    let over_transcript = find(&term, "reply line 50").expect("transcript on screen");

    mouse(&mut app, MouseEventKind::ScrollUp, over_transcript);
    let shown = screen(&mut term, &mut app);
    assert!(!shown.contains(LAST_LINE), "{shown}");
    assert!(shown.contains(MORE_BELOW), "{shown}");

    mouse(&mut app, MouseEventKind::ScrollDown, over_transcript);
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains(LAST_LINE), "{shown}");
    assert!(!shown.contains(MORE_BELOW), "{shown}");
}

#[test]
fn the_wheel_over_the_agent_list_does_not_scroll_the_transcript() {
    let (mut term, mut app, _server) = chatting("app-wheel-list", &["worker"]);
    let over_list = find(&term, "\u{25cf} worker").expect("agent in the list");

    mouse(&mut app, MouseEventKind::ScrollUp, over_list);
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains(LAST_LINE), "{shown}");
    assert!(!shown.contains(MORE_BELOW), "{shown}");
}

#[test]
fn page_keys_scroll_the_transcript_from_either_window() {
    let (mut term, mut app, _server) = chatting("app-page", &["worker"]);

    for leave_chat in [false, true] {
        if leave_chat {
            press(&mut app, KeyCode::Esc);
        }
        press(&mut app, KeyCode::PageUp);
        let shown = screen(&mut term, &mut app);
        assert!(!shown.contains(LAST_LINE), "{shown}");
        assert!(shown.contains(MORE_BELOW), "{shown}");

        press(&mut app, KeyCode::PageDown);
        let shown = screen(&mut term, &mut app);
        assert!(shown.contains(LAST_LINE), "{shown}");
        assert!(!shown.contains(MORE_BELOW), "{shown}");
    }
}

/// The last chat drawn belongs to the agent that was selected then, and paging
/// it from another agent would scroll a transcript nobody can see.
#[test]
fn paging_after_selecting_another_agent_does_nothing_until_it_is_drawn() {
    let (mut term, mut app, _server) = chatting("app-page-other", &["alpha", "beta"]);

    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::PageUp);
    press(&mut app, KeyCode::Up);

    let shown = screen(&mut term, &mut app);
    assert!(shown.contains(LAST_LINE), "{shown}");
    assert!(!shown.contains(MORE_BELOW), "{shown}");
}

#[test]
fn a_held_position_left_past_the_end_follows_again() {
    let (mut term, mut app, _server) = chatting("app-page-shrink", &["worker"]);
    press(&mut app, KeyCode::PageUp);
    assert!(screen(&mut term, &mut app).contains(MORE_BELOW));

    // tall enough for the whole transcript, so nothing is left to scroll
    term.backend_mut().resize(120, 200);
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains(LAST_LINE), "{shown}");
    assert!(!shown.contains(MORE_BELOW), "{shown}");

    term.backend_mut().resize(120, 40);
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains(LAST_LINE), "{shown}");
    assert!(!shown.contains(MORE_BELOW), "{shown}");
}

// Composing /////////////////////////
//////////////////////////////////////

/// A chat opened on an agent with no directive, so nothing is sent until the
/// test sends it.
fn composing(test: &str) -> (Terminal<TestBackend>, TUIApp, StubOpenRouter) {
    let (dir, server) = project_with_server(test, "the answer");
    agent_dir(&dir, "worker", true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);
    press(&mut app, KeyCode::Enter);
    screen(&mut term, &mut app);
    (term, app, server)
}

#[test]
fn a_paste_is_typed_into_the_chat_rather_than_sent() {
    let (mut term, mut app, server) = composing("app-paste");

    paste(&mut app, "pasted first\npasted second");
    let shown = screen(&mut term, &mut app);
    assert!(shown.contains("pasted first"), "{shown}");
    assert!(shown.contains("pasted second"), "{shown}");
    assert!(server.requests().is_empty());
}

/// Outside the chat each pasted character would otherwise run as a command.
#[test]
fn a_paste_outside_the_chat_is_ignored() {
    let (mut term, mut app, _server) = composing("app-paste-agents");

    press(&mut app, KeyCode::Esc);
    paste(&mut app, "pasted text");
    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut term, &mut app);
    assert!(!shown.contains("pasted text"), "{shown}");
}

#[test]
fn the_mouse_and_a_paste_leave_the_status_message_alone() {
    let (mut term, mut app, _server) = composing("app-paste-status");
    type_line(&mut app, "/clear");
    assert!(screen(&mut term, &mut app).contains("session cleared"));

    mouse(&mut app, MouseEventKind::ScrollUp, Position::new(100, 20));
    mouse(&mut app, MouseEventKind::Moved, Position::new(100, 20));
    paste(&mut app, "more");
    assert!(screen(&mut term, &mut app).contains("session cleared"));
}

#[test]
fn alt_enter_and_ctrl_j_start_a_new_line_without_sending() {
    let (mut term, mut app, server) = composing("app-newline");

    type_text(&mut app, "first row");
    press_with(&mut app, KeyCode::Enter, KeyModifiers::ALT);
    type_text(&mut app, "second row");
    press_with(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
    type_text(&mut app, "third row");

    screen(&mut term, &mut app);
    let first = find(&term, "first row").expect("first row");
    let second = find(&term, "second row").expect("second row");
    let third = find(&term, "third row").expect("third row");
    assert_eq!((second.x, second.y), (first.x, first.y + 1));
    assert_eq!((third.x, third.y), (first.x, first.y + 2));
    assert!(server.requests().is_empty());
}

#[test]
fn shift_types_a_capital_but_control_and_alt_type_nothing() {
    let (mut term, mut app, _server) = composing("app-modifiers");

    press(&mut app, KeyCode::Char('x'));
    press_with(&mut app, KeyCode::Char('Y'), KeyModifiers::SHIFT);
    press_with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    press_with(&mut app, KeyCode::Char('z'), KeyModifiers::ALT);
    press(&mut app, KeyCode::Char('w'));

    let shown = screen(&mut term, &mut app);
    assert!(shown.contains("xYw"), "{shown}");
}

#[test]
fn typing_after_moving_left_inserts_mid_message() {
    let (mut term, mut app, _server) = composing("app-insert");

    type_text(&mut app, "mid ac");
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Char('b'));

    assert!(screen(&mut term, &mut app).contains("mid abc"));
}

/// The box grows a row at a time up to its cap, then scrolls what was typed
/// and keeps the cursor on the row being written.
#[test]
fn the_input_grows_to_three_rows_and_then_scrolls() {
    let (mut term, mut app, _server) = composing("app-grow");
    let bottom_left = "\u{2570}";

    type_text(&mut app, "row1");
    screen(&mut term, &mut app);
    let title = find(&term, " message ").expect("the input box");
    let first = find(&term, "row1").expect("row1");
    let left = first.x - 1;
    assert_eq!(first.y, title.y + 1);
    assert_eq!(
        term.backend().buffer()[(left, title.y + 2)].symbol(),
        bottom_left
    );

    for row in ["row2", "row3", "row4"] {
        press_with(&mut app, KeyCode::Enter, KeyModifiers::ALT);
        type_text(&mut app, row);
    }
    let shown = screen(&mut term, &mut app);
    let title = find(&term, " message ").expect("the input box");
    assert!(!shown.contains("row1"), "{shown}");
    assert_eq!(find(&term, "row2").expect("row2").y, title.y + 1);
    let last = find(&term, "row4").expect("row4");
    assert_eq!(last.y, title.y + 3);
    assert_eq!(
        term.backend().buffer()[(left, title.y + 4)].symbol(),
        bottom_left
    );

    // the cursor sits just after what was typed, inside the box
    let cursor = term.get_cursor_position().expect("cursor");
    assert_eq!(cursor, Position::new(last.x + 4, last.y));
}

// Memory and costs //////////////////
//////////////////////////////////////

/// The text of the lines `memory_lines` draws.
fn memory_text(view: &MemoryView, skills_on: bool) -> Vec<String> {
    TUIApp::memory_lines(view, skills_on)
        .iter()
        .map(|line| line.to_string())
        .collect()
}

fn job(state: JobState) -> MemoryJob {
    MemoryJob {
        session_id: 7,
        state,
        spend: Spend::new(14_200, 3_100_000, 0),
    }
}

fn done(added: usize, updated: usize) -> MemoryJob {
    job(JobState::Done {
        skills: SkillsSaved { added, updated },
    })
}

#[test]
fn the_memory_line_counts_skills_and_jobs() {
    let mut view = MemoryView {
        skills: Some(12),
        pending: Some(0),
        ..MemoryView::default()
    };
    assert_eq!(
        memory_text(&view, true),
        vec![
            "Memory:    12 skills · no jobs running",
            "Last job:  none yet"
        ]
    );

    view.last_job = Some(job(JobState::Running));
    assert_eq!(
        memory_text(&view, true)[0],
        "Memory:    12 skills · 1 job running"
    );
    assert_eq!(
        memory_text(&view, true)[1],
        "Last job:  session #7 · analysing · 14k tok · $0.0031"
    );

    view.pending = Some(1);
    assert_eq!(
        memory_text(&view, true)[0],
        "Memory:    12 skills · 2 jobs running"
    );

    view.skills = Some(1);
    view.last_job = Some(done(2, 0));
    assert_eq!(
        memory_text(&view, true),
        vec![
            "Memory:    1 skill · 1 job running",
            "Last job:  session #7 · added 2 skills · 14k tok · $0.0031"
        ]
    );
}

#[test]
fn a_done_job_says_which_skills_it_added_and_which_it_updated() {
    let cases = [
        (1, 0, "added 1 skill"),
        (2, 0, "added 2 skills"),
        (0, 1, "updated 1 skill"),
        (0, 2, "updated 2 skills"),
        (1, 1, "added 1 · updated 1"),
        (0, 0, "saved 0 skills"),
    ];
    for (added, updated, wording) in cases {
        let view = MemoryView {
            last_job: Some(done(added, updated)),
            ..MemoryView::default()
        };
        assert_eq!(
            memory_text(&view, true)[1],
            format!("Last job:  session #7 · {} · 14k tok · $0.0031", wording)
        );
    }
}

#[test]
fn a_failed_job_says_the_rest_wait_for_the_next_clear() {
    let view = MemoryView {
        skills: Some(3),
        pending: Some(1),
        last_job: Some(job(JobState::Failed("the model\nwent away".into()))),
        ..MemoryView::default()
    };

    assert_eq!(
        memory_text(&view, true),
        vec![
            "Memory:    3 skills · 1 job pending (retries on next /clear)",
            "Last job:  session #7 · failed: the model went away · 14k tok · $0.0031"
        ]
    );
}

#[test]
fn a_clear_after_a_failed_job_shows_the_retry_as_running() {
    let view = MemoryView {
        skills: Some(3),
        pending: Some(2),
        last_job: Some(job(JobState::Failed("boom".into()))),
        queued: true,
        ..MemoryView::default()
    };

    assert_eq!(
        memory_text(&view, true)[0],
        "Memory:    3 skills · 2 jobs running"
    );
}

#[test]
fn unread_skills_an_interrupted_job_and_an_error_are_shown() {
    let view = MemoryView {
        message: Some("recording the session failed: disk full".into()),
        last_job: Some(job(JobState::Interrupted)),
        ..MemoryView::default()
    };

    assert_eq!(
        memory_text(&view, true),
        vec![
            "Memory:    … · no jobs running",
            "Last job:  session #7 · interrupted · 14k tok · $0.0031",
            "Status:    recording the session failed: disk full"
        ]
    );
}

#[test]
fn with_skills_off_the_memory_line_says_so() {
    let mut view = MemoryView::default();
    assert_eq!(memory_text(&view, false), vec!["Memory:    skills off"]);

    view.last_job = Some(done(0, 0));
    assert_eq!(
        memory_text(&view, false),
        vec![
            "Memory:    skills off",
            "Last job:  session #7 · saved 0 skills · 14k tok · $0.0031"
        ]
    );
}

fn nanos(dollars: f64) -> Spend {
    Spend::new(0, (dollars * 1e9).round() as i64, 0)
}

#[test]
fn a_cost_in_the_detail_pane_has_more_precision_the_smaller_it_is() {
    let cases = [
        (0.0, "$0"),
        (0.00001, "<$0.0001"),
        (0.0042, "$0.0042"),
        (0.009996, "$0.01"),
        (1.234, "$1.23"),
        (99.996, "$100"),
        (123.4, "$123"),
    ];
    for (dollars, expected) in cases {
        assert_eq!(TUIApp::human_cost(&nanos(dollars)), expected, "{}", dollars);
    }

    let mut partial = nanos(0.0042);
    partial.unpriced = 1;
    assert_eq!(TUIApp::human_cost(&partial), "~$0.0042");
    assert_eq!(TUIApp::human_cost(&Spend::new(10, 0, 2)), "—");
}

#[test]
fn a_cost_on_an_agent_row_stays_within_five_characters() {
    let cases = [
        (0.0, "$0"),
        (0.004, "<$.01"),
        (0.04, "$0.04"),
        (0.996, "$1.00"),
        (9.999, "$10.0"),
        (12.34, "$12.3"),
        (99.96, "$100"),
        (1234.4, "$1234"),
        (123_456.0, "$123k"),
    ];
    for (dollars, expected) in cases {
        assert_eq!(
            TUIApp::compact_cost(&nanos(dollars)),
            expected,
            "{}",
            dollars
        );
    }

    let partial = |dollars: f64| {
        let mut spend = nanos(dollars);
        spend.unpriced = 1;
        TUIApp::compact_cost(&spend)
    };
    assert_eq!(partial(0.004), "~$0");
    assert_eq!(partial(0.04), "~$.04");
    assert_eq!(partial(1.23), "~$1.2");
    assert_eq!(partial(12.3), "~$12");
    assert_eq!(partial(123.0), "~$123");
    assert_eq!(TUIApp::compact_cost(&Spend::new(10, 0, 1)), "—");
    assert_eq!(
        TUIApp::compact_cost(&Spend::new(0, i64::MAX, 0))
            .chars()
            .count(),
        3
    );
}

#[test]
fn the_cost_line_fits_the_narrowest_list_in_the_worst_case() {
    let mut worst = nanos(123.0);
    worst.unpriced = 1;

    let line = TUIApp::cost_line(worst, Some(worst), Some(worst), 32).to_string();
    assert_eq!(line, " sess ~$123 all ~$123 mem ~$123");
    assert!(line.chars().count() <= 32);

    let unread = TUIApp::cost_line(nanos(0.04), None, None, 32).to_string();
    assert_eq!(unread, " sess $0.04 all … mem …");

    let narrow = TUIApp::cost_line(worst, Some(worst), Some(worst), 20).to_string();
    assert_eq!(narrow.chars().count(), 20);
    assert!(narrow.ends_with('…'), "{narrow}");
}

/// The lines of the agent's row in the list, from its name down.
fn row_lines(screen: &str, name: &str, count: usize) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let row = lines
        .iter()
        .position(|line| line.contains(&format!("\u{25cf} {}", name)))
        .unwrap_or_else(|| panic!("`{}` is in the list", name));
    lines[row..row + count]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

#[test]
fn each_agent_row_ends_with_its_costs() {
    let dir = project_dir("app-row-costs");
    for name in ["builder", "reviewer"] {
        agent_dir(&dir, name, true);
    }
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    wait_until("both rows to show their costs", || {
        let shown = screen(&mut term, &mut app);
        ["builder", "reviewer"]
            .iter()
            .all(|name| row_lines(&shown, name, 4)[3].contains("sess $0 all $0 mem $0"))
    });
    let shown = screen(&mut term, &mut app);
    let builder = row_lines(&shown, "builder", 5);
    assert!(builder[1].contains(TEST_MODEL), "{}", builder[1]);
    assert!(builder[2].contains("Context("), "{}", builder[2]);
    // four lines to a row, so the next agent starts right after
    assert!(builder[4].contains("\u{25cf} reviewer"), "{}", builder[4]);
}

#[test]
fn an_analysed_clear_shows_the_memory_lines_under_the_state() {
    let (dir, _server) = project_with_routes(
        "app-memory",
        vec![
            (
                Route::Chat(TEST_MODEL.into()),
                vec![StubReply::text("on it")],
            ),
            (
                Route::Chat(TEST_WRITER_MODEL.into()),
                vec![
                    StubReply::tool_call(
                        "w1",
                        "save_skill",
                        serde_json::json!({
                            "name": "fetch-webpage-into-sqlite",
                            "when_to_use": "The user asks to save a web page.",
                            "body": "## Steps\n1. fetch",
                        }),
                    ),
                    StubReply::tool_call("w2", "end_turn", serde_json::json!({})),
                ],
            ),
            (
                Route::Embeddings,
                vec![StubReply::embedding_with_cost(vec![1.0, 0.0], 5, 0.0001)],
            ),
        ],
    );
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_with_skills(
        &builder,
        TEST_MODEL,
        TEST_WRITER_MODEL,
        serde_json::json!({"enabled": []}),
    );
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    press(&mut app, KeyCode::Enter);
    type_line(&mut app, "save hacker news");
    wait_until("the reply", || {
        screen(&mut term, &mut app).contains("State:     idle")
    });
    type_line(&mut app, "/clear");

    // two writer replies and one embedding, each a tenth of a cent
    let expected = "Last job:  session #1 · added 1 skill · 41 tok · $0.0003";
    // the writer may finish before the agent's thread has reset the session
    wait_until("the job's result and the session's reset", || {
        let shown = screen(&mut term, &mut app);
        shown.contains(expected) && row_lines(&shown, "builder", 4)[3].contains("sess $0 ")
    });
    let shown = screen(&mut term, &mut app);
    let lines: Vec<&str> = shown.lines().collect();
    let state = lines
        .iter()
        .position(|line| line.contains("State:"))
        .expect("the state line");
    assert!(
        lines[state + 1].contains("Memory:    1 skill · no jobs running"),
        "{shown}"
    );
    assert!(lines[state + 2].contains(expected), "{shown}");
    // the chat's cost moved from the session into the total
    let row = row_lines(&shown, "builder", 4);
    assert!(row[3].contains("sess $0 all <$.01 mem <$.01"), "{}", row[3]);
}

#[test]
fn an_agent_without_skills_says_skills_are_off() {
    let dir = project_dir("app-skills-off");
    agent_dir(&dir, "builder", true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    let mut app = app(&dir);

    let shown = screen(&mut term, &mut app);
    let lines: Vec<&str> = shown.lines().collect();
    let state = lines
        .iter()
        .position(|line| line.contains("State:"))
        .expect("the state line");
    assert!(
        lines[state + 1].contains("Memory:    skills off"),
        "{shown}"
    );
    assert!(!shown.contains("Last job:"), "{shown}");
}
