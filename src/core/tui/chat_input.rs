use std::ops::Range;

use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;

use super::app::TAB_WIDTH;

/// The most rows the input box grows to before it scrolls its text instead.
pub const INPUT_MAX_ROWS: usize = 3;

/// A chat message being written. `cursor` is a byte index into `text`, and
/// `goal_col` is the column Up/Down aim for so a short row in between doesn't
/// pull the cursor to the left for good.
#[derive(Debug, Default, Clone)]
pub struct ChatInput {
    text: String,
    cursor: usize,
    goal_col: Option<usize>,
    top_row: usize,
}

/// What the input box draws: the rows in its window, tabs already expanded,
/// and where the cursor sits within that window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputWindow {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub hidden_above: bool,
    pub hidden_below: bool,
}

impl ChatInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn top_row(&self) -> usize {
        self.top_row
    }

    // Editing ///////////////////////////
    //////////////////////////////////////

    pub fn insert_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.insert_raw(c.encode_utf8(&mut [0; 4]));
    }

    pub fn insert_newline(&mut self) {
        self.insert_raw("\n");
    }

    /// Pasted text. Line endings become `\n` and tabs are kept for the message;
    /// other control characters would draw as nothing and are dropped.
    pub fn insert_str(&mut self, text: &str) {
        let cleaned: String = text
            .replace("\r\n", "\n")
            .chars()
            .map(|c| if c == '\r' { '\n' } else { c })
            .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
            .collect();
        self.insert_raw(&cleaned);
    }

    fn insert_raw(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        if let Some(start) = previous_boundary(&self.text, self.cursor) {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if let Some(end) = next_boundary(&self.text, self.cursor) {
            self.text.replace_range(self.cursor..end, "");
        }
        self.goal_col = None;
    }

    // Moving ////////////////////////////
    //////////////////////////////////////

    pub fn move_left(&mut self) {
        if let Some(start) = previous_boundary(&self.text, self.cursor) {
            self.cursor = start;
        }
        self.goal_col = None;
    }

    pub fn move_right(&mut self) {
        if let Some(end) = next_boundary(&self.text, self.cursor) {
            self.cursor = end;
        }
        self.goal_col = None;
    }

    /// The start of the line the cursor is on, ignoring soft wraps.
    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.goal_col = None;
    }

    /// The end of the line the cursor is on, ignoring soft wraps.
    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.text.len());
        self.goal_col = None;
    }

    pub fn move_up(&mut self, width: usize) {
        self.move_vertically(width, false);
    }

    pub fn move_down(&mut self, width: usize) {
        self.move_vertically(width, true);
    }

    /// Steps to the neighbouring visual row. On the first or last row there is
    /// nowhere to go, and nothing changes.
    fn move_vertically(&mut self, width: usize, down: bool) {
        let rows = rows(&self.text, width);
        let (row, col) = cursor_row_col_in(&self.text, &rows, self.cursor);
        let target = if down {
            if row + 1 >= rows.len() {
                return;
            }
            row + 1
        } else {
            if row == 0 {
                return;
            }
            row - 1
        };

        let goal = *self.goal_col.get_or_insert(col);
        self.cursor = position_in_row(&self.text, &rows, target, goal);
    }

    // Layout ////////////////////////////
    //////////////////////////////////////

    /// The cursor's visual row and column when wrapped to `width`.
    pub fn cursor_row_col(&self, width: usize) -> (usize, usize) {
        cursor_row_col_in(&self.text, &rows(&self.text, width), self.cursor)
    }

    /// Wraps to `width` and moves the window just enough to keep the cursor in
    /// it. The window is remembered so the text doesn't jump on every redraw.
    pub fn window(&mut self, width: usize, max_rows: usize) -> InputWindow {
        let rows = rows(&self.text, width);
        let (cursor_row, cursor_col) = cursor_row_col_in(&self.text, &rows, self.cursor);
        let visible = rows.len().clamp(1, max_rows.max(1));
        self.top_row = scroll_to_cursor(self.top_row, cursor_row, rows.len(), visible);

        let lines = rows[self.top_row..self.top_row + visible]
            .iter()
            .map(|row| self.text[row.clone()].replace('\t', &" ".repeat(TAB_WIDTH)))
            .collect();
        InputWindow {
            lines,
            cursor_row: cursor_row - self.top_row,
            cursor_col,
            hidden_above: self.top_row > 0,
            hidden_below: self.top_row + visible < rows.len(),
        }
    }
}

