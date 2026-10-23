use chrono::{FixedOffset, Local, TimeZone};
use ratatui::layout::Rect;

use crate::core::memory::recorder::{JobState, MemoryJob, MemoryView};
use crate::core::memory::store::{SkillListing, SkillsSaved};
use crate::core::openrouter::types::Spend;

use super::memories::{
    body_lines, fit_width, format_absolute, list_rows, relative_label, reload_stamp, updated_label,
    MemoriesView, PendingDelete, ReaderView,
};

const NOW: i64 = 1_800_000_000;

fn listing(id: i64, name: &str) -> SkillListing {
    SkillListing {
        id,
        name: name.into(),
        when_to_use: format!("when {} is needed", name),
        body: format!("## Steps\n{}", name),
        updated_at: NOW - 120,
    }
}

fn view(ids: &[i64]) -> MemoriesView {
    let skills = ids
        .iter()
        .map(|id| listing(*id, &format!("skill-{}", id)))
        .collect();
    MemoriesView::new("builder".into(), skills, (Some(ids.len()), None))
}

fn view_with_nothing() -> MemoriesView {
    MemoriesView::new("builder".into(), Vec::new(), (Some(0), None))
}

fn selected_id(view: &MemoriesView) -> Option<i64> {
    view.selected().map(|skill| skill.id)
}

fn reader(max_top: usize) -> Option<ReaderView> {
    Some(ReaderView {
        area: Rect::new(0, 0, 40, 10),
        max_top,
        height: 10,
    })
}

#[test]
fn a_list_row_says_how_long_ago_it_was_updated() {
    assert_eq!(relative_label(NOW, NOW), "just now");
    assert_eq!(relative_label(NOW - 59, NOW), "just now");
    assert_eq!(relative_label(NOW - 60, NOW), "1m ago");
    assert_eq!(relative_label(NOW - 3_599, NOW), "59m ago");
    assert_eq!(relative_label(NOW - 3_600, NOW), "1h ago");
    assert_eq!(relative_label(NOW - 86_399, NOW), "23h ago");
    assert_eq!(relative_label(NOW - 86_400, NOW), "1d ago");
    assert_eq!(relative_label(NOW - 29 * 86_400, NOW), "29d ago");
    // a clock that moved back still reads sensibly
    assert_eq!(relative_label(NOW + 500, NOW), "just now");
}

#[test]
fn a_month_old_row_shows_its_date_and_no_time_shows_a_dash() {
    let old = NOW - 30 * 86_400;
    let expected = Local
        .timestamp_opt(old, 0)
        .earliest()
        .expect("a local time")
        .format("%Y-%m-%d")
        .to_string();

    assert_eq!(relative_label(old, NOW), expected);
    assert_eq!(relative_label(0, NOW), "—");
    assert_eq!(updated_label(0), "—");
    assert_eq!(updated_label(-5), "—");
}

#[test]
fn the_updated_time_is_shown_in_local_time() {
    let offset = FixedOffset::east_opt(5 * 3_600 + 30 * 60).expect("an offset");
    let time = offset.timestamp_opt(0, 0).single().expect("a time");
    assert_eq!(format_absolute(time), "1970-01-01 05:30");

    let local = Local
        .timestamp_opt(NOW, 0)
        .earliest()
        .expect("a local time")
        .fixed_offset();
    assert_eq!(updated_label(NOW), format_absolute(local));
}

#[test]
fn the_reader_shows_when_it_was_updated_and_the_rendered_body() {
    let lines: Vec<String> = body_lines(&listing(1, "fetch"), 30)
        .iter()
        .map(|line| line.to_string())
        .collect();

    assert_eq!(lines[0], format!("Updated {}", updated_label(NOW - 120)));
    assert_eq!(lines[1], "when fetch is needed");
    assert_eq!(lines[2], "─".repeat(30));
    assert_eq!(lines[3], "Steps");
    assert!(lines.contains(&"fetch".to_string()), "{lines:?}");
}

#[test]
fn list_rows_keep_the_age_right_aligned_within_the_width() {
    let mut wide = listing(1, "漢字の名前がとても長いスキル");
    wide.when_to_use = "line one\nline two 😀 goes on and on and on".into();
    for width in [8, 12, 20, 40] {
        let rows = list_rows(&[wide.clone(), listing(2, "fetch")], width, NOW);
        for [name, detail] in &rows {
            assert!(name.width() <= width, "{name:?} at {width}");
            assert_eq!(detail.width(), width, "{detail:?} at {width}");
            assert!(detail.to_string().ends_with("2m ago"), "{detail:?}");
            assert!(!detail.to_string().contains('\n'));
        }
    }
    // too narrow for anything but the age
    let rows = list_rows(&[listing(1, "fetch")], 4, NOW);
    assert!(rows[0][1].width() <= 4);
}

