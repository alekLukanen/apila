use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, Cell, HighlightSpacing, Padding, Paragraph, Row, Table, TableState, Wrap},
    DefaultTerminal, Frame,
};
use thiserror::Error;

use clap::Parser;

use crate::core::config::config;
use crate::core::memory::recorder::{JobState, MemoryJob, MemoryView};
use crate::core::memory::store::SkillsSaved;
use crate::core::openrouter::types::{Message, Spend};
use crate::core::runtime::agent::{AgentDefinition, AgentState, ClearOutcome};
use crate::core::runtime::{agent_config, runtime};

use super::chat_input::{ChatInput, INPUT_MAX_ROWS};
use super::scroll::{self, ChatScroll, WHEEL_LINES};

/// How long the ui waits for a key before redrawing. Agents answer on
/// background threads, so the screen has to refresh without any input.
const TICK: Duration = Duration::from_millis(100);

/// Events already waiting are handled before the next redraw, so a fast flick
/// of the wheel doesn't cost a frame per tick. Capped so the screen still moves.
const MAX_QUEUED_EVENTS: usize = 64;

/// TODO: read the real context window off the model once it is available.
const CONTEXT_LIMIT_TOKENS: u64 = 1_000_000;

/// How much of a tool's output the transcript shows before it says how much
/// more there was. The model still gets all of it.
const TOOL_OUTPUT_LINES: usize = 12;

/// The column a tab advances to. Terminals use eight and files are written
/// expecting it, so anything else would misalign the thing the transcript is
/// trying to show faithfully.
pub(super) const TAB_WIDTH: usize = 8;

/// The widest a figure on an agent row's cost line may be, so the whole line
/// fits the narrowest list a normal terminal gives it.
const COMPACT_COST_WIDTH: usize = 5;

/// Typed into the chat, ends the agent's session and starts a new one.
const CLEAR_COMMAND: &str = "/clear";

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

/// Where the selected agent's chat was last drawn, so the wheel and paging keys
/// know what they are scrolling and Up/Down know how the input wraps.
#[derive(Debug, Clone)]
struct ChatView {
    agent_id: String,
    transcript_area: Rect,
    max_top: usize,
    height: usize,
    input_width: usize,
}

#[derive(Debug)]
pub struct TUIApp {
    args: Args,
    rt: runtime::Runtime,

