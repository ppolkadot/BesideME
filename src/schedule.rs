//! Read-only status of the Windows scheduled task created by the setup wizard.
//!
//! The query asks Windows (through its own PowerShell cmdlets) whether the task exists, is
//! enabled, and when it will run next. It never creates, changes, enables, disables, or runs
//! a task. On other platforms the status is [`TaskStatus::Unsupported`].

use std::time::Duration;

use serde::Deserialize;

/// Name of the task the installer registers.
pub const TASK_NAME: &str = "BesideME";

/// How long a status query may take before it is abandoned.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(12);

/// What Windows reports about the background-check task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// The task exists and is enabled.
    Scheduled {
        /// Next run, when Windows reports one (Unix seconds).
        next_run_unix: Option<i64>,
        /// The same moment in the user's local time as `yyyy-MM-dd HH:mm`, when reported.
        next_run_local: Option<String>,
        /// Whether a run is in progress right now.
        running: bool,
    },
    /// The task exists but is disabled, so nothing will run.
    Disabled,
    /// No task with this name exists.
    NotInstalled,
    /// Windows could not be asked (or answered unexpectedly).
    Unavailable(String),
    /// Scheduled tasks are only queried on Windows.
    Unsupported,
}

impl TaskStatus {
    /// Next run time, only when the task exists, is enabled, and Windows reported one.
    #[must_use]
    pub const fn next_run_unix(&self) -> Option<i64> {
        match self {
            Self::Scheduled { next_run_unix, .. } => *next_run_unix,
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    #[serde(default)]
    found: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    next: Option<i64>,
    #[serde(default)]
    next_local: Option<String>,
}

/// Interprets the JSON line printed by the query script.
#[must_use]
pub fn parse_query_output(stdout: &str) -> TaskStatus {
    let Some(raw) = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('{'))
        .and_then(|line| serde_json::from_str::<RawQuery>(line).ok())
    else {
        return TaskStatus::Unavailable("Windows returned no task information.".to_owned());
    };
    if raw.error.is_some() {
        return TaskStatus::Unavailable("Windows could not read the task.".to_owned());
    }
    if !raw.found {
        return TaskStatus::NotInstalled;
    }
    let state = raw.state.as_deref().unwrap_or_default();
    if raw.enabled == Some(false) || state.eq_ignore_ascii_case("disabled") {
        return TaskStatus::Disabled;
    }
    if state.is_empty() || state.eq_ignore_ascii_case("unknown") {
        return TaskStatus::Unavailable("Windows did not report the task state.".to_owned());
    }
    let next_run_unix = raw.next.filter(|t| *t > 0);
    TaskStatus::Scheduled {
        next_run_unix,
        next_run_local: next_run_unix.and(raw.next_local).filter(|text| {
            text.len() == 16
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || "- :".contains(c))
        }),
        running: state.eq_ignore_ascii_case("running"),
    }
}

fn query_script(task_name: &str) -> String {
    // The name is a program constant in production; quotes are doubled for PowerShell anyway.
    let name = task_name.replace('\'', "''");
    format!(
        "$ErrorActionPreference='Stop'; \
         try {{ \
           $t = Get-ScheduledTask -TaskName '{name}' -ErrorAction SilentlyContinue; \
           if ($null -eq $t) {{ '{{\"found\":false}}'; exit 0 }}; \
           $i = Get-ScheduledTaskInfo -InputObject $t; \
           $n = $null; $l = $null; \
           if ($i.NextRunTime -and $i.NextRunTime.Year -gt 2000) {{ \
             $n = [int64]([DateTimeOffset]$i.NextRunTime).ToUnixTimeSeconds(); \
             $l = $i.NextRunTime.ToString('yyyy-MM-dd HH:mm', [Globalization.CultureInfo]::InvariantCulture) }}; \
           [pscustomobject]@{{ found = $true; state = [string]$t.State; \
             enabled = [bool]$t.Settings.Enabled; next = $n; next_local = $l }} | ConvertTo-Json -Compress \
         }} catch {{ '{{\"error\":\"query failed\"}}' }}"
    )
}

