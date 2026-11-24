//! Pure chapter detection and split-chapter grouping logic (`decide()`).
//!
//! This module performs no I/O, network calls, or filesystem operations.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex_lite::Regex;

use crate::model::{ChapterRef, SeriesSnapshot};
use crate::state::SeriesState;

/// Maximum number of embeds Discord allows in a single webhook message.
pub const MAX_PARTS_PER_GROUP: usize = 10;

/// Maximum number of new chapters allowed in a single cycle before triggering the sanity guard.
pub const MAX_NEW_CHAPTERS_SANITY_LIMIT: usize = 20;

/// Number of consecutive anomalies after which the watcher automatically re-baselines.
pub const REBASELINE_AFTER_CONSECUTIVE_ANOMALIES: u32 = 3;

/// Configuration policy governing how [`decide`] filters and caps new chapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// If `true`, chapters without a `published` date (early access) are held back.
    pub require_published: bool,
    /// Maximum number of chapter-group messages to send in one polling cycle.
    pub max_messages_per_cycle: usize,
    /// If `true` on an uninitialized series, baselines `chapters[1..]` and announces `chapters[0]`.
    pub announce_on_first_run: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            require_published: false,
            max_messages_per_cycle: 3,
            announce_on_first_run: false,
        }
    }
}

/// A group of consecutive split parts belonging to the same chapter (or a single chapter).
///
/// Each [`ChapterGroup`] corresponds to a single Discord webhook message containing `1..=10` embeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterGroup {
    /// Main chapter number (e.g. `Some(67)` for `"Chapter 67.1"`–`"Chapter 67.3"`), or `None` for unnumbered releases.
    pub number: Option<u32>,
    /// Chapter parts ordered oldest → newest (`1..=10` items).
    pub parts: Vec<ChapterRef>,
}

/// Outcome of comparing a freshly parsed [`SeriesSnapshot`] against persisted [`SeriesState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Series has no prior state; establish a silent baseline of all current chapter IDs.
    Baseline {
        /// All chapter IDs currently on the page.
        ids: Vec<String>,
        /// Label of the newest head chapter (`chapters_newest_first[0]`).
        head_label: String,
    },
    /// No unannounced chapters were found.
    NoChange,
    /// One or more unannounced chapters were detected.
    Announce {
        /// Chapter groups ordered oldest → newest; each group is sent as one Discord POST (`<= 10` parts).
        groups: Vec<ChapterGroup>,
        /// Chapter IDs from older groups exceeding `max_messages_per_cycle` that should be marked announced without their own message.
        folded: Vec<String>,
        /// Count of new chapters that were filtered out by `require_published = true` (not added to `announced_ids`).
        held_back: usize,
    },
    /// Sanity guard triggered (`> 20` new chapters or snapshot shrank by more than half).
    Anomaly(String),
}

/// Parsed chapter number and optional split part from a chapter label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedChapterNumber {
    /// Main integer chapter number (e.g. `67` from `"Chapter 67.3"`).
    pub chapter: u32,
    /// Optional split-part integer (e.g. `Some(3)` from `"Chapter 67.3"`, `None` from `"Chapter 68"`).
    pub part: Option<u32>,
}

/// Extracts the first chapter number (and optional `.part`) from a label using `(\d+)(?:\.(\d+))?`.
#[must_use]
#[allow(clippy::missing_panics_doc)]
pub fn parse_chapter_number(label: &str) -> Option<ParsedChapterNumber> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        #[allow(clippy::expect_used)]
        Regex::new(r"(\d+)(?:\.(\d+))?").expect("valid static chapter number regex")
    });

    let caps = re.captures(label)?;
    let chapter = caps.get(1)?.as_str().parse::<u32>().ok()?;
    let part = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok());
    Some(ParsedChapterNumber { chapter, part })
}

