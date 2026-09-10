use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::KeyModifiers;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
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
use crate::core::openrouter::types::Message;
use crate::core::runtime::agent::{AgentDefinition, AgentState};
use crate::core::runtime::{agent_config, runtime};

/// How long the ui waits for a key before redrawing. Agents answer on
/// background threads, so the screen has to refresh without any input.
const TICK: Duration = Duration::from_millis(100);

/// TODO: read the real context window off the model once it is available.
const CONTEXT_LIMIT_TOKENS: u64 = 1_000_000;

/// How much of a tool's output the transcript shows before it says how much
/// more there was. The model still gets all of it.
const TOOL_OUTPUT_LINES: usize = 12;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// The project directory Apila uses to store data and create repositories in
    #[arg(short, long)]
    project_dir: String,
}

impl Args {
    pub fn new(project_dir: String) -> Args {
        Args { project_dir }
    }
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

/// Where key presses go. The agent list drives everything until the user
/// steps into an agent's chat.
#[derive(Debug, PartialEq)]
enum Focus {
    Agents,
    Chat,
}

#[derive(Debug)]
pub struct TUIApp {
    args: Args,
    rt: runtime::Runtime,

    agents_table_state: TableState,
    /// The half typed chat message for each agent.
    chat_inputs: HashMap<String, String>,
    /// True while typing into the selected agent's chat box.
    chat_focused: bool,
    /// Shown along the bottom of the detail window until the next key press.
    status_message: Option<String>,

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

        // agents come from the directories in the project directory, so the
        // list is already populated before the first frame
        let rt = runtime::Runtime::new(runtime::RuntimeConfig::new(project_dir.clone(), cfg))?;

        let mut agents_table_state = TableState::default();
        if !rt.list_agents().is_empty() {
            agents_table_state.select(Some(0));
        }

        Ok(TUIApp {
            args,
            rt,
            agents_table_state,
            chat_inputs: HashMap::new(),
            chat_focused: false,
            status_message: None,
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

    // Input /////////////////////////////
    //////////////////////////////////////

    fn handle_key_events(&mut self) -> Result<(), TUIAppError> {
        // agents answer on their own threads; the wait has to time out so the
        // screen keeps up with them
        if !event::poll(TICK)? {
            return Ok(());
        }

        let key = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => key,
            _ => return Ok(()),
        };

        self.handle_key(key)
    }

    /// Acts on one key press, whichever window has focus.
    pub fn handle_key(&mut self, key: KeyEvent) -> Result<(), TUIAppError> {
        self.status_message = None;

        // always available, whatever has focus
        if let KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } = key
        {
            self.exit = true;
            return Ok(());
        }

        match self.focus() {
            Focus::Agents => self.handle_agents_key(key),
            Focus::Chat => self.handle_chat_key(key),
        }
    }

    fn handle_agents_key(&mut self, key: KeyEvent) -> Result<(), TUIAppError> {
        match key.code {
            KeyCode::Char('q') => self.exit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_agent_offset(1),
            KeyCode::Up | KeyCode::Char('k') => self.select_agent_offset(-1),
            KeyCode::Char('r') => self.reload_agents(),
            KeyCode::Enter | KeyCode::Char('i') => self.open_selected_agent(),
            _ => {}
        }
        Ok(())
    }

    /// `<Enter>` on an agent runs it the first time and steps into its chat
    /// from then on.
    fn open_selected_agent(&mut self) {
        let Some(definition) = self.selected_agent() else {
            return;
        };

        if definition.state().started() {
            self.chat_focused = true;
            return;
        }

        match self.rt.start_agent(&definition.id()) {
            Ok(_) => self.chat_focused = true,
            Err(err) => self.status_message = Some(err.to_string()),
        }
    }

    /// Picks up directories added or removed since the last load, and re-reads
    /// the configuration files of every agent that hasn't started.
    fn reload_agents(&mut self) {
        self.rt.load_agents();
        self.select_agent_offset(0);
    }

