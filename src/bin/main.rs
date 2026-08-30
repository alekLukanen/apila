use apila::core::tui_app::TUIApp;
use color_eyre::eyre::WrapErr;

use apila::core::tui;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let mut terminal = tui::init()?;
    let mut app = TUIApp::new();
    let result = app.run(&mut terminal).wrap_err("tui app failed");
    if let Err(err) = tui::restore() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
    result
}