/// Groups an oldest-to-newest sequence of chapters into [`ChapterGroup`]s of at most [`MAX_PARTS_PER_GROUP`] parts.
#[must_use]
pub fn group_chapters_oldest_first(chapters_oldest_first: Vec<ChapterRef>) -> Vec<ChapterGroup> {
    let mut groups: Vec<ChapterGroup> = Vec::new();

    for ch in chapters_oldest_first {
        let number = parse_chapter_number(&ch.label).map(|p| p.chapter);
        match (number, groups.last_mut()) {
            (Some(num), Some(last))
                if last.number == Some(num) && last.parts.len() < MAX_PARTS_PER_GROUP =>
            {
                last.parts.push(ch);
            }
            _ => {
                groups.push(ChapterGroup {
                    number,
                    parts: vec![ch],
                });
            }
        }
    }

    groups
}

/// Pure decision function comparing the persisted [`SeriesState`] against a new [`SeriesSnapshot`].
#[must_use]
pub fn decide(state: Option<&SeriesState>, snap: &SeriesSnapshot, policy: &Policy) -> Decision {
    if snap.chapters_newest_first.is_empty() {
        return Decision::Anomaly("snapshot chapter list is empty".to_string());
    }

    let synthetic_first_run_state;
    let active_state = match state.filter(|s| !s.announced_ids.is_empty()) {
        None => {
            if !policy.announce_on_first_run {
                let ids = snap
                    .chapters_newest_first
                    .iter()
                    .rev()
                    .map(|c| c.id.clone())
                    .collect();
                let head_label = snap.chapters_newest_first[0].label.clone();
                return Decision::Baseline { ids, head_label };
            }
            // --announce-on-first-run: baseline everything except chapters[0], then proceed to step 4.
            let baseline_ids: Vec<String> = snap
                .chapters_newest_first
                .iter()
                .skip(1)
                .rev()
                .map(|c| c.id.clone())
                .collect();
            synthetic_first_run_state = SeriesState {
                announced_ids: baseline_ids,
                last_announced_label: snap.chapters_newest_first.get(1).map(|c| c.label.clone()),
                last_snapshot_len: snap.chapters_newest_first.len(),
                ..SeriesState::default()
            };
            &synthetic_first_run_state
        }
        Some(s) => s,
    };

    let current_len = snap.chapters_newest_first.len();
    if active_state.last_snapshot_len > 0 && current_len < active_state.last_snapshot_len / 2 {
        return Decision::Anomaly(format!(
            "chapter list shrank from {} to {} (< half of previous snapshot)",
            active_state.last_snapshot_len, current_len
        ));
    }

    let announced_set: HashSet<&str> = active_state
        .announced_ids
        .iter()
        .map(String::as_str)
        .collect();

    let new_newest_first: Vec<&ChapterRef> = snap
        .chapters_newest_first
        .iter()
        .filter(|c| !announced_set.contains(c.id.as_str()))
        .collect();

    if new_newest_first.is_empty() {
        return Decision::NoChange;
    }

    if new_newest_first.len() > MAX_NEW_CHAPTERS_SANITY_LIMIT {
        return Decision::Anomaly(format!(
            "detected {} new chapters in one cycle (sanity limit is {})",
            new_newest_first.len(),
            MAX_NEW_CHAPTERS_SANITY_LIMIT
        ));
    }

    let mut held_back = 0usize;
    let mut eligible_newest_first: Vec<ChapterRef> = Vec::with_capacity(new_newest_first.len());
    for ch in new_newest_first {
        if policy.require_published && ch.published.is_none() {
            held_back += 1;
        } else {
            eligible_newest_first.push(ch.clone());
        }
    }

    if eligible_newest_first.is_empty() {
        return Decision::Announce {
            groups: Vec::new(),
            folded: Vec::new(),
            held_back,
        };
    }

    // Reverse to order oldest -> newest before grouping split parts.
    eligible_newest_first.reverse();
    let all_groups = group_chapters_oldest_first(eligible_newest_first);

    let cap = policy.max_messages_per_cycle.max(1);
    if all_groups.len() <= cap {
        Decision::Announce {
            groups: all_groups,
            folded: Vec::new(),
            held_back,
        }
    } else {
        let split_idx = all_groups.len() - cap;
        let (older_groups, newest_groups) = all_groups.split_at(split_idx);
        let folded: Vec<String> = older_groups
            .iter()
            .flat_map(|g| g.parts.iter().map(|p| p.id.clone()))
            .collect();
        Decision::Announce {
            groups: newest_groups.to_vec(),
            folded,
            held_back,
        }
    }
}
