use chrono::{DateTime, FixedOffset, Local, TimeZone};
use ratatui::{
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::TableState,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::core::memory::recorder::{JobState, MemoryView};
use crate::core::memory::store::SkillListing;

use super::markdown::{markdown_lines, plain_lines};

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
/// From this age on a list row shows the date instead of how long ago.
const RELATIVE_LIMIT: i64 = 30 * DAY;

/// What the memories view was loaded against: the skill count and the newest
/// job, so a job that only updates a skill still triggers a reload.
pub type ReloadStamp = (Option<usize>, Option<(i64, JobState)>);

pub fn reload_stamp(view: &MemoryView) -> ReloadStamp {
    (
        view.skills,
        view.last_job
            .as_ref()
            .map(|job| (job.session_id, job.state.clone())),
    )
}

/// A delete waiting on `y`. It names the agent too, so a confirmation never
/// lands on another agent's skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelete {
    pub agent_id: String,
    pub skill_id: i64,
    pub name: String,
}

/// Where the reader was last drawn, for paging and the wheel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderView {
    pub area: Rect,
    pub max_top: usize,
    pub height: usize,
}

/// The selected agent's skills on the Memories tab.
#[derive(Debug)]
pub struct MemoriesView {
    agent_id: String,
    skills: Vec<SkillListing>,
    pub table_state: TableState,
    /// The first reader line on screen.
    top: usize,
    reader: Option<ReaderView>,
    confirm_delete: Option<PendingDelete>,
    stamp: ReloadStamp,
    /// The open skill's rendered lines, by skill id and width.
    rendered: Option<(i64, usize, Vec<Line<'static>>)>,
}

impl MemoriesView {
    pub fn new(agent_id: String, skills: Vec<SkillListing>, stamp: ReloadStamp) -> MemoriesView {
        let mut table_state = TableState::default();
        if !skills.is_empty() {
            table_state.select(Some(0));
        }
        MemoriesView {
            agent_id,
            skills,
            table_state,
            top: 0,
            reader: None,
            confirm_delete: None,
            stamp,
            rendered: None,
        }
    }

