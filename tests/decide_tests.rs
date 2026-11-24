//! Pure unit tests for `decide()` and split-chapter grouping (`detect.rs`, §8.3).
#![allow(clippy::assert_is_empty, clippy::too_many_lines)]

use besideme::detect::{Decision, ParsedChapterNumber, Policy, decide, parse_chapter_number};
use besideme::model::{ChapterRef, SeriesSnapshot};
use besideme::sites::{MangaUp, Site};
use besideme::state::SeriesState;

const FIXTURE_BASE: &str = include_str!("fixtures/manga261_base.html");
const FIXTURE_NEW: &str = include_str!("fixtures/manga261_new.html");
const FIXTURE_HEAD_REMOVED: &str = include_str!("fixtures/manga261_head_removed.html");

fn make_chapter(id: &str, label: &str, published: Option<&str>) -> ChapterRef {
    ChapterRef {
        id: id.to_string(),
        label: label.to_string(),
        subtitle: Some("Smoke".to_string()),
        url: format!("https://global.manga-up.com/manga/261/{id}"),
        thumbnail: None,
        published: published.map(ToString::to_string),
        early_access: published.is_none(),
    }
}

fn make_snapshot(chapters_newest_first: Vec<ChapterRef>) -> SeriesSnapshot {
    SeriesSnapshot {
        title: "Smoking Behind the Supermarket with You".to_string(),
        chapters_newest_first,
    }
}

fn state_from_snapshot(snap: &SeriesSnapshot) -> SeriesState {
    SeriesState {
        announced_ids: snap
            .chapters_newest_first
            .iter()
            .rev()
            .map(|c| c.id.clone())
            .collect(),
        last_announced_label: snap.chapters_newest_first.first().map(|c| c.label.clone()),
        last_snapshot_len: snap.chapters_newest_first.len(),
        ..SeriesState::default()
    }
}

#[test]
fn uninitialized_establishes_baseline_and_identical_returns_no_change() {
    let site = MangaUp::default();
    let snap = site.parse("261", FIXTURE_BASE).unwrap();
    let policy = Policy::default();

    let decision = decide(None, &snap, &policy);
    match decision {
        Decision::Baseline { ids, head_label } => {
            assert_eq!(ids.len(), 5);
            assert_eq!(head_label, "Chapter 67.2");
            assert!(ids.contains(&"130419".to_string()));
            assert!(ids.contains(&"28665".to_string()));
        }
        other => panic!("expected Baseline, got {other:?}"),
    }

    let state = state_from_snapshot(&snap);
    assert_eq!(decide(Some(&state), &snap, &policy), Decision::NoChange);
}

#[test]
fn one_new_and_three_new_chapters_ordered_oldest_first() {
    let site = MangaUp::default();
    let base_snap = site.parse("261", FIXTURE_BASE).unwrap();
    let new_snap = site.parse("261", FIXTURE_NEW).unwrap();
    let state = state_from_snapshot(&base_snap);

    // 1 new chapter (67.3)
    let dec = decide(Some(&state), &new_snap, &Policy::default());
    match dec {
        Decision::Announce {
            groups,
            folded,
            held_back,
        } => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].number, Some(67));
            assert_eq!(groups[0].parts.len(), 1);
            assert_eq!(groups[0].parts[0].id, "130422");
            assert!(folded.is_empty());
            assert_eq!(held_back, 0);
        }
        other => panic!("expected Announce, got {other:?}"),
    }

    // 3 new distinct chapters (68, 69, 70), newest first in snapshot -> announced oldest first (68, 69, 70)
    let mut snap3 = base_snap.clone();
    snap3
        .chapters_newest_first
        .insert(0, make_chapter("2001", "Chapter 68", Some("Dec 1")));
    snap3
        .chapters_newest_first
        .insert(0, make_chapter("2002", "Chapter 69", Some("Dec 2")));
    snap3
        .chapters_newest_first
        .insert(0, make_chapter("2003", "Chapter 70", Some("Dec 3")));

    match decide(Some(&state), &snap3, &Policy::default()) {
        Decision::Announce { groups, folded, .. } => {
            assert_eq!(groups.len(), 3);
            assert_eq!(groups[0].number, Some(68));
            assert_eq!(groups[1].number, Some(69));
            assert_eq!(groups[2].number, Some(70));
            assert!(folded.is_empty());
        }
        other => panic!("expected Announce, got {other:?}"),
    }
}