/// Environment variable naming a different task to query (for smoke tests with a task of
/// their own); the installer's task is queried when it is unset.
pub const TASK_NAME_ENV: &str = "BESIDEME_TASK_NAME";

/// Asks Windows about the installer's task. Blocking; call it from a worker thread.
#[must_use]
pub fn query() -> TaskStatus {
    let name = std::env::var(TASK_NAME_ENV)
        .ok()
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    query_named(name.as_deref().unwrap_or(TASK_NAME))
}

/// Asks Windows about the task called `task_name`. Blocking; call it from a worker thread.
#[must_use]
pub fn query_named(task_name: &str) -> TaskStatus {
    #[cfg(windows)]
    {
        windows_query(task_name)
    }
    #[cfg(not(windows))]
    {
        let _ = task_name;
        TaskStatus::Unsupported
    }
}

#[cfg(windows)]
fn windows_query(task_name: &str) -> TaskStatus {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned());
    let exe = std::path::Path::new(&root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");

    let Ok(mut child) = Command::new(exe)
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(query_script(task_name))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    else {
        return TaskStatus::Unavailable("Windows PowerShell could not start.".to_owned());
    };

    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut text);
        }
        text
    });

    let deadline = Instant::now() + QUERY_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return TaskStatus::Unavailable("Windows did not answer in time.".to_owned());
            }
        }
    }
    reader.join().map_or_else(
        |_| TaskStatus::Unavailable("Windows returned no task information.".to_owned()),
        |text| parse_query_output(&text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_idle_task_reports_next_run() {
        let status = parse_query_output(
            r#"{"found":true,"state":"Ready","enabled":true,"next":1795618800,"next_local":"2026-11-25 14:30"}"#,
        );
        assert_eq!(
            status,
            TaskStatus::Scheduled {
                next_run_unix: Some(1_795_618_800),
                next_run_local: Some("2026-11-25 14:30".into()),
                running: false
            }
        );
        assert_eq!(status.next_run_unix(), Some(1_795_618_800));
    }

    #[test]
    fn running_task_is_flagged() {
        let status =
            parse_query_output(r#"{"found":true,"state":"Running","enabled":true,"next":null}"#);
        assert_eq!(
            status,
            TaskStatus::Scheduled {
                next_run_unix: None,
                next_run_local: None,
                running: true
            }
        );
    }

    #[test]
    fn missing_disabled_and_unavailable_are_distinct_states() {
        assert_eq!(
            parse_query_output(r#"{"found":false}"#),
            TaskStatus::NotInstalled
        );
        let disabled =
            parse_query_output(r#"{"found":true,"state":"Disabled","enabled":false,"next":null}"#);
        assert_eq!(disabled, TaskStatus::Disabled);
        assert_eq!(disabled.next_run_unix(), None);
        assert!(matches!(
            parse_query_output(r#"{"error":"query failed"}"#),
            TaskStatus::Unavailable(_)
        ));
        assert!(matches!(
            parse_query_output("garbage"),
            TaskStatus::Unavailable(_)
        ));
        assert!(matches!(
            parse_query_output(r#"{"found":true,"state":"Unknown","enabled":true}"#),
            TaskStatus::Unavailable(_)
        ));
    }

    #[test]
    fn disabled_task_never_reports_a_next_run() {
        let status = parse_query_output(
            r#"{"found":true,"state":"Ready","enabled":false,"next":1795618800}"#,
        );
        assert_eq!(status, TaskStatus::Disabled);
    }

    #[test]
    fn script_quotes_task_name() {
        assert!(query_script("it's").contains("'it''s'"));
    }

    #[cfg(windows)]
    #[test]
    fn unknown_task_is_not_installed() {
        // Read-only query of a name that cannot exist; exercises the real Windows path.
        let status = query_named("BesideME-no-such-task-3f9c1d");
        assert!(
            matches!(
                status,
                TaskStatus::NotInstalled | TaskStatus::Unavailable(_)
            ),
            "{status:?}"
        );
    }
}