    /// Swaps in a fresh list, keeping the selected skill when it is still
    /// there and otherwise the row it was on.
    pub fn reload(&mut self, skills: Vec<SkillListing>, stamp: ReloadStamp) {
        let selected = self.selected().map(|skill| skill.id);
        let index = self.table_state.selected().unwrap_or(0);
        let kept = selected.and_then(|id| skills.iter().position(|skill| skill.id == id));
        let next = match kept {
            Some(position) => Some(position),
            None if skills.is_empty() => None,
            None => Some(index.min(skills.len() - 1)),
        };
        if kept.is_none() {
            self.top = 0;
        }
        self.skills = skills;
        self.table_state.select(next);
        self.stamp = stamp;
        self.confirm_delete = None;
        self.rendered = None;
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn skills(&self) -> &[SkillListing] {
        &self.skills
    }

    pub fn selected(&self) -> Option<&SkillListing> {
        self.skills.get(self.table_state.selected()?)
    }

    pub fn needs_reload(&self, stamp: &ReloadStamp) -> bool {
        &self.stamp != stamp
    }

    /// Moves the selection, clamped to the list, and opens the new skill at
    /// its first line.
    pub fn select_offset(&mut self, offset: isize) {
        if self.skills.is_empty() {
            self.table_state.select(None);
            return;
        }
        let last = self.skills.len() as isize - 1;
        let selected = match self.table_state.selected() {
            Some(selected) => (selected as isize + offset).clamp(0, last) as usize,
            None => 0,
        };
        if Some(selected) != self.table_state.selected() {
            self.top = 0;
        }
        self.table_state.select(Some(selected));
    }

    pub fn top(&self) -> usize {
        self.top
    }

    /// Scrolls the reader by `delta` lines, within what was last drawn.
    pub fn scroll_by(&mut self, delta: isize) {
        let max_top = self.reader.map(|reader| reader.max_top).unwrap_or(0);
        self.top = self.top.saturating_add_signed(delta).min(max_top);
    }

    pub fn reader(&self) -> Option<ReaderView> {
        self.reader
    }

    /// Records the drawn reader, pulling a position past its end back.
    pub fn set_reader(&mut self, reader: Option<ReaderView>) {
        if let Some(reader) = reader {
            self.top = self.top.min(reader.max_top);
        }
        self.reader = reader;
    }

    /// Asks to delete the selected skill. False when there is none.
    pub fn arm_delete(&mut self) -> bool {
        let Some(skill) = self.selected() else {
            return false;
        };
        self.confirm_delete = Some(PendingDelete {
            agent_id: self.agent_id.clone(),
            skill_id: skill.id,
            name: skill.name.clone(),
        });
        true
    }

    pub fn confirm_delete(&self) -> Option<&PendingDelete> {
        self.confirm_delete.as_ref()
    }

    pub fn take_confirmed(&mut self) -> Option<PendingDelete> {
        self.confirm_delete.take()
    }

    /// The open skill's reader lines at `width`, rendered again only when the
    /// skill or the width changes.
    pub fn reader_lines(&mut self, width: usize) -> &[Line<'static>] {
        let Some(skill) = self.selected() else {
            return &[];
        };
        let id = skill.id;
        let stale = !matches!(&self.rendered, Some((cached, cached_width, _)) if *cached == id && *cached_width == width);
        if stale {
            let lines = body_lines(skill, width);
            self.rendered = Some((id, width, lines));
        }
        self.rendered
            .as_ref()
            .map(|(_, _, lines)| lines.as_slice())
            .unwrap_or(&[])
    }
}

/// The reader's text: when the skill was updated, when to use it, and its
/// body rendered as markdown.
pub fn body_lines(skill: &SkillListing, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = vec![Line::from(vec![
        Span::from("Updated ").dark_gray(),
        Span::from(updated_label(skill.updated_at)).blue(),
    ])];
    let when = skill.when_to_use.replace(['\r', '\n'], " ");
    lines.extend(plain_lines(when.trim(), Style::default().italic(), width));
    lines.push(Line::from("─".repeat(width)).dark_gray());
    lines.extend(markdown_lines(&skill.body, width));
    lines
}

/// Each skill's two list lines: its name, then when to use it with how long
/// ago it was updated right-aligned. Both fit `width`.
pub fn list_rows(skills: &[SkillListing], width: usize, now: i64) -> Vec<[Line<'static>; 2]> {
    skills
        .iter()
        .map(|skill| {
            let name = Line::from(Span::from(fit_width(&skill.name, width)).bold().cyan());
            let age = relative_label(skill.updated_at, now);
            let age_width = age.width();
            let detail = if age_width + 2 > width {
                Line::from(Span::from(fit_width(&age, width)).dark_gray())
            } else {
                let room = width - age_width - 1;
                let when = fit_width(&skill.when_to_use, room);
                let gap = " ".repeat(width - when.width() - age_width);
                Line::from(vec![
                    Span::from(when).dark_gray(),
                    Span::from(gap),
                    Span::from(age).dark_gray(),
                ])
            };
            [name, detail]
        })
        .collect()
}

/// `text` on one line within `width` columns, cut with an ellipsis.
pub fn fit_width(text: &str, width: usize) -> String {
    let flattened: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    if flattened.width() <= width {
        return flattened;
    }
    let Some(room) = width.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0;
    for grapheme in flattened.graphemes(true) {
        let grapheme_width = grapheme.width();
        if used + grapheme_width > room {
            break;
        }
        out.push_str(grapheme);
        used += grapheme_width;
    }
    out.push('…');
    out
}

/// How long ago `updated_at` was, for a list row. A time in the future is
/// a clock that moved, and reads as just now.
pub fn relative_label(updated_at: i64, now: i64) -> String {
    if updated_at <= 0 {
        return "—".to_string();
    }
    let age = now - updated_at;
    if age < MINUTE {
        "just now".to_string()
    } else if age < HOUR {
        format!("{}m ago", age / MINUTE)
    } else if age < DAY {
        format!("{}h ago", age / HOUR)
    } else if age < RELATIVE_LIMIT {
        format!("{}d ago", age / DAY)
    } else {
        local_time(updated_at)
            .map(|time| time.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "—".to_string())
    }
}

/// When `updated_at` was, in local time, for the reader.
pub fn updated_label(updated_at: i64) -> String {
    if updated_at <= 0 {
        return "—".to_string();
    }
    local_time(updated_at)
        .map(format_absolute)
        .unwrap_or_else(|| "—".to_string())
}

pub fn format_absolute(time: DateTime<FixedOffset>) -> String {
    time.format("%Y-%m-%d %H:%M").to_string()
}

fn local_time(seconds: i64) -> Option<DateTime<FixedOffset>> {
    Local
        .timestamp_opt(seconds, 0)
        .earliest()
        .map(|time| time.fixed_offset())
}