    fn handle_chat_key(&mut self, key: KeyEvent) -> Result<(), TUIAppError> {
        let Some(id) = self.selected_agent_id() else {
            return Ok(());
        };

        match key.code {
            KeyCode::Esc => self.chat_focused = false,
            KeyCode::Backspace => {
                if let Some(input) = self.chat_inputs.get_mut(&id) {
                    input.pop();
                }
            }
            KeyCode::Enter => {
                let content = self.chat_inputs.get(&id).cloned().unwrap_or_default();
                if content.trim() == "" {
                    return Ok(());
                }
                match self.rt.send_agent_message(&id, content) {
                    Ok(_) => {
                        self.chat_inputs.remove(&id);
                    }
                    Err(err) => self.status_message = Some(err.to_string()),
                }
            }
            KeyCode::Char(c) => {
                self.chat_inputs.entry(id).or_default().push(c);
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

    fn selected_agent(&self) -> Option<AgentDefinition> {
        let selected = self.agents_table_state.selected()?;
        self.rt.list_agents().into_iter().nth(selected)
    }

    fn selected_agent_id(&self) -> Option<String> {
        self.selected_agent().map(|agent| agent.id())
    }

    fn focus(&self) -> Focus {
        let started = self
            .selected_agent()
            .is_some_and(|agent| agent.state().started());
        if self.chat_focused && started {
            Focus::Chat
        } else {
            Focus::Agents
        }
    }

    // Rendering /////////////////////////
    //////////////////////////////////////

    pub fn draw(&mut self, frame: &mut Frame) {
        let instructions = Line::from(Self::instruction_spans(self.focus(), self.enter_label()));
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

    /// What `<Enter>` does to the selected agent, for the key hints.
    fn enter_label(&self) -> &'static str {
        let Some(definition) = self.selected_agent() else {
            return "Run Agent";
        };
        if definition.state().started() {
            return "Chat";
        }
        // with no directive there is nothing to run it on yet
        match definition.config_files() {
            Some(files) if files.directive_file().present() => "Run Agent",
            _ => "Open Chat",
        }
    }

    /// The key hints along the bottom, which follow whatever has focus.
    /// `enter_label` says what `<Enter>` does to the selected agent.
    fn instruction_spans(focus: Focus, enter_label: &'static str) -> Vec<Span<'static>> {
        let pairs: Vec<(&str, &str)> = match focus {
            Focus::Agents => vec![
                ("Quit", " <Q>"),
                ("Move", " <↑/↓>"),
                (enter_label, " <Enter>"),
                ("Reload", " <R>"),
            ],
            Focus::Chat => vec![
                ("Send", " <Enter>"),
                ("Leave Chat", " <Esc>"),
                ("Quit", " <Ctrl+C>"),
            ],
        };

        let count = pairs.len();
        pairs
            .iter()
            .enumerate()
            .flat_map(|(idx, (title, code))| {
                let mut spans = vec![
                    Span::from(title.to_string()),
                    code.to_string().blue().bold(),
                ];
                if idx + 1 < count {
                    spans.push(Span::from(" | "))
                }
                spans
            })
            .collect()
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
                let state = agent_definition.state();
                let indicator =
                    Span::from("●").style(Style::default().fg(Self::state_color(&state)));

                let context_used = agent_definition
                    .usage()
                    .map(|usage| usage.total_tokens as u64)
                    .unwrap_or(0);

                Row::new(vec![Cell::from(vec![
                    Line::from(vec![
                        indicator,
                        Span::from(" "),
                        Span::from(config.name()).bold().cyan(),
                        Span::from(format!("  {}", state.label())).dark_gray(),
                    ]),
                    Line::from(vec![Span::from("  "), config.model().label().into()]).cyan(),
                    Self::context_usage_line(context_used, CONTEXT_LIMIT_TOKENS, detail_width),
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

    /// The color the status dot takes for each agent state.
    fn state_color(state: &AgentState) -> Color {
        match state {
            AgentState::Configuring => Color::Yellow,
            AgentState::Working => Color::Green,
            AgentState::Idle => Color::Cyan,
            AgentState::Failed(_) => Color::Red,
        }
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

    /// The detail window. Everything about an agent lives here: its
    /// configuration, the directory picker and configuration files it is set
    /// up with, and the chat session once it is running.
    fn render_agent_detail_area(
        &mut self,
        area: Rect,
        frame: &mut Frame,
    ) -> Result<(), TUIAppError> {
        let block = Block::bordered()
            .title(" 🤖Agent ")
            .border_set(border::ROUNDED)
            .border_style(Style::default().cyan());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(definition) = self.selected_agent() else {
            let project_dir = self.rt.config().project_dir();
            let hint = Paragraph::new(vec![
                Line::from("No agents found.").dark_gray(),
                Line::from(""),
                Line::from(vec![
                    Span::from("Agents are loaded from the directories in ").dark_gray(),
                    Span::from(project_dir.to_string_lossy().to_string()).blue(),
                    Span::from(". Add one with an ").dark_gray(),
                    Span::from(agent_config::AGENTS_FILE_NAME).blue().bold(),
                    Span::from(" file in it, then press ").dark_gray(),
                    Span::from("<R>").blue().bold(),
                    Span::from(" to reload.").dark_gray(),
                ]),
            ])
            .wrap(Wrap::default());
            frame.render_widget(hint, inner);
            return Ok(());
        };

        let status_height = if self.status_message.is_some() { 2 } else { 0 };
        let config_lines = self.config_lines(&definition);
        let layout = Layout::vertical([
            Constraint::Length(config_lines.len() as u16 + 1),
            Constraint::Fill(1),
            Constraint::Length(status_height),
        ]);
        let [config_area, body_area, status_area] = layout.areas(inner);

        // render top info area
        frame.render_widget(Paragraph::new(config_lines), config_area);

        // render configuration or session
        match definition.state() {
            AgentState::Configuring => self.render_configuring(&definition, body_area, frame),
            _ => self.render_session(&definition, body_area, frame),
        }

        // render status message
        if let Some(status) = &self.status_message {
            frame.render_widget(
                Paragraph::new(Line::from(status.clone()).red()).wrap(Wrap::default()),
                status_area,
            );
        }

        Ok(())
    }

    /// The agent's configuration, shown above whatever the detail window is
    /// currently doing.
    fn config_lines(&self, definition: &AgentDefinition) -> Vec<Line<'static>> {
        let config = definition.config();
        let dir = config.dir();
        let dir_text = if dir.as_os_str().is_empty() {
            "(not selected)".to_string()
        } else {
            dir.to_string_lossy().to_string()
        };
        let state = definition.state();
        // an agent whose config.json failed to load has no tools at all, which
        // says more than an empty list
        let names = definition.tools().names();
        let tools = if names.is_empty() {
            "(none)".to_string()
        } else {
            names.join(", ")
        };

        vec![
            // the name is the directory the agent was loaded from, which is
            // also its id, so there is nothing else worth putting here
            Line::from(Span::from(config.name()).bold().cyan()),
            Line::from(vec![
                Span::from("Model:     ").dark_gray(),
                Span::from(config.model().label()).blue(),
            ]),
            Line::from(vec![
                Span::from("Directory: ").dark_gray(),
                Span::from(dir_text).blue(),
            ]),
            Line::from(vec![
                Span::from("Max iters: ").dark_gray(),
                Span::from(config.max_iterations().to_string()).blue(),
            ]),
            Line::from(vec![
                Span::from("Tools:     ").dark_gray(),
                Span::from(tools).blue(),
            ]),
            Line::from(vec![
                Span::from("State:     ").dark_gray(),
                Span::from(state.label()).style(Style::default().fg(Self::state_color(&state))),
            ]),
        ]
    }

    fn render_configuring(&mut self, definition: &AgentDefinition, area: Rect, frame: &mut Frame) {
        // config.json is what the rest is read from, so when it cannot be
        // loaded there is no checklist to show — only what went wrong with it
        let Some(config_files) = definition.config_files() else {
            let lines = match definition.config_error() {
                Some(err) => vec![
                    Line::from(format!("[ ] {}", agent_config::AGENT_CONFIG_FILE_NAME))
                        .bold()
                        .red(),
                    Line::from(format!("     {}", err)).red(),
                    Line::from(""),
                    Line::from("Fix it, then press <R> to reload.").red(),
                ],
                None => vec![Line::from("Not loaded yet.").dark_gray()],
            };
            frame.render_widget(Paragraph::new(lines).wrap(Wrap::default()), area);
            return;
        };

        let mut lines = vec![Line::from("Configuration files:").bold(), Line::from("")];
        for file in config_files.files() {
            // an optional file that isn't there is not a problem to fix, so it
            // is dimmed rather than flagged
            let (mark, style) = match (file.present(), file.required()) {
                (true, _) => ("[✓]", Style::default().green()),
                (false, true) => ("[ ]", Style::default().red()),
                (false, false) => ("[-]", Style::default().dark_gray()),
            };
            let mut name = vec![
                Span::from(mark).style(style),
                Span::from(format!(" {}", file.name())).bold(),
            ];
            if !file.required() {
                name.push(Span::from("  optional").dark_gray());
            }
            lines.push(Line::from(name));
            lines.push(Line::from(format!("     {}", file.path().to_string_lossy())).dark_gray());
        }

        lines.push(Line::from(""));
        if config_files.complete() {
            // the directive is what decides whether <Enter> starts the agent
            // working or just opens the chat
            let tail = if config_files.directive_file().present() {
                " to run the agent on its directive."
            } else {
                " to open the chat. With no directive the agent starts on your first message."
            };
            lines.push(
                Line::from(vec![
                    Span::from("Ready. Press "),
                    Span::from("<Enter>").blue().bold(),
                    Span::from(tail),
                ])
                .green(),
            );
        } else {
            lines.push(Line::from("Add the missing file(s), then press <R> to reload them.").red());
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap::default()), area);
    }

    /// The running agent's chat session: the transcript with an input box
    /// underneath it.
    fn render_session(&mut self, definition: &AgentDefinition, area: Rect, frame: &mut Frame) {
        let layout = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]);
        let [transcript_area, input_area] = layout.areas(area);

        let width = transcript_area.width.max(1) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for (idx, message) in definition.messages().iter().enumerate() {
            // the opening turn can come from DIRECTIVE.md instead of the user
            let directive = idx == 0 && definition.opened_with_directive();
            lines.extend(Self::message_lines(message, width, directive));
        }
        // what the user typed mid turn is on its way but not in the
        // conversation yet, so it is shown below what the agent is answering
        for message in definition.queued_messages() {
            lines.extend(Self::message_lines(message, width, false));
            lines.push(
                Line::from("  ↑ queued for the next turn")
                    .dark_gray()
                    .italic(),
            );
        }
        if definition.state() == AgentState::Working {
            lines.push(Line::from("…thinking").dark_gray().italic());
        }
        if let AgentState::Failed(err) = definition.state() {
            lines.push(Line::from(format!("error: {}", err)).red());
        }

        // keep the newest output on screen
        let height = transcript_area.height as usize;
        let scroll = lines.len().saturating_sub(height) as u16;
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), transcript_area);

        let focused = self.focus() == Focus::Chat;
        let input = self
            .chat_inputs
            .get(&definition.id())
            .cloned()
            .unwrap_or_default();
        let input_block = Block::bordered()
            .border_set(border::ROUNDED)
            .border_style(if focused {
                Style::default().cyan()
            } else {
                Style::default().dark_gray()
            })
            .title(if focused {
                " message "
            } else {
                " <Enter> to chat "
            });

        let text = if focused {
            Line::from(vec![Span::from(input), Span::from("▏").cyan()])
        } else {
            Line::from(input).dark_gray()
        };
        frame.render_widget(Paragraph::new(text).block(input_block), input_area);
    }

    /// One message rendered as a labeled block, hard wrapped to `width` so the
    /// transcript can be scrolled by whole lines. `directive` marks the opening
    /// turn that came out of `DIRECTIVE.md` rather than from the user.
    fn message_lines(message: &Message, width: usize, directive: bool) -> Vec<Line<'static>> {
        let (label, style) = match message {
            Message::User { .. } if directive => ("directive", Style::default().yellow().bold()),
            Message::User { .. } => ("you", Style::default().cyan().bold()),
            Message::Assistant { .. } => ("agent", Style::default().green().bold()),
            Message::Tool { .. } => ("tool", Style::default().magenta().bold()),
            Message::System { .. } => ("system", Style::default().dark_gray().bold()),
        };

        let mut lines = vec![Line::from(Span::from(label).style(style))];
        let content = message.content().unwrap_or("").to_string();

        let wrapped = Self::wrap(&content, width);
        // a command that printed thousands of lines would otherwise bury the
        // conversation it belongs to. the whole of it is still what went to the
        // model; this is only what is on screen
        let shown = match message {
            Message::Tool { .. } => TOOL_OUTPUT_LINES.min(wrapped.len()),
            _ => wrapped.len(),
        };
        for line in wrapped.iter().take(shown) {
            lines.push(Line::from(line.clone()));
        }
        if shown < wrapped.len() {
            lines.push(
                Line::from(format!("  … {} more lines", wrapped.len() - shown))
                    .dark_gray()
                    .italic(),
            );
        }

        for call in message.tool_calls() {
            // a command with a newline in it would otherwise break the layout,
            // so the arguments are kept to the one line they are shown on
            lines.push(
                Line::from(format!(
                    "  → {}({})",
                    call.function.name,
                    Self::one_line(&call.function.arguments, width.saturating_sub(6))
                ))
                .dark_gray(),
            );
        }
        lines.push(Line::from(""));
        lines
    }

    /// `text` on a single line, cut to `width` with an ellipsis when it does
    /// not fit.
    fn one_line(text: &str, width: usize) -> String {
        let flattened: String = text
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        if flattened.chars().count() <= width {
            return flattened;
        }
        let kept: String = flattened.chars().take(width.saturating_sub(1)).collect();
        format!("{}…", kept)
    }

    /// Word wraps `text` to `width` columns, keeping the author's own line breaks.
    fn wrap(text: &str, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        for paragraph in text.split('\n') {
            let mut current = String::new();
            for word in paragraph.split_whitespace() {
                if current.is_empty() {
                    current = word.to_string();
                } else if current.chars().count() + 1 + word.chars().count() <= width {
                    current.push(' ');
                    current.push_str(word);
                } else {
                    out.push(std::mem::take(&mut current));
                    current = word.to_string();
                }
            }
            out.push(current);
        }
        out
    }
}
