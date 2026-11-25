//! Wording for the status strip: when the last check finished, whether anything needs
//! attention, and what Windows says about the background task.

use std::fmt::Write as _;

use besideme::schedule::TaskStatus;
use besideme::state::{RunPhase, RunRecord};

use crate::data::Row;

/// How a line should be coloured (it is always also conveyed by its text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Good,
    Warn,
}

/// What the app knows about the scheduled task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskView {
    /// The first query has not answered yet.
    Loading,
    Known(TaskStatus),
}

/// `"just now"`, `"12 min ago"`, `"3 h ago"`, `"2 days ago"`.
#[must_use]
pub fn ago(secs: i64) -> String {
    match secs {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", s / 60),
        s if s < 36 * 3600 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

fn in_future(secs: i64) -> String {
    match secs {
        s if s <= 0 => "due now".into(),
        s if s < 90 => "in under 2 min".into(),
        s if s < 90 * 60 => format!("in {} min", (s + 59) / 60),
        s if s < 36 * 3600 => format!("in {} h", s / 3600),
        s => format!("in {} days", s / 86400),
    }
}

/// The "last check" line. `rows` provides a fallback for installs that predate run summaries.
#[must_use]
pub fn last_check_line(run: Option<&RunRecord>, rows: &[Row], now: i64) -> (String, Tone) {
    if let Some(run) = run {
        match run.phase(now) {
            RunPhase::Running => {
                return ("A background check is running now…".into(), Tone::Neutral);
            }
            RunPhase::Interrupted => {
                return (
                    format!(
                        "The check started {} did not finish. It will run again on schedule.",
                        ago(now - run.started_unix)
                    ),
                    Tone::Warn,
                );
            }
            RunPhase::Finished => {
                let when = ago(now - run.last_attempt_unix());
                // Per-title state is always current (a single-title retry updates it without
                // replacing the run), so prefer it over the run's own counts.
                let (failed, total) = if rows.is_empty() {
                    (run.failed, run.ok + run.failed)
                } else {
                    (rows.iter().filter(|r| r.has_issue()).count(), rows.len())
                };
                if failed == 0 {
                    return (
                        format!(
                            "Last check {when} · {}",
                            match total {
                                0 => "nothing to check".to_owned(),
                                1 => "1 title OK".to_owned(),
                                n => format!("all {n} titles OK"),
                            }
                        ),
                        Tone::Good,
                    );
                }
                let mut text =
                    format!("Last check {when} · {failed} of {total} titles had problems");
                if let Some(ok_at) = run.last_success_unix {
                    let _ = write!(text, " · last fully successful check {}", ago(now - ok_at));
                }
                return (text, Tone::Warn);
            }
        }
    }
    match rows.iter().filter_map(|r| r.last_checked).max() {
        Some(t) => (format!("Last check {}", ago(now - t)), Tone::Neutral),
        None if rows.is_empty() => ("Add a manga to start checking.".into(), Tone::Neutral),
        None => ("No check has finished yet.".into(), Tone::Neutral),
    }
}

/// The background-task line, or `None` where scheduled tasks do not apply.
#[must_use]
pub fn schedule_line(task: &TaskView, now: i64) -> Option<(String, Tone)> {
    let status = match task {
        TaskView::Loading => {
            return Some(("Checking Task Scheduler…".into(), Tone::Neutral));
        }
        TaskView::Known(status) => status,
    };
    Some(match status {
        TaskStatus::Unsupported => return None,
        TaskStatus::Scheduled {
            running: true, ..
        } => ("Background check is running now.".into(), Tone::Neutral),
        TaskStatus::Scheduled {
            next_run_unix: Some(next),
            next_run_local,
            ..
        } => {
            let clock = next_run_local
                .as_deref()
                .map_or_else(String::new, |t| format!(" ({t})"));
            (
                format!("Background checks on · next run {}{clock}", in_future(next - now)),
                Tone::Good,
            )
        }
        TaskStatus::Scheduled {
            next_run_unix: None,
            ..
        } => (
            "Background checks are scheduled, but Windows didn't report the next run.".into(),
            Tone::Neutral,
        ),
        TaskStatus::Disabled => (
            "Background checks are turned off in Task Scheduler. Enable the \"BesideME\" task to resume them."
                .into(),
            Tone::Warn,
        ),
        TaskStatus::NotInstalled => (
            "Background checks aren't set up. Run Setup again and choose background checks to add them."
                .into(),
            Tone::Warn,
        ),
        TaskStatus::Unavailable(reason) => (
            format!("Couldn't read Task Scheduler: {reason}"),
            Tone::Warn,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn run(failed: usize, ok: usize) -> RunRecord {
        RunRecord {
            started_unix: NOW - 700,
            finished_unix: Some(NOW - 600),
            ok,
            failed,
            last_success_unix: Some(NOW - 7200),
            ..RunRecord::default()
        }
    }

    #[test]
    fn finished_runs_describe_health() {
        let (text, tone) = last_check_line(Some(&run(0, 3)), &[], NOW);
        assert_eq!(text, "Last check 10 min ago · all 3 titles OK");
        assert_eq!(tone, Tone::Good);
        let (text, tone) = last_check_line(Some(&run(1, 2)), &[], NOW);
        assert!(text.contains("1 of 3 titles had problems"), "{text}");
        assert!(
            text.contains("last fully successful check 2 h ago"),
            "{text}"
        );
        assert_eq!(tone, Tone::Warn);
    }

    fn row(error: Option<&str>) -> Row {
        Row {
            key: besideme::model::SeriesKey::new("mangaup", "1"),
            url: String::new(),
            title: "T".into(),
            latest: None,
            last_checked: Some(NOW - 60),
            last_success: None,
            failures: u32::from(error.is_some()),
            last_error: error.map(str::to_owned),
            unread: Vec::new(),
            retry_ready_at: None,
            baselined: true,
        }
    }

    #[test]
    fn per_title_state_wins_so_a_successful_retry_clears_the_warning() {
        // The full run saw one failure, but the retried title has since recovered.
        let rows = [row(None), row(None)];
        let (text, tone) = last_check_line(Some(&run(1, 1)), &rows, NOW);
        assert_eq!(text, "Last check 10 min ago · all 2 titles OK");
        assert_eq!(tone, Tone::Good);
        // And a title that failed since the run is reported even if the run was clean.
        let rows = [row(None), row(Some("HTTP 503"))];
        let (text, tone) = last_check_line(Some(&run(0, 2)), &rows, NOW);
        assert!(text.contains("1 of 2 titles had problems"), "{text}");
        assert_eq!(tone, Tone::Warn);
    }

    #[test]
    fn running_and_interrupted_runs_are_distinct_from_finished() {
        let running = RunRecord {
            started_unix: NOW - 30,
            ..RunRecord::default()
        };
        assert!(
            last_check_line(Some(&running), &[], NOW)
                .0
                .contains("running now")
        );
        let interrupted = RunRecord {
            started_unix: NOW - 3600,
            ..RunRecord::default()
        };
        let (text, tone) = last_check_line(Some(&interrupted), &[], NOW);
        assert!(text.contains("did not finish"));
        assert_eq!(tone, Tone::Warn);
    }

    #[test]
    fn upgraded_installs_without_a_summary_fall_back_to_the_titles() {
        assert_eq!(
            last_check_line(None, &[], NOW).0,
            "Add a manga to start checking."
        );
    }

    #[test]
    fn task_states_read_differently() {
        let scheduled = TaskView::Known(TaskStatus::Scheduled {
            next_run_unix: Some(NOW + 18 * 60),
            next_run_local: Some("2026-11-25 14:30".into()),
            running: false,
        });
        let (text, tone) = schedule_line(&scheduled, NOW).unwrap();
        assert_eq!(
            text,
            "Background checks on · next run in 18 min (2026-11-25 14:30)"
        );
        assert_eq!(tone, Tone::Good);

        let no_next = TaskView::Known(TaskStatus::Scheduled {
            next_run_unix: None,
            next_run_local: None,
            running: false,
        });
        let (text, _) = schedule_line(&no_next, NOW).unwrap();
        assert!(
            !text.contains("next run in"),
            "no invented next-run time: {text}"
        );

        let disabled = schedule_line(&TaskView::Known(TaskStatus::Disabled), NOW).unwrap();
        let missing = schedule_line(&TaskView::Known(TaskStatus::NotInstalled), NOW).unwrap();
        let broken =
            schedule_line(&TaskView::Known(TaskStatus::Unavailable("x".into())), NOW).unwrap();
        assert!(disabled.0.contains("turned off"));
        assert!(missing.0.contains("aren't set up"));
        assert!(broken.0.contains("Couldn't read"));
        assert_ne!(disabled.0, missing.0);
        assert!(schedule_line(&TaskView::Known(TaskStatus::Unsupported), NOW).is_none());
        assert_eq!(
            schedule_line(&TaskView::Loading, NOW).unwrap().0,
            "Checking Task Scheduler…"
        );
    }

    #[test]
    fn relative_times_round_sensibly() {
        assert_eq!(in_future(-5), "due now");
        assert_eq!(in_future(10), "in under 2 min");
        assert_eq!(in_future(61 * 60), "in 61 min");
        assert_eq!(in_future(5 * 3600), "in 5 h");
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(3 * 86400), "3 days ago");
    }
}
