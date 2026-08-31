use std::path::{Path, PathBuf};

use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use thiserror::Error;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// The project directory Apila uses to store data and create repositories in
    #[arg(short, long)]
    project_dir: String,
}

#[derive(Debug, Error)]
pub enum TUIAppError {
    #[error("ratatui draw error")]
    RatatuiErr(#[from] std::io::Error),

    #[error("invalid arg(s): {0}")]
    InalidArgs(String),
}

#[derive(Debug)]
pub struct TUIApp {
    args: Args,
    project_dir: PathBuf,

    exit: bool,
}

impl TUIApp {
    pub fn new(args: Args) -> Result<TUIApp, TUIAppError> {
        // project_dir ////
        let mut project_dir = PathBuf::new();
        project_dir.push(args.project_dir.clone());

        let project_path = Path::new(&project_dir);
        if !project_path.is_dir() {
            return Err(TUIAppError::InalidArgs(
                "project_dir isn't a directory".to_string(),
            ));
        }

        Ok(TUIApp {
            args,
            project_dir,
            exit: false,
        })
    }

    pub fn run(&mut self, term: &mut DefaultTerminal) -> Result<(), TUIAppError> {
        // draw initial frame
        term.draw(|frame| self.draw(frame))?;

        // setup...

        while !self.exit {
            term.draw(|frame| self.draw(frame))?;
            self.handle_key_events()?;
        }

        Ok(())
    }

    fn handle_key_events(&mut self) -> Result<(), TUIAppError> {
        match event::read()? {
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                ..
            }) => {
                self.exit = true;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn draw(&self, frame: &mut Frame) {
        let title = Line::from(vec![Span::styled(
            " Executing Queries ",
            ratatui::style::Style::default().bold().cyan(),
        )]);

        let instructions = Line::from(vec![" Quit ".into(), "<Q> ".blue().bold()]);
        let block = Block::default()
            .title(title.centered())
            .title_bottom(instructions.centered());

        let view_layout =
            Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]);
        let [left_area, right_area] = view_layout.areas(block.inner(frame.area()));

        let left_layout = Layout::vertical([Constraint::Length(10), Constraint::Fill(1)]);
        let [left_info_area, left_agents_area] = left_layout.areas(left_area);

        if let Err(err) = self.render_info_area(left_info_area, frame) {
            panic!("error: {}", err);
        }
        if let Err(err) = self.render_agents_area(left_agents_area, frame) {
            panic!("error: {}", err);
        }
        if let Err(err) = self.render_agent_detail_area(right_area, frame) {
            panic!("error: {}", err);
        }

        frame.render_widget(block, frame.area())
    }

    fn render_info_area(&self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" Status ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        let info_items = vec![Line::from(vec![
            "File: ".cyan(),
            format!("{}", self.project_dir.to_string_lossy().to_string()).blue(),
        ])];
        let info_line_count = info_items.len() as u16;

        let info_para = Paragraph::new(info_items).wrap(Wrap::default());

        let info_block_layout = Layout::vertical([
            Constraint::Length(info_line_count),
            //Constraint::Length(1),
            //Constraint::Fill(1),
        ]);
        let [info_para_area] = info_block_layout.areas(block.inner(area));

        frame.render_widget(info_para, info_para_area);
        frame.render_widget(block, area);
        Ok(())
    }

    fn render_agents_area(&self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" Agents ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        frame.render_widget(block, area);
        Ok(())
    }

    fn render_agent_detail_area(&self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" Agent Memory ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        frame.render_widget(block, area);

        Ok(())
    }
}
