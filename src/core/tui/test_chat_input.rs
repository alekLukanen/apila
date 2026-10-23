use super::app::TAB_WIDTH;
use super::chat_input::{rows, scroll_to_cursor, ChatInput};

/// An input holding `text` with the cursor at its end, as if it was typed.
fn input(text: &str) -> ChatInput {
    let mut input = ChatInput::default();
    input.insert_str(text);
    input
}

fn wrapped(text: &str, width: usize) -> Vec<&str> {
    rows(text, width)
        .into_iter()
        .map(|range| &text[range])
        .collect()
}

// Wrapping //////////////////////////
//////////////////////////////////////

#[test]
fn a_row_breaks_after_the_last_space() {
    assert_eq!(wrapped("hello world", 8), vec!["hello ", "world"]);
}

#[test]
fn a_word_longer_than_the_row_is_cut() {
    assert_eq!(wrapped("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
}

#[test]
fn a_newline_starts_a_row() {
    assert_eq!(wrapped("one\ntwo", 20), vec!["one", "two"]);
}

#[test]
fn blank_lines_are_rows_of_their_own() {
    assert_eq!(wrapped("one\n\ntwo", 20), vec!["one", "", "two"]);
    assert_eq!(wrapped("a\n", 20), vec!["a", ""]);
    assert_eq!(wrapped("", 20), vec![""]);
}

#[test]
fn a_zero_width_wraps_as_one_column() {
    assert_eq!(wrapped("abc", 0), vec!["a", "b", "c"]);
}

#[test]
fn rows_are_measured_in_columns_rather_than_characters() {
    assert_eq!(wrapped("日本語", 4), vec!["日本", "語"]);
}

#[test]
fn a_wide_character_is_never_split() {
    assert_eq!(wrapped("日本語", 3), vec!["日", "本", "語"]);
}

/// A row always takes something, or a character wider than the row would
/// never be placed.
#[test]
fn a_character_wider_than_the_row_still_finishes() {
    assert_eq!(wrapped("日本", 1), vec!["日", "本"]);
}

#[test]
fn the_rows_cover_the_text_apart_from_its_newlines() {
    let text = "first line wraps here\n\nsecond\tline 日本 end";

    for width in 1..12 {
        let ranges = rows(text, width);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges.last().expect("a row").end, text.len());
        for pair in ranges.windows(2) {
            let gap = &text[pair[0].end..pair[1].start];
            assert!(gap.is_empty() || gap == "\n", "width {}: {:?}", width, gap);
        }
    }
}

#[test]
fn a_tab_takes_a_fixed_number_of_columns() {
    assert_eq!(wrapped("\tab", TAB_WIDTH + 1), vec!["\ta", "b"]);
    assert_eq!(input("\t").cursor_row_col(usize::MAX), (0, TAB_WIDTH));
}

// Editing ///////////////////////////
//////////////////////////////////////

#[test]
fn typing_after_moving_left_inserts_in_the_middle() {
    let mut input = input("ac");
    input.move_left();
    input.insert_char('b');

    assert_eq!(input.text(), "abc");
    assert_eq!(input.cursor(), 2);
}

#[test]
fn an_accented_letter_is_one_step_however_it_is_encoded() {
    for accented in ["é", "e\u{301}"] {
        let mut typed = input(&format!("a{}", accented));
        typed.move_left();
        assert_eq!(typed.cursor(), 1, "{:?}", accented);

        let mut typed = input(&format!("a{}", accented));
        typed.backspace();
        assert_eq!(typed.text(), "a", "{:?}", accented);

        let mut typed = input(&format!("a{}", accented));
        typed.home();
        typed.move_right();
        typed.delete();
        assert_eq!(typed.text(), "a", "{:?}", accented);
    }
}

/// Typing in front of a combining mark leaves the cursor inside the grapheme
/// they now form.
#[test]
fn a_cursor_left_inside_a_grapheme_still_moves_and_deletes() {
    let mut typed = input("\u{301}");
    typed.home();
    typed.insert_char('e');
    assert_eq!(typed.text(), "e\u{301}");

    typed.move_right();
    assert_eq!(typed.cursor(), typed.text().len());

    let mut typed = input("\u{301}");
    typed.home();
    typed.insert_char('e');
    typed.backspace();
    assert_eq!(typed.text(), "\u{301}");
    assert_eq!(typed.cursor(), 0);
}

#[test]
fn edits_at_either_end_do_nothing() {
    let mut empty = ChatInput::default();
    empty.backspace();
    empty.delete();
    empty.move_left();
    empty.move_right();
    assert_eq!(empty.text(), "");
    assert_eq!(empty.cursor(), 0);

    let mut typed = input("ab");
    typed.delete();
    typed.move_right();
    assert_eq!((typed.text(), typed.cursor()), ("ab", 2));

    typed.home();
    typed.backspace();
    typed.move_left();
    assert_eq!((typed.text(), typed.cursor()), ("ab", 0));
}

#[test]
fn control_characters_are_not_typed() {
    let mut typed = input("a");
    typed.insert_char('\u{7}');
    typed.insert_char('\t');

    assert_eq!(typed.text(), "a");
}

// Up and Down ///////////////////////
//////////////////////////////////////

#[test]
fn up_keeps_the_column() {
    let mut typed = input("abcd\nefgh");
    typed.move_left();
    typed.move_left();
    typed.move_up(usize::MAX);

    assert_eq!(typed.cursor(), 2);
}

/// A short row in between doesn't drag the cursor left for good.
#[test]
fn the_column_is_remembered_across_a_short_row() {
    let mut typed = input("abcdef\nab\nabcdef");

    typed.move_up(usize::MAX);
    assert_eq!(typed.cursor(), "abcdef\nab".len());
    typed.move_up(usize::MAX);
    assert_eq!(typed.cursor(), "abcdef".len());
}

#[test]
fn up_on_the_first_row_and_down_on_the_last_do_nothing() {
    let mut typed = input("abc\ndef");
    typed.move_down(usize::MAX);
    assert_eq!(typed.cursor(), 7);

    typed.move_up(usize::MAX);
    typed.move_left();
    typed.move_up(usize::MAX);
    assert_eq!(typed.cursor(), 2);
}

#[test]
fn up_and_down_never_land_inside_a_wide_character() {
    let mut typed = input("ab\n日本");
    typed.move_up(usize::MAX);
    typed.home();
    typed.move_right();
    typed.move_down(usize::MAX);

    // column one is the middle of 日, so the cursor goes in front of it
    assert_eq!(typed.cursor(), "ab\n".len());
}

/// The end of a soft wrapped row is the start of the next, so stopping there
/// would put the cursor back on the row it left.
#[test]
fn up_never_stops_at_the_end_of_a_soft_wrapped_row() {
    let mut typed = input("abcdefgh");
    typed.move_up(4);

    assert_eq!(typed.cursor(), 3);
    assert_eq!(typed.cursor_row_col(4), (0, 3));
}

#[test]
fn home_and_end_go_to_the_ends_of_the_line_not_the_row() {
    let mut typed = input("abcd efgh\nxyz");
    typed.move_up(5);
    assert_eq!(typed.cursor_row_col(5), (1, 3));

    typed.home();
    assert_eq!(typed.cursor(), 0);
    typed.end();
    assert_eq!(typed.cursor(), "abcd efgh".len());
}

#[test]
fn a_cursor_on_a_soft_wrap_is_at_the_start_of_the_next_row() {
    let mut typed = input("abcdefgh");
    for _ in 0..4 {
        typed.move_left();
    }

    assert_eq!(typed.cursor(), 4);
    assert_eq!(typed.cursor_row_col(4), (1, 0));
}

// Window ////////////////////////////
//////////////////////////////////////

#[test]
fn the_window_moves_only_as_far_as_the_cursor_needs() {
    assert_eq!(scroll_to_cursor(0, 0, 1, 3), 0);
    assert_eq!(scroll_to_cursor(0, 4, 5, 3), 2);
    assert_eq!(scroll_to_cursor(1, 2, 5, 3), 1);
    assert_eq!(scroll_to_cursor(2, 0, 5, 3), 0);
    // fewer rows than a stale top
    assert_eq!(scroll_to_cursor(5, 1, 2, 3), 0);
}

#[test]
fn the_window_shows_the_rows_around_the_cursor() {
    let mut typed = input("1\n2\n3\n4\n5");

    let window = typed.window(10, 3);
    assert_eq!(window.lines, vec!["3", "4", "5"]);
    assert_eq!(window.cursor_row, 2);
    assert!(window.hidden_above);
    assert!(!window.hidden_below);

    for _ in 0..4 {
        typed.move_up(10);
    }
    let window = typed.window(10, 3);
    assert_eq!(window.lines, vec!["1", "2", "3"]);
    assert_eq!(typed.top_row(), 0);
    assert!(!window.hidden_above);
    assert!(window.hidden_below);
}

#[test]
fn tabs_are_drawn_as_spaces() {
    let window = input("a\tb").window(40, 3);

    assert_eq!(window.lines, vec![format!("a{}b", " ".repeat(TAB_WIDTH))]);
    assert_eq!(window.cursor_col, TAB_WIDTH + 2);
}

// Paste /////////////////////////////
//////////////////////////////////////

#[test]
fn pasted_line_endings_become_newlines() {
    assert_eq!(input("a\r\nb\rc").text(), "a\nb\nc");
}

#[test]
fn pasted_tabs_are_kept() {
    assert_eq!(input("a\tb").text(), "a\tb");
}

#[test]
fn pasted_control_characters_are_dropped() {
    assert_eq!(input("a\u{7}b\u{1b}c").text(), "abc");
}

#[test]
fn the_cursor_ends_up_after_what_was_pasted() {
    let mut typed = input("xy");
    typed.move_left();
    typed.insert_str("12");

    assert_eq!(typed.text(), "x12y");
    assert_eq!(typed.cursor(), 3);
}
