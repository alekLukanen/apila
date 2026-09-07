use std::{env, fs, path::PathBuf};

use ratatui::{backend::TestBackend, Terminal};

use super::app::{Args, TUIApp};

fn project_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("apila-test-app-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create project dir");
    fs::write(
        dir.join("config.json"),
        r#"{"openrouter_api_key": "sk-or-test"}"#,
    )
    .expect("write config");
    fs::write(dir.join("SYSTEM.md"), "guidelines").expect("write system file");
    dir
}

fn app(dir: &PathBuf) -> TUIApp {
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

    let agent_dir = dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join("AGENTS.md"), "purpose").expect("write agents file");
    fs::write(
        agent_dir.join("config.json"),
        r#"{"model": "openai/gpt-4o"}"#,
    )
    .expect("write agent config");

    // the agent is loaded from the directory, already selected and configured
    let mut app = app(&dir);
    term.draw(|frame| app.draw(frame))
        .expect("draw configuring");

    app.seed_session_for_test(
        "Review the parser.",
        "Here is the plan.\n\nIt wraps across several lines so the transcript \
         has something long enough to fold.",
        true,
    );
    term.draw(|frame| app.draw(frame)).expect("draw session");

    // an opening turn out of DIRECTIVE.md is not the user talking
    let screen = format!("{}", term.backend());
    assert!(screen.contains("directive"));
    assert!(!screen.contains("you"));
}

#[test]
fn agents_are_listed_without_the_user_doing_anything() {
    let dir = project_dir("auto-load");
    for name in ["builder", "reviewer"] {
        let agent_dir = dir.join(name);
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(agent_dir.join("AGENTS.md"), "purpose").expect("write agents file");
        fs::write(
            agent_dir.join("config.json"),
            r#"{"model": "openai/gpt-4o"}"#,
        )
        .expect("write agent config");
    }

    let app = app(&dir);

    assert_eq!(app.agent_names_for_test(), vec!["builder", "reviewer"]);
    // the first agent is selected, so the detail window has something to show
    assert_eq!(
        app.selected_agent_name_for_test().as_deref(),
        Some("builder")
    );
}