#[test]
fn fitting_cuts_by_display_width() {
    assert_eq!(fit_width("short", 10), "short");
    assert_eq!(fit_width("a\nb", 10), "a b");
    assert_eq!(fit_width("abcdef", 4), "abc…");
    // a wide character that would cross the edge is left out whole
    assert_eq!(fit_width("ab漢字", 4), "ab…");
    assert_eq!(fit_width("abc", 0), "");
}

#[test]
fn a_reload_keeps_the_selected_skill_or_its_row() {
    let mut view = view(&[1, 2, 3]);
    view.select_offset(1);
    assert_eq!(selected_id(&view), Some(2));

    // a new skill on top moves the row but not the selection
    view.reload(
        vec![
            listing(4, "new"),
            listing(1, "a"),
            listing(2, "b"),
            listing(3, "c"),
        ],
        (Some(4), None),
    );
    assert_eq!(selected_id(&view), Some(2));

    // the selected skill deleted: the row it was on is kept
    view.reload(
        vec![listing(4, "new"), listing(1, "a"), listing(3, "c")],
        (Some(3), None),
    );
    assert_eq!(selected_id(&view), Some(3));

    // the last row deleted: the selection moves up
    view.reload(vec![listing(4, "new"), listing(1, "a")], (Some(2), None));
    assert_eq!(selected_id(&view), Some(1));

    view.reload(Vec::new(), (Some(0), None));
    assert_eq!(selected_id(&view), None);
}

#[test]
fn selection_and_scrolling_are_clamped() {
    let mut view = view(&[1, 2]);
    view.select_offset(-1);
    assert_eq!(selected_id(&view), Some(1));
    view.select_offset(5);
    assert_eq!(selected_id(&view), Some(2));

    view.set_reader(reader(4));
    view.scroll_by(3);
    assert_eq!(view.top(), 3);
    view.scroll_by(10);
    assert_eq!(view.top(), 4);
    view.scroll_by(-10);
    assert_eq!(view.top(), 0);

    // another skill opens at its first line
    view.scroll_by(2);
    view.select_offset(-1);
    assert_eq!(view.top(), 0);

    // a reader that got shorter pulls the position back
    view.scroll_by(4);
    view.set_reader(reader(1));
    assert_eq!(view.top(), 1);
}

#[test]
fn a_delete_is_armed_then_taken_once() {
    let mut view = view(&[7]);
    assert!(view.arm_delete());
    let expected = PendingDelete {
        agent_id: "builder".into(),
        skill_id: 7,
        name: "skill-7".into(),
    };
    assert_eq!(view.confirm_delete(), Some(&expected));
    assert_eq!(view.take_confirmed(), Some(expected));
    assert_eq!(view.take_confirmed(), None);

    // a reload clears what was armed against the old list
    view.arm_delete();
    view.reload(vec![listing(7, "skill-7")], (Some(1), None));
    assert_eq!(view.confirm_delete(), None);

    let mut empty = view_with_nothing();
    assert!(!empty.arm_delete());
}

#[test]
fn a_reload_is_needed_only_when_the_stamp_moves() {
    let job = |state| MemoryJob {
        session_id: 3,
        state,
        spend: Spend::default(),
    };
    let mut memory = MemoryView {
        skills: Some(2),
        last_job: Some(job(JobState::Running)),
        ..MemoryView::default()
    };
    let view = MemoriesView::new("builder".into(), Vec::new(), reload_stamp(&memory));
    assert!(!view.needs_reload(&reload_stamp(&memory)));

    // a job that only updated a skill leaves the count alone
    memory.last_job = Some(job(JobState::Done {
        skills: SkillsSaved {
            added: 0,
            updated: 1,
        },
    }));
    assert!(view.needs_reload(&reload_stamp(&memory)));

    let counted = MemoryView {
        skills: Some(3),
        last_job: Some(job(JobState::Running)),
        ..MemoryView::default()
    };
    assert!(view.needs_reload(&reload_stamp(&counted)));
}

#[test]
fn the_rendered_body_is_kept_until_the_width_changes() {
    let mut view = view(&[1]);
    let narrow = view.reader_lines(10).to_vec();
    assert_eq!(view.reader_lines(10), narrow.as_slice());
    assert_ne!(view.reader_lines(30), narrow.as_slice());
}
