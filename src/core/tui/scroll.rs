/// How far one tick of the mouse wheel moves the transcript.
pub const WHEEL_LINES: usize = 3;

/// Where an agent's transcript is scrolled to. `Follow` keeps the newest line
/// on screen as the agent writes; `Top` holds a line in place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatScroll {
    #[default]
    Follow,
    Top(usize),
}

/// `state` moved by `delta` lines. Reaching the bottom follows again, so new
/// output is not missed after scrolling back down.
pub fn scrolled(state: ChatScroll, delta: isize, max_top: usize) -> ChatScroll {
    let top = visible_top(state, max_top);
    let next = if delta < 0 {
        top.saturating_sub(delta.unsigned_abs())
    } else {
        top.saturating_add(delta as usize).min(max_top)
    };
    if next >= max_top {
        ChatScroll::Follow
    } else {
        ChatScroll::Top(next)
    }
}

/// The first transcript line on screen. A `Top` left past the end by a
/// shrinking transcript is pulled back to the bottom.
pub fn visible_top(state: ChatScroll, max_top: usize) -> usize {
    match state {
        ChatScroll::Follow => max_top,
        ChatScroll::Top(top) => top.min(max_top),
    }
}

/// One page keeps a line of overlap so the reader doesn't lose their place.
pub fn page_lines(height: usize) -> usize {
    height.saturating_sub(1).max(1)
}
