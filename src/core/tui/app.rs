use std::path::{Path, PathBuf};

use crossterm::event::KeyModifiers;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Cell, Padding, Paragraph, Row, Table, TableState, Wrap},
    DefaultTerminal, Frame,
};
use thiserror::Error;

use clap::Parser;

use crate::core::runtime::{agent, runtime};

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

    #[error("runtime error: {0}")]
    RuntimeError(#[from] runtime::RuntimeError),
}

#[derive(Debug)]
pub struct TUIApp {
    args: Args,
    rt: runtime::Runtime,

    agents_table_state: TableState,

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

        let rt = runtime::Runtime::new(runtime::RuntimeConfig::new(project_dir.clone()));

        Ok(TUIApp {
            args,
            rt,
            agents_table_state: TableState::default(),
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
            Event::Key(KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => {
                self.rt.create_agent(agent::AgentConfig::empty())?;
                if self.agents_table_state.selected().is_none() {
                    self.agents_table_state.select(Some(0));
                }
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down | KeyCode::Char('j'),
                ..
            }) => {
                self.select_agent_offset(1);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Up | KeyCode::Char('k'),
                ..
            }) => {
                self.select_agent_offset(-1);
            }
            _ => {}
        }
        Ok(())
    }

    /// Move the agent table selection by `offset` rows, clamped to the
    /// number of agents currently in the runtime.
    fn select_agent_offset(&mut self, offset: isize) {
        let agent_count = self.rt.list_agents().len();
        if agent_count == 0 {
            self.agents_table_state.select(None);
            return;
        }
        let selected = match self.agents_table_state.selected() {
            Some(selected) => (selected as isize + offset).clamp(0, agent_count as isize - 1),
            None => 0,
        };
        self.agents_table_state.select(Some(selected as usize));
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let instruction_pairs = vec![
            ("Quit", " <Q>"),
            ("Create Agent", " <Ctrl+N>"),
            ("Stop Agents", " <Ctrl+H>"),
        ];
        let num_instruction_pairs = instruction_pairs.len();
        let instruction_items: Vec<Span> = instruction_pairs
            .iter()
            .enumerate()
            .flat_map(|(idx, (title, code))| {
                let mut spans = vec![Span::from(*title), code.blue().bold()];
                if idx + 1 < num_instruction_pairs {
                    spans.push(Span::from(" | "))
                }
                spans
            })
            .collect();

        let instructions = Line::from(instruction_items);
        let block = Block::default()
            .padding(Padding::top(1))
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

    fn render_info_area(&mut self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" ✨Status ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        let info_items = vec![Line::from(vec![
            "Project Dir: ".cyan(),
            format!(
                "{}",
                self.rt.config().project_dir().to_string_lossy().to_string()
            )
            .blue(),
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

    fn render_agents_area(&mut self, area: Rect, frame: &mut Frame) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" 🤖Agents ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        let agent_definitions = self.rt.list_agents();

        // create table rows
        let rows: Vec<Row> = agent_definitions
            .iter()
            .map(|agent_definition| {
                let config = agent_definition.config();
                // TODO: use the agent's real state once it's tracked; active for now
                let active = true;
                let indicator = if active {
                    "●".green()
                } else {
                    "●".dark_gray()
                };
                Row::new(vec![
                    Cell::from(Line::from(indicator)),
                    Cell::from(Line::from(vec![
                        Span::from(config.name()).bold(),
                        Span::from(" "),
                        config.model().full_slug().dark_gray(),
                    ])),
                ])
            })
            .collect();

        // create and render table
        let table_area = block.inner(area);
        let table = Table::new(rows, [Constraint::Length(1), Constraint::Fill(1)])
            .column_spacing(1)
            .row_highlight_style(Style::default().on_dark_gray());

        frame.render_widget(block, area);
        frame.render_stateful_widget(table, table_area, &mut self.agents_table_state);
        Ok(())
    }

    fn render_agent_detail_area(
        &mut self,
        area: Rect,
        frame: &mut Frame,
    ) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" 🤖Agent Memory ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());

        frame.render_widget(block, area);

        Ok(())
    }
}