/// The columns a grapheme takes when drawn. A tab is drawn as spaces, and
/// other control characters are dropped by the renderer, so take nothing.
fn grapheme_width(grapheme: &str) -> usize {
    if grapheme == "\t" {
        TAB_WIDTH
    } else if grapheme.contains(char::is_control) {
        0
    } else {
        grapheme.cell_width() as usize
    }
}

/// The byte ranges of `text` drawn on each row at `width` columns. A `\n`
/// starts a new row and is in none of them. Rows break after the last space
/// when they can, and always take at least one grapheme so wrapping finishes.
pub fn rows(text: &str, width: usize) -> Vec<Range<usize>> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        wrap_line(line, line_start, width, &mut out);
        line_start += line.len() + 1;
    }
    out
}

fn wrap_line(line: &str, offset: usize, width: usize, out: &mut Vec<Range<usize>>) {
    let graphemes: Vec<(usize, &str)> = line.grapheme_indices(true).collect();
    if graphemes.is_empty() {
        out.push(offset..offset);
        return;
    }
    let byte_at = |index: usize| {
        graphemes
            .get(index)
            .map(|(byte, _)| offset + byte)
            .unwrap_or(offset + line.len())
    };

    let mut start = 0;
    while start < graphemes.len() {
        let mut limit = start;
        let mut used = 0usize;
        while limit < graphemes.len() {
            let next = used.saturating_add(grapheme_width(graphemes[limit].1));
            if next > width && limit > start {
                break;
            }
            used = next;
            limit += 1;
        }
        let end = if limit == graphemes.len() {
            limit
        } else {
            break_at(&graphemes, start, limit)
        };
        out.push(byte_at(start)..byte_at(end));
        start = end;
    }
}

/// Mirrors the transcript's wrap: just after the last space, unless the row is
/// only indentation so far, else a hard cut at `limit`.
fn break_at(graphemes: &[(usize, &str)], start: usize, limit: usize) -> usize {
    let Some(content) = (start..limit).find(|index| graphemes[*index].1 != " ") else {
        return limit;
    };
    (content..limit)
        .rev()
        .find(|index| graphemes[*index].1 == " ")
        .map(|index| index + 1)
        .unwrap_or(limit)
}

/// A cursor on a soft wrap belongs to the start of the next row, which is the
/// last row starting at or before it.
fn cursor_row_col_in(text: &str, rows: &[Range<usize>], cursor: usize) -> (usize, usize) {
    let row = rows
        .iter()
        .rposition(|row| row.start <= cursor)
        .unwrap_or(0);
    let start = rows.get(row).map(|row| row.start).unwrap_or(0);
    (row, text_width(&text[start..cursor]))
}

fn text_width(text: &str) -> usize {
    text.graphemes(true).map(grapheme_width).sum()
}

/// The last place on `row` whose column is no further right than `goal`. The
/// end of a soft wrapped row is the start of the next, so it isn't a candidate.
fn position_in_row(text: &str, rows: &[Range<usize>], row: usize, goal: usize) -> usize {
    let range = rows[row].clone();
    let hard_end = range.end == text.len() || text[range.end..].starts_with('\n');

    let mut best = range.start;
    let mut col = 0;
    for (byte, grapheme) in text[range.clone()].grapheme_indices(true) {
        if col > goal {
            return best;
        }
        best = range.start + byte;
        col += grapheme_width(grapheme);
    }
    if hard_end && col <= goal {
        best = range.end;
    }
    best
}

/// The first row to show so the cursor's row is inside `max_visible` rows,
/// moving `top_row` as little as possible.
pub fn scroll_to_cursor(
    top_row: usize,
    cursor_row: usize,
    total_rows: usize,
    max_visible: usize,
) -> usize {
    let visible = total_rows.clamp(1, max_visible.max(1));
    let mut top = top_row.min(total_rows.saturating_sub(visible));
    if cursor_row < top {
        top = cursor_row;
    } else if cursor_row >= top + visible {
        top = cursor_row + 1 - visible;
    }
    top
}

fn next_boundary(text: &str, cursor: usize) -> Option<usize> {
    text.grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .find(|byte| *byte > cursor)
}

fn previous_boundary(text: &str, cursor: usize) -> Option<usize> {
    text.grapheme_indices(true)
        .map(|(byte, _)| byte)
        .take_while(|byte| *byte < cursor)
        .last()
}
