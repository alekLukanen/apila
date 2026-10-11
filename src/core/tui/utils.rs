use std::fmt;
use std::io::{self, stdout, Stdout};

use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{DisableBracketedPaste, EnableBracketedPaste},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        Command,
    },
    Terminal,
};

/// Reports the mouse wheel and clicks but not motion, so moving the mouse
/// doesn't wake the ui. SGR encoding keeps large coordinates intact.
#[derive(Debug, Clone, Copy)]
pub struct EnableWheelCapture;

impl Command for EnableWheelCapture {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1000h\x1b[?1006h")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DisableWheelCapture;

impl Command for DisableWheelCapture {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        f.write_str("\x1b[?1006l\x1b[?1000l")
    }
}

/// Initialize the terminal. A failure part way through puts back whatever was
/// already changed.
pub fn init() -> io::Result<ratatui::Terminal<CrosstermBackend<Stdout>>> {
    if let Err(err) = enter() {
        let _ = restore();
        return Err(err);
    }
    set_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout())).inspect_err(|_| {
        let _ = restore();
    })
}

fn enter() -> io::Result<()> {
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableWheelCapture,
        EnableBracketedPaste
    )?;
    enable_raw_mode()
}

fn set_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // a tool's panic is caught and reported to the model, so the program is
        // not going anywhere and the screen has to stay as it is. printing over
        // the alternate screen would be as bad as leaving it
        if crate::core::tools::tool::panics_are_being_caught() {
            return;
        }
        let _ = restore(); // ignore any errors as we are already failing
        hook(panic_info);
    }));
}

/// Restore the terminal to its original state. Every step is tried even if an
/// earlier one fails, and the first error is returned.
pub fn restore() -> io::Result<()> {
    let steps = [
        execute!(stdout(), DisableBracketedPaste),
        execute!(stdout(), DisableWheelCapture),
        execute!(stdout(), LeaveAlternateScreen),
        disable_raw_mode(),
    ];
    steps.into_iter().collect()
}
