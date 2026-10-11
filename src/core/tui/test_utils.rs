use ratatui::crossterm::Command;

use super::utils::{DisableWheelCapture, EnableWheelCapture};

fn ansi(command: impl Command) -> String {
    let mut out = String::new();
    command.write_ansi(&mut out).expect("write ansi");
    out
}

#[test]
fn wheel_capture_turns_on_click_reports_in_sgr_encoding() {
    assert_eq!(ansi(EnableWheelCapture), "\x1b[?1000h\x1b[?1006h");
    assert_eq!(ansi(DisableWheelCapture), "\x1b[?1006l\x1b[?1000l");
}

/// Motion tracking would wake the ui on every mouse move.
#[test]
fn wheel_capture_does_not_ask_for_motion() {
    let enable = ansi(EnableWheelCapture);

    assert!(!enable.contains("?1002h"), "{:?}", enable);
    assert!(!enable.contains("?1003h"), "{:?}", enable);
}
