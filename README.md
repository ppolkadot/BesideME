# BesideME

BesideME is a Windows desktop app and Rust command-line tool that watches manga releases and sends Discord notifications. It supports **Manga UP! Global**. The example configuration follows *Smoking Behind the Supermarket with You* (`/manga/261`).

## Features

- **Pure HTML/JSON extraction**: No browser, no headless Chrome, no cookies, no database.
- **Set-based detection (`announced_ids`)**: Immune to false positives from removed, hidden, or reordered chapters.
- **Silent first-run baseline**: The first run records all existing chapters without flooding Discord.
- **First-class split-chapter grouping**: Split releases (`67.1`, `67.2`, `67.3`) are grouped into a single Discord message with one rich embed per part (`<= 10` embeds, ordered oldest to newest), while distinct chapters (`68.x`) are sent in separate messages.
- **Strict secret separation**: Discord webhook URLs live **only** in a gitignored `.env` file, never in `config.toml`, and are redacted in all `Debug`/`Display` and log outputs.
- **Operational health alerts**: Consecutive failures trigger a single non-pinging alert to a separate health webhook (`BESIDEME_HEALTH_WEBHOOK`), followed by a recovery notice when the site succeeds again.
- **Single-instance file lock**: An OS-level lock on `state.lock` (`std::fs::File::try_lock`) prevents overlapping `check` and `watch` runs from double-announcing.
- **Release shelf (desktop app)**: Titles you follow appear as cover-led cards with **All / Unread / Issues** filters. Chapters found after a series was baselined show as *New* until you mark them read; upgrading or adding a series never creates phantom unread badges.
- **Check status**: The app shows when the last check finished, which titles failed and why, what Windows reports for the background task, and live progress for manual checks.

---

## Windows: setup wizard + desktop app (easiest)

Run `BesideME-Setup-<version>.exe`. The wizard:

1. installs per-user (no admin) to `%LOCALAPPDATA%\Programs\BesideME`;
2. offers Discord setup on a fresh install and stores the URLs in `%APPDATA%\BesideME\.env`;
3. optionally creates the scheduled task **"BesideME"**, which runs `BesideME.exe --check` every 30 minutes with no console window;
4. opens **BesideME**, where you add manga with **Quick Add**: search by name or paste a link.

The app window is borderless: it has no Windows minimize/maximize/close buttons. Use the
macOS-style traffic lights in the top-left corner (red = close, yellow = minimize,
green = zoom). Alt+F4 and the taskbar's "Close window" are ignored on purpose; only the
red button closes the app. Uninstalling removes the task and asks before deleting the
current BesideME settings and local data.

### The release shelf

The window opens on the manga you follow, in a dark, cover-led shelf (Poppins type,
coral accent). Each card shows the cover, the latest chapter, when it was last checked,
and what needs attention.

- **All / Unread / Issues** filters, with counts. *Unread* lists titles whose newest
  chapters were discovered after the series was baselined and that you have not marked
  read; *Issues* lists titles whose last check failed, with the reason on the card.
- **Open chapter** opens the newest unread chapter on Manga UP! in your browser (only
  official Manga UP! links are ever opened). **Mark read** clears the title's unread
  badge. **Open series** opens the series page. **Retry** re-checks just that title.
  **Remove** asks for confirmation (press it twice) before it stops watching a title.
- A missing cover keeps its 16:9 slot with the title's initial, so rows never jump and
  titles stay readable even when an image fails to load or the title is long or non-Latin.
- Everything is reachable with the keyboard (Tab, Shift+Tab, Enter/Space); the control
  with focus is ringed in white. The traffic lights have accessible names too
  ("Close window", "Minimize window", "Zoom window"). `Ctrl` + `+`/`-`/`0` zooms the UI.
  On small or heavily scaled screens the window opens smaller and, when height is very
  tight, the whole page scrolls.

### Status strip

The strip above the shelf answers "when did the last check finish and is anything
wrong?" It reads the checker's persisted run summary, so a result from the scheduled
task appears within a couple of seconds without restarting the app.