#[test]
fn head_removed_and_reordered_list_produce_no_change() {
    let site = MangaUp::default();
    let base_snap = site.parse("261", FIXTURE_BASE).unwrap();
    let removed_snap = site.parse("261", FIXTURE_HEAD_REMOVED).unwrap();
    let state = state_from_snapshot(&base_snap);

    // Head removed -> NoChange (never falsely announces old chapter 67.1!)
    assert_eq!(
        decide(Some(&state), &removed_snap, &Policy::default()),
        Decision::NoChange
    );

    // Reordered list -> NoChange
    let mut reordered = base_snap.clone();
    reordered.chapters_newest_first.reverse();
    assert_eq!(
        decide(Some(&state), &reordered, &Policy::default()),
        Decision::NoChange
    );
}

#[test]
fn require_published_holds_back_undated_chapter_until_date_appears() {
    let base_snap = make_snapshot(vec![
        make_chapter("101", "Chapter 10", Some("Nov 1, 2026")),
        make_chapter("100", "Chapter 9", Some("Oct 1, 2026")),
    ]);
    let state = state_from_snapshot(&base_snap);
    let policy = Policy {
        require_published: true,
        max_messages_per_cycle: 3,
        announce_on_first_run: false,
    };

    // Undated Chapter 11 arrives -> held back
    let undated_snap = make_snapshot(vec![
        make_chapter("102", "Chapter 11", None),
        make_chapter("101", "Chapter 10", Some("Nov 1, 2026")),
        make_chapter("100", "Chapter 9", Some("Oct 1, 2026")),
    ]);
    match decide(Some(&state), &undated_snap, &policy) {
        Decision::Announce {
            groups,
            folded,
            held_back,
        } => {
            assert!(groups.is_empty());
            assert!(folded.is_empty());
            assert_eq!(held_back, 1);
        }
        other => panic!("expected held-back Announce, got {other:?}"),
    }

    // Later, Chapter 11 gains a publication date -> announced once!
    let dated_snap = make_snapshot(vec![
        make_chapter("102", "Chapter 11", Some("Dec 1, 2026")),
        make_chapter("101", "Chapter 10", Some("Nov 1, 2026")),
        make_chapter("100", "Chapter 9", Some("Oct 1, 2026")),
    ]);
    match decide(Some(&state), &dated_snap, &policy) {
        Decision::Announce {
            groups, held_back, ..
        } => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].parts[0].id, "102");
            assert_eq!(held_back, 0);
        }
        other => panic!("expected Announce, got {other:?}"),
    }
}

#[test]
fn sanity_guard_detects_25_new_and_half_shrunk_list_as_anomalies() {
    let mut base_chapters = Vec::new();
    for i in (1..=20).rev() {
        base_chapters.push(make_chapter(
            &format!("old_{i}"),
            &format!("Chapter {i}"),
            Some("Jan 1"),
        ));
    }
    let base_snap = make_snapshot(base_chapters.clone());
    let state = state_from_snapshot(&base_snap);

    // 25 new chapters (> 20) -> Anomaly
    let mut flood_chapters = Vec::new();
    for i in (21..=45).rev() {
        flood_chapters.push(make_chapter(
            &format!("new_{i}"),
            &format!("Chapter {i}"),
            Some("Jan 2"),
        ));
    }
    flood_chapters.extend(base_chapters.clone());
    let flood_snap = make_snapshot(flood_chapters);
    assert!(matches!(
        decide(Some(&state), &flood_snap, &Policy::default()),
        Decision::Anomaly(_)
    ));

    // List shrinks by more than half (from 20 to 8, where 8 < 20 / 2) -> Anomaly
    let shrunk_snap = make_snapshot(base_chapters.into_iter().take(8).collect());
    assert!(matches!(
        decide(Some(&state), &shrunk_snap, &Policy::default()),
        Decision::Anomaly(_)
    ));
}

