use super::scroll::{page_lines, scrolled, visible_top, ChatScroll};

#[test]
fn scrolling_up_from_the_bottom_holds_a_line() {
    assert_eq!(scrolled(ChatScroll::Follow, -3, 20), ChatScroll::Top(17));
}

#[test]
fn scrolling_up_stops_at_the_first_line() {
    assert_eq!(scrolled(ChatScroll::Top(2), -3, 20), ChatScroll::Top(0));
    assert_eq!(scrolled(ChatScroll::Top(0), -3, 20), ChatScroll::Top(0));
}

#[test]
fn scrolling_back_down_to_the_bottom_follows_again() {
    assert_eq!(scrolled(ChatScroll::Top(17), 3, 20), ChatScroll::Follow);
    assert_eq!(scrolled(ChatScroll::Top(19), 3, 20), ChatScroll::Follow);
    assert_eq!(scrolled(ChatScroll::Follow, 3, 20), ChatScroll::Follow);
}

/// The transcript can shrink under a held position, after a clear for one.
#[test]
fn a_position_past_the_end_is_pulled_back() {
    assert_eq!(visible_top(ChatScroll::Top(30), 20), 20);
    assert_eq!(scrolled(ChatScroll::Top(30), -3, 20), ChatScroll::Top(17));
}

#[test]
fn a_transcript_that_fits_never_scrolls() {
    assert_eq!(scrolled(ChatScroll::Follow, -3, 0), ChatScroll::Follow);
    assert_eq!(scrolled(ChatScroll::Follow, 3, 0), ChatScroll::Follow);
    assert_eq!(visible_top(ChatScroll::Follow, 0), 0);
}

#[test]
fn a_page_keeps_a_line_of_overlap() {
    assert_eq!(page_lines(0), 1);
    assert_eq!(page_lines(1), 1);
    assert_eq!(page_lines(20), 19);
}
