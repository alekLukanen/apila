use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{backend::TestBackend, Terminal};

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