- Last check: how long ago, how many titles had problems, and when the last fully
  successful check was. A check that never finished is reported as interrupted.
- Background task: what Windows reports for the **"BesideME"** scheduled task. A next-run
  time appears only when the task exists, is enabled, and Windows reports one. *Turned
  off*, *not set up*, and *couldn't read Task Scheduler* are shown as different states,
  never as a healthy idle task. The query is read-only (it never creates, edits, enables,
  or runs a task) and is refreshed about once a minute.
- **Check now** runs the same check as the scheduled task, with a progress bar
  ("Checking 2 of 5 · Title") while the rest of the app stays usable. If a background
  check already holds the instance lock the app says so instead of waiting.
- **Retry** checks a single title through the same checker code, the same instance lock
  and the same Discord delivery rules. It is refused for 30 seconds after a title was last
  checked, and until `Retry-After` has passed after a rate limit (HTTP 403/429).

### Quick Add

Press **Ctrl+K** or choose **Add manga**. One panel handles both ways of adding:

- Type a title to search the cached Manga UP! catalog; use Up/Down to move through the
  results and **Enter** to add the highlighted one (or press the result's **Add** button).
- Paste a Manga UP! link (even without `https://`) to preview the page first, then add it.
  The preview is requested after a short pause or when you press Enter. An answer for
  text you have since changed is discarded and can never be added.
- Titles you already follow show **Already watching**. If the catalog can't be loaded the
  panel says so, offers **Retry catalog**, and pasting a link keeps working.
- **Esc** closes the panel and returns keyboard focus to where it was.

### Discord settings

Open **Settings** to add or update the Discord URLs. **Yamada** posts chapter updates.
**Sasaki** posts health and recovery alerts to a separate destination. Leave the health
URL blank to disable health alerts. Settings masks the URLs by default.
Launching the app again or rerunning setup is unnecessary after saving.

Each destination has its own **Send test** button. A click posts exactly one message
labelled "Yamada · chapter updates destination" or "Sasaki · health alerts destination"
(an embed that cannot mention anyone) to the URL currently typed in the field, saved or
not. "Delivered" is shown only after Discord accepts the message. A malformed URL, a
deleted or revoked webhook (HTTP 401/403/404), a rejected message, a rate limit, and a
network problem each get their own actionable message next to the field. Changing the URL
clears its result, and an answer that arrives after the URL changed is ignored. URLs and
tokens are never written to the log or shown in any message.

For a silent install, use `BesideME-Setup-<version>.exe /VERYSILENT /TASKS=background`.
The optional `/WEBHOOK` and `/HEALTHWEBHOOK` arguments can seed a fresh settings file.
Existing webhook settings are edited in the app.

When upgrading from Manga Watcher, Setup keeps the previous install location and copies
the earlier watchlist and dotenv file into the new directory if those files are missing
there. Existing history stays readable in its original location until a BesideME state
file exists. The previous data directories are retained as upgrade sources.

**Build the installer:** `powershell -ExecutionPolicy Bypass -File installer\build.ps1`
(requires Inno Setup 6.7+: `winget install JRSoftware.InnoSetup`). Output: `dist\`.

**Brand assets.** The icon generator pads the original 22×32 Yamada artwork to square
transparent canvases without changing its proportions. Run `py -3 -I assets/make_icons.py`
with Pillow installed to regenerate the 16–256 px ICO, window PNG, and 100%/200% DPI
wizard images. The app embeds Poppins Regular and SemiBold. The font license is in
[`assets/fonts/OFL.txt`](assets/fonts/OFL.txt).

The [redesign plan](docs/redesign-plan.md) describes the design and what was deliberately
left for later (tray mode, Windows toasts, an in-app reader, other providers, cloud sync,
theme editing).

### Files

| File | Where | What |
| --- | --- | --- |
| `config.toml` | `%APPDATA%\BesideME` | Watchlist and checker settings (no secrets). |
| `.env` | next to `config.toml` | Discord webhook URLs. |
| `state.json` | `%LOCALAPPDATA%\BesideME` | Checker state: announced chapters, per-title check results and failure reason, discovered releases (at most 24 per title), and the last run summary. Written only by the checker, under `state.lock`. Still schema version 2: the new fields are optional, so existing files load unchanged and show nothing as unread. A copy of the previous version that rewrites the file simply drops the new fields. |
| `reads.json` | next to `state.json` | Which discovered releases you marked read (at most 200 per title). Written only by the app, under its own short-lived `reads.lock`, so marking read never waits for, or collides with, a running check. A damaged file is set aside as `reads.json.bad` and treated as empty. |
| `logs\` | next to `state.json` | Daily rotated logs (7 kept). |

Discovery rules: a chapter group is recorded the moment a check finds it unannounced on a
series that already has a baseline, so it shows on the shelf even while Discord delivery
is failing, and it is never recorded twice (across restarts, repeated failed deliveries, or
a group that grows while undelivered). `check --dry-run` records nothing. Baselines (a new
series, `reset`, or a re-baseline after repeated anomalies) never create unread entries.
With `--announce-on-first-run` the announced chapter is recorded only once Discord accepts
it. The stored link is the provider's own chapter page; anything else is replaced by the
official series page.

### Testing without touching an install

These variables make smoke tests hermetic; none is needed in normal use.

| Variable | Effect |
| --- | --- |
| `BESIDEME_PROFILE_DIR` | Keep `config.toml`, `.env`, `state.json`, `reads.json` and logs in this one folder; no other location is read. |
| `BESIDEME_SITE_URL` | Point Manga UP! at a local fixture server (`https`, or `http` on loopback). |
| `BESIDEME_ALLOW_ANY_WEBHOOK=1` | Same as `--allow-any-webhook`, for the CLI and the app (loopback mock webhooks). |
| `BESIDEME_TASK_NAME` | Query a different scheduled task in the status strip. |

---

## Installation & Build

Requires **Rust 1.89+** (Edition 2024).

```powershell
cargo build --release                                       # CLI + desktop app
cargo build --release --no-default-features --bin besideme-cli   # CLI only (servers/Linux)
```

Binaries: `target/release/besideme-cli(.exe)` (CLI) and `target/release/BesideME.exe` (desktop app; `BesideME.exe --check` runs one headless check).

On Windows the build embeds `assets/yamada.ico` as the executable icon (see `build.rs`; requires the Windows SDK's `rc.exe`, which ships with the MSVC build tools).

### Portable install (recommended on Windows)

Put these three files in one folder, e.g. `C:\tools\BesideME\`:

```
besideme-cli.exe   (from target\release\)
config.toml         (from config.example.toml)
.env                (from .env.example, with your real webhook URLs)
```

`config.toml` is looked up next to the executable, and `.env` next to `config.toml`, so this works from Task Scheduler even though it starts programs with `C:\Windows\System32` as the working directory.

---

## Configuration

### 1. Create `.env` (Secrets)

Copy `.env.example` to `.env` (either in the current working directory or alongside `config.toml`):

```dotenv
# Chapter announcements (required unless running `check --dry-run`)
BESIDEME_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>

# Operational health alerts (optional, separate channel recommended)
BESIDEME_HEALTH_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>
```

> **Warning — Secrets in `config.toml` are rejected:**
> Never put `discord_webhook` or `webhook` keys inside `config.toml`. `config.toml` uses `deny_unknown_fields` and will reject any webhook key with an error instructing you to move it to `.env`.

`.env` resolution order (first file found wins):
1. `--env-file <path>`
2. `.env` in the same directory as the resolved `config.toml`
3. `./.env`

Variables already present in the real process environment take precedence over `.env`.
The previous default webhook variable names are accepted for upgrades. Explicit per-series
environment variable names continue to work.

### 2. Create `config.toml`

Copy `config.example.toml` to one of these locations (first found wins):
1. `--config <path>`
2. `%APPDATA%\BesideME\config.toml` (`~/.config/BesideME/config.toml` on Linux)
3. the legacy Manga Watcher user config directory, if present
4. next to `besideme-cli.exe` (portable install)
5. `./config.toml` (current working directory)

```toml
interval_minutes = 30          # minimum 10 (clamped with a warning)
jitter_seconds = 120           # uniform random in [-jitter, +jitter]
timeout_seconds = 20
user_agent = "BesideME/0.1 (+personal use)"
require_published = false      # announce early-access (undated) chapters too
max_messages_per_cycle = 3     # max chapter-group messages per series per cycle
alert_after_failures = 6       # health alert after N consecutive failures; 0 = off
mention = "@everyone"          # message content for chapter announcements; "" = no ping
# thread_id = "123"            # optional: post into a thread / forum post

[[series]]
url = "https://global.manga-up.com/manga/261"
# site = "mangaup"             # optional; if given, must match the site resolved from url
# label = "Smoking Behind the Supermarket with You"
# webhook_env = "BESIDEME_WEBHOOK_SMOKING"
# thread_id = "..."
# interval_minutes = 15
# require_published = true
```

> **Discord `@everyone` permission note:**
>Announcements send `"content": "@everyone ..."` with `"allowed_mentions": { "parse": ["everyone"] }`. If the ping does not trigger in Discord, verify that the target channel's permissions allow webhooks to **"Mention @everyone, @here, and All Roles"**.

---

## CLI Usage

```text
besideme-cli check        [--config P] [--state P] [--dry-run] [--json] [--announce-on-first-run]
besideme-cli watch        [--config P] [--state P]
besideme-cli test-webhook [--config P] [--series URL] [--health]
besideme-cli list         [--config P] [--state P] [--offline]
besideme-cli reset        (<series-url> | --all) [--state P]
Global flags: -v / -q, --allow-any-webhook, --env-file P
```

- **State file location**: `--state <path>`, defaulting to `%LOCALAPPDATA%\BesideME\state.json` on Windows (`~/.local/share/BesideME/state.json` on Linux). `besideme-cli check` records the same discovered releases, per-title results, and run summary as the desktop app's scheduled check (`watch` also records a summary for each completed pass over its due titles). Exit codes, flags, and JSON output are unchanged.
- **Log files**: Daily rotated logs (retaining 7 files) are written to `%LOCALAPPDATA%\BesideME\logs\` alongside `stderr`.

### Exit Codes

For `check` (when multiple series run, the highest-severity code wins: `1 > 5 > 4 > 2 > 3 > 10 > 0`):

| Exit Code | Hex (Task Scheduler) | Meaning |
| --- | --- | --- |
| `0` | `0x0` | OK (baseline established or no new chapters) |
| `10` | `0xA` | New chapter(s) announced successfully (**Note:** Windows Task Scheduler displays exit code `10` as `"Last Run Result: 0xA"`, which is normal when a new chapter is announced) |
| `3` | `0x3` | Site network / HTTP error |
| `2` | `0x2` | HTML / JSON parse or schema mismatch error |
| `4` | `0x4` | Discord webhook delivery error |
| `5` | `0x5` | Another `besideme-cli` instance is already holding `state.lock` |
| `1` | `0x1` | Configuration error or corrupt/unreadable `state.json` |

### Accepted Duplicate Edge Case

State is persisted immediately after each successful chapter-group webhook `POST`. If the process is forcefully killed (`SIGKILL` / power loss) in the tiny window *after* Discord accepts the HTTP POST (`200 OK`) and *before* the atomic rename of `state.json.tmp` to `state.json` completes, the next run will re-send that single chapter group once.

---

## Windows Scheduling

Choose **one** of the two modes below. Do **not** run both at the same time (if they overlap, the single-instance lock on `state.lock` safely causes the second process to exit immediately with code `5`).

### Mode A: Periodic `check` via Task Scheduler (Recommended)

Create a scheduled task that runs `besideme-cli check` every 30 minutes:

```powershell
schtasks /create /tn "BesideME" /sc minute /mo 30 /tr "\"C:\tools\BesideME\besideme-cli.exe\" check"
```

To prevent a console window from flashing on every run:
1. Open **Task Scheduler** (`taskschd.msc`) and open the `BesideME` task properties.
2. Under the **General** tab, select **"Run whether user is logged on or not"**.

### Mode B: Long-Running `watch` Daemon

1. In **Task Scheduler**, create a task triggered **"At log on"**.
2. Set the action to run `C:\tools\BesideME\besideme-cli.exe` with arguments `watch`.
3. Under **General**, select **"Run whether user is logged on or not"** (runs hidden).
4. Under **Settings**, enable **"If the task fails, restart every: 1 minute"**.

---

## Manual Acceptance Steps

1. **Dry-run against the live page**:
   ```powershell
   besideme-cli check --config config.example.toml --dry-run
   ```
   Prints `would establish baseline at Chapter 67.3` (or the current head chapter), makes no Discord POST, and creates no `state.json`.
2. **Verify webhook delivery**:
   ```powershell
   besideme-cli test-webhook
   ```
   Delivers a test embed as Yamada without pinging `@everyone`. Add `--health` to test
   the health webhook as Sasaki.
3. **End-to-end announcement test**:
   Run `besideme-cli check` once to establish the baseline in `state.json`. Then remove the newest chapter ID from `announced_ids` in `state.json` and run `besideme-cli check` again — it will post exactly one message for that chapter with a working link and exit with code `10`.

---

## How to Add a Site

Sites are pure HTML/JSON parsers that implement the `Site` trait in `src/sites/mod.rs` and perform **no I/O**. To add a new site:

1. Create `src/sites/my_site.rs` and implement `Site` (~30 lines):

```rust
use reqwest::Url;
use crate::model::{ChapterRef, SeriesSnapshot};
use crate::sites::{ParseError, Site};

pub struct MySite;

impl Site for MySite {
    fn id(&self) -> &'static str { "mysite" }
    fn display_name(&self) -> &'static str { "My Manga Site" }

    fn series_key(&self, url: &Url) -> Option<String> {
        if url.host_str()? != "example.com" { return None; }
        match url.path_segments()?.collect::<Vec<_>>().as_slice() {
            ["series", key] if !key.is_empty() => Some((*key).to_string()),
            _ => None,
        }
    }

    fn series_url(&self, key: &str) -> Url {
        Url::parse(&format!("https://example.com/series/{key}")).unwrap()
    }

    fn parse(&self, key: &str, body: &str) -> Result<SeriesSnapshot, ParseError> {
        let v: serde_json::Value =
            serde_json::from_str(body).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
        let title = v["title"].as_str()
            .ok_or_else(|| ParseError::SchemaMismatch("title".into()))?.to_string();
        let items = v["chapters"].as_array()
            .ok_or_else(|| ParseError::SchemaMismatch("chapters".into()))?;
        if items.is_empty() { return Err(ParseError::EmptyChapterList); }
        let chapters_newest_first = items.iter().map(|ch| ChapterRef {
            id: ch["id"].as_str().unwrap_or_default().to_string(),
            label: ch["label"].as_str().unwrap_or_default().to_string(),
            subtitle: None,
            url: format!("https://example.com/series/{key}/{}", ch["id"].as_str().unwrap_or_default()),
            thumbnail: None,
            published: ch["date"].as_str().map(ToString::to_string),
            early_access: ch["date"].is_null(),
        }).collect();
        Ok(SeriesSnapshot { title, chapters_newest_first })
    }
}
```

2. Register `Box::new(MySite)` in `Registry::with_defaults()` in `src/sites/mod.rs`.
3. Add a synthetic fixture under `tests/fixtures/` and a parser unit test in `tests/parser_tests.rs`.
