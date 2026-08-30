use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::Block,
    DefaultTerminal, Frame,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TUIAppError {
    #[error("ratatui draw error")]
    RatatuiErr(#[from] std::io::Error),
}

#[derive(Debug)]
pub struct TUIApp {
    exit: bool,
}

impl TUIApp {
    pub fn new() -> TUIApp {
        TUIApp { exit: false }
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

        if let Err(err) = self.render_status_area(left_info_area, frame) {
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

    fn render_status_area(&self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" Status ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

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
