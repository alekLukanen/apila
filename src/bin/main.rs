use apila::core::tui;
use clap::Parser;
use color_eyre::eyre::WrapErr;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let args = tui::app::Args::parse();

    // the app is built before taking over the terminal so a startup failure,
    // like a missing config.json, prints normally
    let mut app = tui::app::TUIApp::new(args)?;

    let mut terminal = tui::utils::init()?;
    let result = app.run(&mut terminal).wrap_err("tui app failed");
    if let Err(err) = tui::utils::restore() {
        eprintln!(
            "failed to restore terminal. Run `reset` or restart your terminal to recover: {err}"
        );
    }
    result
}