    agents_table_state: TableState,
    /// The half typed chat message for each agent.
    chat_inputs: HashMap<String, ChatInput>,
    /// How far back each agent's transcript is scrolled. Missing means it
    /// follows the newest output.
    chat_scroll: HashMap<String, ChatScroll>,
    /// Set while drawing a chat session, and cleared before every redraw.
    chat_view: Option<ChatView>,
    /// True while typing into the selected agent's chat box.
    chat_focused: bool,
    /// Shown along the bottom of the detail window until the next key press.
    status_message: Option<String>,
    /// The same, for news that is not an error.
    info_message: Option<String>,

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
            chat_scroll: HashMap::new(),
            chat_view: None,
            chat_focused: false,
            status_message: None,
            info_message: None,
            exit: false,
        })
    }

    pub fn run(&mut self, term: &mut DefaultTerminal) -> Result<(), TUIAppError> {
        // draw initial frame
        term.draw(|frame| self.draw(frame))?;

        // setup...

        while !self.exit {
            term.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
        }

        Ok(())
    }

    // Input /////////////////////////////
    //////////////////////////////////////

    fn handle_events(&mut self) -> Result<(), TUIAppError> {
        // agents answer on their own threads; the wait has to time out so the
        // screen keeps up with them
        if !event::poll(TICK)? {
            return Ok(());
        }
        self.handle_event(event::read()?)?;

        for _ in 0..MAX_QUEUED_EVENTS {
            if self.exit || !event::poll(Duration::ZERO)? {
                break;
            }
            self.handle_event(event::read()?)?;
        }
        Ok(())
    }

    /// Acts on one terminal event, the way the event loop would.
    pub fn handle_event(&mut self, event: Event) -> Result<(), TUIAppError> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(mouse) => {
                self.handle_mouse(mouse);
                Ok(())
            }
            Event::Paste(text) => {
                self.handle_paste(&text);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Acts on one key press, whichever window has focus.
    pub fn handle_key(&mut self, key: KeyEvent) -> Result<(), TUIAppError> {
        self.status_message = None;
        self.info_message = None;

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

        // the transcript scrolls whichever window has focus
        let page = match key.code {
            KeyCode::PageUp => Some(-1),
            KeyCode::PageDown => Some(1),
            _ => None,
        };
        if let Some(direction) = page {
            self.page_chat(direction);
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
        // Up/Down follow the rows as drawn; with nothing drawn yet they fall
        // back to the lines as typed
        let width = self
            .selected_chat_view()
            .map(|view| view.input_width)
            .unwrap_or(usize::MAX);
        let modifiers = key.modifiers;

        match key.code {
            KeyCode::Esc => self.chat_focused = false,
            KeyCode::Enter if modifiers.is_empty() => self.send_chat_input(&id),
            KeyCode::Enter if modifiers == KeyModifiers::ALT => {
                self.chat_input(&id).insert_newline()
            }
            KeyCode::Char('j') if modifiers == KeyModifiers::CONTROL => {
                self.chat_input(&id).insert_newline()
            }
            KeyCode::Left => self.chat_input(&id).move_left(),
            KeyCode::Right => self.chat_input(&id).move_right(),
            KeyCode::Up => self.chat_input(&id).move_up(width),
            KeyCode::Down => self.chat_input(&id).move_down(width),
            KeyCode::Home => self.chat_input(&id).home(),
            KeyCode::End => self.chat_input(&id).end(),
            KeyCode::Backspace => self.chat_input(&id).backspace(),
            KeyCode::Delete => self.chat_input(&id).delete(),
            KeyCode::Char(c)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.chat_input(&id).insert_char(c)
            }
            _ => {}
        }
        Ok(())
    }

    fn chat_input(&mut self, id: &str) -> &mut ChatInput {
        self.chat_inputs.entry(id.to_string()).or_default()
    }

    fn send_chat_input(&mut self, id: &str) {
        let content = self
            .chat_inputs
            .get(id)
            .map(|input| input.text().to_string())
            .unwrap_or_default();
        if content.trim() == "" {
            return;
        }
        if content.trim() == CLEAR_COMMAND {
            self.clear_session(id);
            return;
        }
        match self.rt.send_agent_message(id, content) {
            Ok(_) => {
                self.chat_inputs.remove(id);
                self.chat_scroll.remove(id);
            }
            Err(err) => self.status_message = Some(err.to_string()),
        }
    }

    /// Only the wheel does anything; clicks are reported too but ignored.
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let delta = match mouse.kind {
            MouseEventKind::ScrollUp => -(WHEEL_LINES as isize),
            MouseEventKind::ScrollDown => WHEEL_LINES as isize,
            _ => return,
        };
        let Some(view) = self.selected_chat_view() else {
            return;
        };
        if view
            .transcript_area
            .contains(Position::new(mouse.column, mouse.row))
        {
            self.scroll_chat(&view, delta);
        }
    }

    /// A paste outside the chat would otherwise run as a string of key
    /// commands, so it goes nowhere.
    fn handle_paste(&mut self, text: &str) {
        if self.focus() != Focus::Chat {
            return;
        }
        if let Some(id) = self.selected_agent_id() {
            self.chat_input(&id).insert_str(text);
        }
    }

    /// Scrolls the selected agent's transcript by a page, `direction` being
    /// negative to go back.
    fn page_chat(&mut self, direction: isize) {
        if let Some(view) = self.selected_chat_view() {
            let lines = scroll::page_lines(view.height) as isize;
            self.scroll_chat(&view, direction * lines);
        }
    }

    fn scroll_chat(&mut self, view: &ChatView, delta: isize) {
        let state = self
            .chat_scroll
            .get(&view.agent_id)
            .copied()
            .unwrap_or_default();
        match scroll::scrolled(state, delta, view.max_top) {
            ChatScroll::Follow => {
                self.chat_scroll.remove(&view.agent_id);
            }
            top => {
                self.chat_scroll.insert(view.agent_id.clone(), top);
            }
        }
    }

    /// The last drawn chat, if it is the selected agent's. Another agent may
    /// have been selected since, and its chat isn't on screen yet.
    fn selected_chat_view(&self) -> Option<ChatView> {
        let view = self.chat_view.as_ref()?;
        (Some(&view.agent_id) == self.selected_agent_id().as_ref()).then(|| view.clone())
    }

    /// `/clear` ends the session rather than going to the model as a message.
    fn clear_session(&mut self, id: &str) {
        match self.rt.clear_agent_session(id) {
            Ok(outcome) => {
                self.chat_inputs.remove(id);
                self.chat_scroll.remove(id);
                self.info_message = Some(match outcome {
                    ClearOutcome::Cleared => "session cleared".into(),
                    ClearOutcome::Queued => "clear queued — runs when this turn ends".into(),
                });
            }
            Err(err) => self.status_message = Some(err.to_string()),
        }
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
                ("Newline", " <Alt+Enter>"),
                ("Scroll", " <PgUp/PgDn>"),
                ("New Session", " /clear"),
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
                    Line::from(vec![Span::from("  "), config.model_label().into()]).cyan(),
                    Self::context_usage_line(context_used, CONTEXT_LIMIT_TOKENS, detail_width),
                    Self::agent_cost_line(agent_definition, detail_width),
                ])])
                .height(4)
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

    /// What the agent's chat and memory have cost. Totals kept in the memory
    /// database show `…` until they have been read.
    fn agent_cost_line(definition: &AgentDefinition, width: usize) -> Line<'static> {
        let memory = definition.memory_view();
        let (all, mem) = if memory.loaded {
            (
                Some(definition.all_sessions_spend()),
                Some(memory.memory_spend()),
            )
        } else {
            (None, None)
        };
        Self::cost_line(definition.session_spend(), all, mem, width)
    }

    /// `sess $0.04 all $0.42 mem $0.03`, cut to `width` when it cannot fit.
    pub(super) fn cost_line(
        session: Spend,
        all: Option<Spend>,
        mem: Option<Spend>,
        width: usize,
    ) -> Line<'static> {
        let figure = |spend: Option<Spend>| {
            spend
                .map(|spend| Self::compact_cost(&spend))
                .unwrap_or_else(|| "…".to_string())
        };
        let parts = [
            ("sess", figure(Some(session))),
            ("all", figure(all)),
            ("mem", figure(mem)),
        ];
        let plain = parts
            .iter()
            .map(|(label, value)| format!("{} {}", label, value))
            .collect::<Vec<_>>()
            .join(" ");
        let length = plain.chars().count();
        if length > width {
            return Line::from(Self::one_line(&plain, width)).dark_gray();
        }

        let mut spans = Vec::new();
        if length < width {
            spans.push(Span::from(" "));
        }
        for (index, (label, value)) in parts.into_iter().enumerate() {
            let gap = if index == 0 { "" } else { " " };
            spans.push(Span::from(format!("{}{} ", gap, label)).dark_gray());
            spans.push(Span::from(value).cyan());
        }
        Line::from(spans)
    }

    /// A cost in at most [`COMPACT_COST_WIDTH`] characters, losing precision
    /// as it grows. `~` marks a total some responses left unpriced.
    pub(super) fn compact_cost(spend: &Spend) -> String {
        let nanos = i128::from(spend.cost_nanos.max(0));
        if spend.unpriced > 0 && nanos == 0 {
            return "—".to_string();
        }
        let prefix = if spend.unpriced > 0 { "~" } else { "" };
        if nanos == 0 {
            return "$0".to_string();
        }

        // each tier is a unit in nanodollars and the decimals shown in it
        const TIERS: [(i128, u32, &str); 6] = [
            (1_000_000_000, 2, ""),
            (1_000_000_000, 1, ""),
            (1_000_000_000, 0, ""),
            (1_000_000_000_000, 0, "k"),
            (1_000_000_000_000_000, 0, "M"),
            (1_000_000_000_000_000_000, 0, "B"),
        ];
        for (unit, decimals, suffix) in TIERS {
            let step = unit / 10i128.pow(decimals);
            let rounded = (nanos + step / 2) / step;
            if rounded == 0 {
                continue;
            }
            let scale = 10i128.pow(decimals);
            let (whole, fraction) = (rounded / scale, rounded % scale);
            let number = match (decimals, whole) {
                (0, _) => whole.to_string(),
                // below a dollar the leading zero is the first thing to go
                (_, 0) => format!(".{:0width$}", fraction, width = decimals as usize),
                _ => format!("{}.{:0width$}", whole, fraction, width = decimals as usize),
            };
            let candidates = if decimals > 0 && whole == 0 {
                vec![
                    format!("{}$0{}", prefix, number),
                    format!("{}${}", prefix, number),
                ]
            } else {
                vec![format!("{}${}{}", prefix, number, suffix)]
            };
            if let Some(text) = candidates
                .into_iter()
                .find(|text| text.chars().count() <= COMPACT_COST_WIDTH)
            {
                return text;
            }
        }
        // under a cent: too small to show, and too small to matter when partial
        if spend.unpriced > 0 {
            "~$0".to_string()
        } else {
            "<$.01".to_string()
        }
    }

    /// A cost for the detail pane, with more precision the smaller it is.
    pub(super) fn human_cost(spend: &Spend) -> String {
        let nanos = i128::from(spend.cost_nanos.max(0));
        if spend.unpriced > 0 && nanos == 0 {
            return "—".to_string();
        }
        let prefix = if spend.unpriced > 0 { "~" } else { "" };
        let text = if nanos == 0 {
            "$0".to_string()
        } else if nanos < 100_000 {
            "<$0.0001".to_string()
        } else {
            // the precision is picked after rounding, so 0.009996 reads $0.01
            let ten_thousandths = (nanos + 50_000) / 100_000;
            let cents = (nanos + 5_000_000) / 10_000_000;
            if ten_thousandths < 100 {
                format!("$0.{:04}", ten_thousandths)
            } else if cents < 10_000 {
                format!("${}.{:02}", cents / 100, cents % 100)
            } else {
                format!("${}", (nanos + 500_000_000) / 1_000_000_000)
            }
        };
        format!("{}{}", prefix, text)
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
        self.chat_view = None;

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

        let status_height = if self.status_message.is_some() || self.info_message.is_some() {
            2
        } else {
            0
        };
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
        } else if let Some(info) = &self.info_message {
            frame.render_widget(
                Paragraph::new(Line::from(info.clone()).dark_gray()).wrap(Wrap::default()),
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

        let mut lines = vec![
            // the name is the directory the agent was loaded from, which is
            // also its id, so there is nothing else worth putting here
            Line::from(Span::from(config.name()).bold().cyan()),
            Line::from(vec![
                Span::from("Model:     ").dark_gray(),
                Span::from(config.model_label()).blue(),
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
        ];
        lines.extend(Self::memory_lines(
            &definition.memory_view(),
            config.memory().skills.is_some(),
        ));
        lines
    }

    /// The `Memory:` line, the newest job's, and any error, shown under `State:`.
    pub(super) fn memory_lines(view: &MemoryView, skills_on: bool) -> Vec<Line<'static>> {
        let labelled = |label: &'static str, text: String| {
            Line::from(vec![Span::from(label).dark_gray(), Span::from(text).blue()])
        };
        let mut lines = Vec::new();
        if skills_on {
            let skills = match view.skills {
                Some(count) => format!("{} {}", count, plural(count, "skill")),
                None => "…".to_string(),
            };
            lines.push(labelled(
                "Memory:    ",
                format!("{} · {}", skills, Self::jobs_summary(view)),
            ));
            lines.push(labelled(
                "Last job:  ",
                Self::job_summary(view.last_job.as_ref()),
            ));
        } else {
            lines.push(labelled("Memory:    ", "skills off".to_string()));
            if view.last_job.is_some() {
                lines.push(labelled(
                    "Last job:  ",
                    Self::job_summary(view.last_job.as_ref()),
                ));
            }
        }
        if let Some(message) = &view.message {
            lines.push(Line::from(vec![
                Span::from("Status:    ").dark_gray(),
                Span::from(message.clone()).red(),
            ]));
        }
        lines
    }

    /// How many sessions are being or waiting to be analysed. After a failure
    /// the writer stops, so the rest wait for the next `/clear`.
    fn jobs_summary(view: &MemoryView) -> String {
        let pending = view.pending.unwrap_or(0);
        if view.stalled() && pending > 0 {
            return format!(
                "{} {} pending (retries on next {})",
                pending,
                plural(pending, "job"),
                CLEAR_COMMAND
            );
        }
        match pending + usize::from(view.job_running()) {
            0 => "no jobs running".to_string(),
            count => format!("{} {} running", count, plural(count, "job")),
        }
    }

    /// `session #7 · added 2 skills · 14k tok · $0.0031`, or `none yet`.
    pub(super) fn job_summary(job: Option<&MemoryJob>) -> String {
        let Some(job) = job else {
            return "none yet".to_string();
        };
        let state = match &job.state {
            JobState::Running => "analysing".to_string(),
            JobState::Done { skills } => Self::skills_summary(skills),
            JobState::Failed(err) => format!("failed: {}", Self::one_line(err, usize::MAX)),
            JobState::Interrupted => "interrupted".to_string(),
        };
        format!(
            "session #{} · {} · {} tok · {}",
            job.session_id,
            state,
            Self::human_tokens(job.spend.tokens),
            Self::human_cost(&job.spend)
        )
    }

    fn skills_summary(skills: &SkillsSaved) -> String {
        match (skills.added, skills.updated) {
            (0, 0) => "saved 0 skills".to_string(),
            (added, 0) => format!("added {} {}", added, plural(added, "skill")),
            (0, updated) => format!("updated {} {}", updated, plural(updated, "skill")),
            (added, updated) => format!("added {} · updated {}", added, updated),
        }
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
        let id = definition.id();
        let focused = self.focus() == Focus::Chat;

        // one column is kept free so the cursor has somewhere to sit at the end
        // of a full row
        let input_width = (area.width.saturating_sub(2) as usize)
            .saturating_sub(1)
            .max(1);
        let window = self.chat_input(&id).window(input_width, INPUT_MAX_ROWS);

        let layout = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(window.lines.len() as u16 + 2),
        ]);
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
        // a clear waits for the turn to end, and what is sent meanwhile opens
        // the session after it
        if definition.clear_pending() {
            lines.push(Line::from(""));
            lines.push(
                Line::from("── clear queued — runs when this turn ends ──")
                    .yellow()
                    .italic(),
            );
            if !definition.next_session_messages().is_empty() {
                lines.push(Line::from("next session").yellow().bold());
                for message in definition.next_session_messages() {
                    lines.extend(Self::message_lines(message, width, false));
                }
            }
        }

        let height = transcript_area.height as usize;
        let total = lines.len();
        let max_top = total.saturating_sub(height);
        // a transcript that shrank under a held position follows again
        let state = match self.chat_scroll.get(&id).copied() {
            Some(ChatScroll::Top(top)) if top < max_top => ChatScroll::Top(top),
            _ => {
                self.chat_scroll.remove(&id);
                ChatScroll::Follow
            }
        };
        let top = scroll::visible_top(state, max_top);
        let mut shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
        if top < max_top && height >= 2 {
            shown.pop();
            let hidden = total - (top + shown.len());
            shown.push(
                Line::from(format!("↓ {} more lines — PgDn or scroll down", hidden))
                    .dark_gray()
                    .italic(),
            );
        }
        frame.render_widget(Paragraph::new(shown), transcript_area);

        self.chat_view = Some(ChatView {
            agent_id: id,
            transcript_area,
            max_top,
            height,
            input_width,
        });

        let mut input_block = Block::bordered()
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

        if focused && (window.hidden_above || window.hidden_below) {
            let arrows = match (window.hidden_above, window.hidden_below) {
                (true, true) => " ↑↓ ",
                (true, false) => " ↑ ",
                _ => " ↓ ",
            };
            input_block = input_block.title_top(Line::from(arrows).dark_gray().right_aligned());
        }
        let inner = input_block.inner(input_area);

        let style = if focused {
            Style::default()
        } else {
            Style::default().dark_gray()
        };
        let text: Vec<Line> = window
            .lines
            .into_iter()
            .map(|line| Line::from(line).style(style))
            .collect();
        frame.render_widget(Paragraph::new(text).block(input_block), input_area);

        // an area too small to hold the box has nowhere to put the cursor
        if focused
            && window.cursor_row < inner.height as usize
            && window.cursor_col < inner.width as usize
        {
            frame.set_cursor_position(Position::new(
                inner.x + window.cursor_col as u16,
                inner.y + window.cursor_row as u16,
            ));
        }
    }

    /// One message rendered as a labeled block, hard wrapped to `width` so the
    /// transcript can be scrolled by whole lines. `directive` marks the opening
    /// turn that came out of `DIRECTIVE.md` rather than from the user.
    pub fn message_lines(message: &Message, width: usize, directive: bool) -> Vec<Line<'static>> {
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

    /// No whitespace is collapsed, so command output keeps its columns. Every
    /// returned line fits `width`, so counting lines counts what is drawn.
    pub fn wrap(text: &str, width: usize) -> Vec<String> {
        // a zero width would otherwise make no line ever fit
        let width = width.max(1);
        let mut out = Vec::new();

        for line in text.split('\n') {
            // a file with windows line endings would otherwise draw the carriage
            // return as a glyph of its own at the end of every line
            let line = Self::expand_tabs(line.strip_suffix('\r').unwrap_or(line));
            let characters: Vec<char> = line.chars().collect();

            if characters.len() <= width {
                out.push(line);
                continue;
            }

            let mut start = 0;
            while start < characters.len() {
                if characters.len() - start <= width {
                    out.push(characters[start..].iter().collect());
                    break;
                }
                let end = Self::break_at(&characters, start, start + width);
                out.push(characters[start..end].iter().collect());
                start = end;
            }
        }

        out
    }

    /// Breaks just after the last space so the next line starts on text. A run
    /// with no space is cut at `limit` rather than drawn clipped.
    fn break_at(characters: &[char], start: usize, limit: usize) -> usize {
        // a space inside the indentation is not somewhere to break: doing so
        // would hand back a line of nothing but whitespace and come straight
        // back for the rest
        let Some(content) = (start..limit).find(|index| characters[*index] != ' ') else {
            return limit;
        };

        (content..limit)
            .rev()
            .find(|index| characters[*index] == ' ')
            .map(|index| index + 1)
            .unwrap_or(limit)
    }

    /// A tab left in a cell takes one column or none, which turns a tab
    /// indented file ragged.
    fn expand_tabs(line: &str) -> String {
        if !line.contains('\t') {
            return line.to_string();
        }

        let mut out = String::with_capacity(line.len());
        let mut column = 0;
        for character in line.chars() {
            if character == '\t' {
                let advance = TAB_WIDTH - (column % TAB_WIDTH);
                out.extend(std::iter::repeat_n(' ', advance));
                column += advance;
            } else {
                out.push(character);
                column += 1;
            }
        }
        out
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{}s", noun)
    }
}