#[test]
fn announce_on_first_run_announces_only_newest_chapter() {
    let site = MangaUp::default();
    let snap = site.parse("261", FIXTURE_BASE).unwrap();
    let policy = Policy {
        require_published: false,
        max_messages_per_cycle: 3,
        announce_on_first_run: true,
    };

    match decide(None, &snap, &policy) {
        Decision::Announce {
            groups,
            folded,
            held_back,
        } => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].parts.len(), 1);
            assert_eq!(groups[0].parts[0].id, "130419");
            assert_eq!(groups[0].parts[0].label, "Chapter 67.2");
            assert!(folded.is_empty());
            assert_eq!(held_back, 0);
        }
        other => panic!("expected Announce of head chapter, got {other:?}"),
    }
}

#[test]
fn split_chapter_grouping_and_folding_rules() {
    let base_snap = make_snapshot(vec![make_chapter("1", "Chapter 66", Some("Nov 1"))]);
    let state = state_from_snapshot(&base_snap);

    // 1. 67.1, 67.2, 67.3 -> 1 group of 3 (oldest first: 67.1, 67.2, 67.3)
    let snap_67 = make_snapshot(vec![
        make_chapter("67_3", "Chapter 67.3", None),
        make_chapter("67_2", "Chapter 67.2", None),
        make_chapter("67_1", "Chapter 67.1", None),
        make_chapter("1", "Chapter 66", Some("Nov 1")),
    ]);
    match decide(Some(&state), &snap_67, &Policy::default()) {
        Decision::Announce { groups, .. } => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].number, Some(67));
            let ids: Vec<&str> = groups[0].parts.iter().map(|p| p.id.as_str()).collect();
            assert_eq!(ids, vec!["67_1", "67_2", "67_3"]);
        }
        other => panic!("unexpected {other:?}"),
    }

    // 2. 67.1-67.3 + 68.1-68.3 -> 2 groups, 67 first then 68
    let snap_67_68 = make_snapshot(vec![
        make_chapter("68_3", "Chapter 68.3", None),
        make_chapter("68_2", "Chapter 68.2", None),
        make_chapter("68_1", "Chapter 68.1", None),
        make_chapter("67_3", "Chapter 67.3", None),
        make_chapter("67_2", "Chapter 67.2", None),
        make_chapter("67_1", "Chapter 67.1", None),
        make_chapter("1", "Chapter 66", Some("Nov 1")),
    ]);
    match decide(Some(&state), &snap_67_68, &Policy::default()) {
        Decision::Announce { groups, .. } => {
            assert_eq!(groups.len(), 2);
            assert_eq!(groups[0].number, Some(67));
            assert_eq!(groups[0].parts.len(), 3);
            assert_eq!(groups[1].number, Some(68));
            assert_eq!(groups[1].parts.len(), 3);
        }
        other => panic!("unexpected {other:?}"),
    }

    // 3. 68 (no part) + 67.2 -> 2 groups; and unnumbered "Extra" -> its own group
    let snap_mixed = make_snapshot(vec![
        make_chapter("ex2", "Special Illustration", None),
        make_chapter("ex1", "Extra", None),
        make_chapter("68_0", "Chapter 68", None),
        make_chapter("67_2", "Chapter 67.2", None),
        make_chapter("1", "Chapter 66", Some("Nov 1")),
    ]);
    match decide(
        Some(&state),
        &snap_mixed,
        &Policy {
            max_messages_per_cycle: 10,
            ..Policy::default()
        },
    ) {
        Decision::Announce { groups, .. } => {
            assert_eq!(groups.len(), 4);
            assert_eq!(groups[0].number, Some(67));
            assert_eq!(groups[1].number, Some(68));
            assert_eq!(groups[2].number, None);
            assert_eq!(groups[2].parts[0].label, "Extra");
            assert_eq!(groups[3].number, None);
            assert_eq!(groups[3].parts[0].label, "Special Illustration");
        }
        other => panic!("unexpected {other:?}"),
    }

    // 4. 12 parts of one chapter -> split into 2 messages (10 + 2)
    let mut twelve_parts = Vec::new();
    for p in (1..=12).rev() {
        twelve_parts.push(make_chapter(
            &format!("67_{p}"),
            &format!("Chapter 67.{p}"),
            None,
        ));
    }
    twelve_parts.push(make_chapter("1", "Chapter 66", Some("Nov 1")));
    let snap_12 = make_snapshot(twelve_parts);
    match decide(Some(&state), &snap_12, &Policy::default()) {
        Decision::Announce { groups, .. } => {
            assert_eq!(groups.len(), 2);
            assert_eq!(groups[0].parts.len(), 10);
            assert_eq!(groups[0].parts[0].id, "67_1");
            assert_eq!(groups[0].parts[9].id, "67_10");
            assert_eq!(groups[1].parts.len(), 2);
            assert_eq!(groups[1].parts[0].id, "67_11");
            assert_eq!(groups[1].parts[1].id, "67_12");
        }
        other => panic!("unexpected {other:?}"),
    }

    // 5. 5 groups with max_messages_per_cycle = 3 -> newest 3 groups sent, older 2 folded
    let snap_5_groups = make_snapshot(vec![
        make_chapter("71_1", "Chapter 71", None),
        make_chapter("70_1", "Chapter 70", None),
        make_chapter("69_1", "Chapter 69", None),
        make_chapter("68_2", "Chapter 68.2", None),
        make_chapter("68_1", "Chapter 68.1", None),
        make_chapter("67_1", "Chapter 67.1", None),
        make_chapter("1", "Chapter 66", Some("Nov 1")),
    ]);
    match decide(
        Some(&state),
        &snap_5_groups,
        &Policy {
            max_messages_per_cycle: 3,
            ..Policy::default()
        },
    ) {
        Decision::Announce { groups, folded, .. } => {
            assert_eq!(groups.len(), 3);
            assert_eq!(groups[0].number, Some(69));
            assert_eq!(groups[1].number, Some(70));
            assert_eq!(groups[2].number, Some(71));
            // Folded has 67.1 (1 part) + 68.1, 68.2 (2 parts) = 3 part IDs
            assert_eq!(folded, vec!["67_1", "68_1", "68_2"]);
        }
        other => panic!("unexpected {other:?}"),
    }

    // Also verify 5 new chapters with cap = 1 -> newest 1 sent, 4 folded
    match decide(
        Some(&state),
        &snap_5_groups,
        &Policy {
            max_messages_per_cycle: 1,
            ..Policy::default()
        },
    ) {
        Decision::Announce { groups, folded, .. } => {
            assert_eq!(groups.len(), 1);
            assert_eq!(groups[0].number, Some(71));
            assert_eq!(folded.len(), 5);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn label_number_parsing_matches_spec_cases() {
    assert_eq!(
        parse_chapter_number("Chapter 67.3"),
        Some(ParsedChapterNumber {
            chapter: 67,
            part: Some(3)
        })
    );
    assert_eq!(
        parse_chapter_number("Chapter 68"),
        Some(ParsedChapterNumber {
            chapter: 68,
            part: None
        })
    );
    assert_eq!(
        parse_chapter_number("Ch. 5.1"),
        Some(ParsedChapterNumber {
            chapter: 5,
            part: Some(1)
        })
    );
    assert_eq!(parse_chapter_number("Extra"), None);
    assert_eq!(parse_chapter_number("Special Bonus"), None);
}
