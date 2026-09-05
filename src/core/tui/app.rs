use std::path::{Path, PathBuf};

use crossterm::event::KeyModifiers;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, Cell, HighlightSpacing, Padding, Paragraph, Row, Table, TableState, Wrap},
    DefaultTerminal, Frame,
};
use thiserror::Error;

use clap::Parser;

use crate::core::config::config;
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

    #[error("config error: {0}")]
    ConfigError(#[from] config::ConfigError),
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

        // config.json ////
        let cfg = config::Config::load(&project_dir)?;

        let rt = runtime::Runtime::new(runtime::RuntimeConfig::new(project_dir.clone(), cfg))?;

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
        let table_area = block.inner(area);
        // the highlight symbol and the column spacing sit to the left of the cell
        let detail_width = table_area.width.saturating_sub(2) as usize;

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

                // TODO: stubbed until the agent reports its real token usage
                let context_used: u64 = 240_000;
                let context_limit: u64 = 1_000_000;

                Row::new(vec![Cell::from(vec![
                    Line::from(vec![
                        indicator,
                        Span::from(" "),
                        Span::from(config.name()).bold().cyan(),
                    ]),
                    Line::from(vec![Span::from("  "), config.model().full_slug().into()]).cyan(),
                    Self::context_usage_line(context_used, context_limit, detail_width),
                ])])
                .height(3)
            })
            .collect();

        // create and render table
        let table = Table::new(rows, [Constraint::Fill(1)])
            .column_spacing(1)
            // subtle slate wash so the cyan row text stays readable
            .row_highlight_style(Style::default().bg(Color::Rgb(45, 55, 72)))
            .highlight_symbol(Text::from(vec![">".bold().cyan().into()]))
            .highlight_spacing(HighlightSpacing::Always);

        frame.render_widget(block, area);
        frame.render_stateful_widget(table, table_area, &mut self.agents_table_state);

        Ok(())
    }

    /// Render a context window usage bar, e.g. "Context(24k/1M): ==>   2%".
    /// The bar grows with usage and fills whatever width is left over.
    fn context_usage_line(used: u64, limit: u64, width: usize) -> Line<'static> {
        let fraction = if limit == 0 {
            0.0
        } else {
            (used as f64 / limit as f64).clamp(0.0, 1.0)
        };
        let percent = (fraction * 100.0).round() as u64;

        let label = format!(
            "  Context({}/{}): ",
            Self::human_tokens(used),
            Self::human_tokens(limit)
        );
        let percent_text = format!(" {:>3}%", percent);

        let bar_width = width
            .saturating_sub(label.chars().count())
            .saturating_sub(percent_text.chars().count());
        // always show the ">" head for any non-zero usage
        let filled = (((bar_width as f64) * fraction).round() as usize)
            .max(if used > 0 { 1 } else { 0 })
            .min(bar_width);
        let bar = if filled == 0 {
            String::new()
        } else {
            format!("{}>", "=".repeat(filled.saturating_sub(1)))
        };

        Line::from(vec![
            Span::from(label).dark_gray(),
            Span::from(format!("{:<bar_width$}", bar)).cyan(),
            Span::from(percent_text).dark_gray(),
        ])
    }

    /// Format a token count compactly: 950 -> "950", 24_000 -> "24k", 1_000_000 -> "1M"
    fn human_tokens(tokens: u64) -> String {
        match tokens {
            t if t >= 1_000_000 => format!("{}M", t / 1_000_000),
            t if t >= 1_000 => format!("{}k", t / 1_000),
            t => t.to_string(),
        }
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
