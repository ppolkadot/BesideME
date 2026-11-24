# manga-watcher

A small, single-binary Rust utility that watches manga series pages and posts a Discord webhook notification when a new chapter appears. Built primarily for **Manga UP! Global** (`https://global.manga-up.com`), starting with *Smoking Behind the Supermarket with You* (`/manga/261`).

## Features

- **Pure HTML/JSON extraction**: No browser, no headless Chrome, no cookies, no database.
- **Set-based detection (`announced_ids`)**: Immune to false positives from removed, hidden, or reordered chapters.
- **Silent first-run baseline**: The first run records all existing chapters without flooding Discord.
- **First-class split-chapter grouping**: Split releases (`67.1`, `67.2`, `67.3`) are grouped into a single Discord message with one rich embed per part (`<= 10` embeds, ordered oldest to newest), while distinct chapters (`68.x`) are sent in separate messages.
- **Strict secret separation**: Discord webhook URLs live **only** in a gitignored `.env` file, never in `config.toml`, and are redacted in all `Debug`/`Display` and log outputs.
- **Operational health alerts**: Consecutive failures trigger a single non-pinging alert to a separate health webhook (`MANGA_WATCHER_HEALTH_WEBHOOK`), followed by a recovery notice when the site succeeds again.
- **Single-instance file lock**: An OS-level lock on `state.lock` (`std::fs::File::try_lock`) prevents overlapping `check` and `watch` runs from double-announcing.

---

## Windows: setup wizard + desktop app (easiest)

Run `MangaWatcher-Setup-<version>.exe`. The wizard:

1. installs per-user (no admin) to `%LOCALAPPDATA%\Programs\Manga Watcher`;
2. asks for the **new chapters** webhook (required) and the **health alerts** webhook (optional, must be a different channel) and saves them to `%APPDATA%\manga-watcher\.env`;
3. optionally creates the scheduled task **"Manga Watcher"**, which runs `MangaWatcher.exe --check` every 30 minutes with no console window;
4. opens **Manga Watcher**, where you add manga by **searching by name** or **pasting a link**.

The app window is borderless: it has no Windows minimize/maximize/close buttons. Use the
macOS-style traffic lights in the top-left corner (red = close, yellow = minimize,
green = zoom). Alt+F4 and the taskbar's "Close window" are ignored on purpose; only the
red button closes the app. Re-run the setup to change webhooks (current values are
pre-filled). Uninstalling removes the task and asks before deleting your settings.

Silent/scripted install: `MangaWatcher-Setup-<version>.exe /VERYSILENT /TASKS=background /WEBHOOK=<url> /HEALTHWEBHOOK=<url>`.

**Build the installer:** `powershell -ExecutionPolicy Bypass -File installer\build.ps1`
(requires Inno Setup 6.7+: `winget install JRSoftware.InnoSetup`). Output: `dist\`.

**Icons:** `assets/yamada-source.ico` is the original (22×32). `python -I assets/make_icons.py`
regenerates the square multi-size `yamada.ico` (16–256 px), the window icon PNG and the
installer wizard images. Replace the source with a larger square image for sharper results.

---

## Installation & Build

Requires **Rust 1.89+** (Edition 2024).

```powershell
cargo build --release                                       # CLI + desktop app
cargo build --release --no-default-features --bin manga-watcher   # CLI only (servers/Linux)
```

Binaries: `target/release/manga-watcher(.exe)` (CLI) and `target/release/MangaWatcher.exe` (desktop app; `MangaWatcher.exe --check` runs one headless check).

On Windows the build embeds `assets/yamada.ico` as the executable icon (see `build.rs`; requires the Windows SDK's `rc.exe`, which ships with the MSVC build tools).

### Portable install (recommended on Windows)

Put these three files in one folder, e.g. `C:\tools\manga-watcher\`:

```
manga-watcher.exe   (from target\release\)
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
MANGA_WATCHER_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>

# Operational health alerts (optional, separate channel recommended)
MANGA_WATCHER_HEALTH_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>
```

> **Warning — Secrets in `config.toml` are rejected:**
> Never put `discord_webhook` or `webhook` keys inside `config.toml`. `config.toml` uses `deny_unknown_fields` and will reject any webhook key with an error instructing you to move it to `.env`.

`.env` resolution order (first file found wins):
1. `--env-file <path>`
2. `.env` in the same directory as the resolved `config.toml`
3. `./.env`

Variables already present in the real process environment take precedence over `.env`.

### 2. Create `config.toml`

Copy `config.example.toml` to one of these locations (first found wins):
1. `--config <path>`
2. `%APPDATA%\manga-watcher\config.toml` (`~/.config/manga-watcher/config.toml` on Linux)
3. next to `manga-watcher.exe` (portable install)
4. `./config.toml` (current working directory)

```toml
interval_minutes = 30          # minimum 10 (clamped with a warning)
jitter_seconds = 120           # uniform random in [-jitter, +jitter]
timeout_seconds = 20
user_agent = "manga-watcher/0.1 (+personal use)"
require_published = false      # announce early-access (undated) chapters too
max_messages_per_cycle = 3     # max chapter-group messages per series per cycle
alert_after_failures = 6       # health alert after N consecutive failures; 0 = off
mention = "@everyone"          # message content for chapter announcements; "" = no ping
# thread_id = "123"            # optional: post into a thread / forum post

[[series]]
url = "https://global.manga-up.com/manga/261"
# site = "mangaup"             # optional; if given, must match the site resolved from url
# label = "Smoking Behind the Supermarket with You"
# webhook_env = "MANGA_WATCHER_WEBHOOK_SMOKING"
# thread_id = "..."
# interval_minutes = 15
# require_published = true
```

> **Discord `@everyone` permission note:**
>Announcements send `"content": "@everyone ..."` with `"allowed_mentions": { "parse": ["everyone"] }`. If the ping does not trigger in Discord, verify that the target channel's permissions allow webhooks to **"Mention @everyone, @here, and All Roles"**.

---

## CLI Usage

```text
manga-watcher check        [--config P] [--state P] [--dry-run] [--json] [--announce-on-first-run]
manga-watcher watch        [--config P] [--state P]
manga-watcher test-webhook [--config P] [--series URL] [--health]
manga-watcher list         [--config P] [--state P] [--offline]
manga-watcher reset        (<series-url> | --all) [--state P]
Global flags: -v / -q, --allow-any-webhook, --env-file P
```

- **State file location**: `--state <path>`, defaulting to `%LOCALAPPDATA%\manga-watcher\state.json` on Windows (`~/.local/share/manga-watcher/state.json` on Linux).
- **Log files**: Daily rotated logs (retaining 7 files) are written to `%LOCALAPPDATA%\manga-watcher\logs\` alongside `stderr`.

### Exit Codes

For `check` (when multiple series run, the highest-severity code wins: `1 > 5 > 4 > 2 > 3 > 10 > 0`):

| Exit Code | Hex (Task Scheduler) | Meaning |
| --- | --- | --- |
| `0` | `0x0` | OK (baseline established or no new chapters) |
| `10` | `0xA` | New chapter(s) announced successfully (**Note:** Windows Task Scheduler displays exit code `10` as `"Last Run Result: 0xA"`, which is normal when a new chapter is announced) |
| `3` | `0x3` | Site network / HTTP error |
| `2` | `0x2` | HTML / JSON parse or schema mismatch error |
| `4` | `0x4` | Discord webhook delivery error |
| `5` | `0x5` | Another `manga-watcher` instance is already holding `state.lock` |
| `1` | `0x1` | Configuration error or corrupt/unreadable `state.json` |

### Accepted Duplicate Edge Case

State is persisted immediately after each successful chapter-group webhook `POST`. If the process is forcefully killed (`SIGKILL` / power loss) in the tiny window *after* Discord accepts the HTTP POST (`200 OK`) and *before* the atomic rename of `state.json.tmp` to `state.json` completes, the next run will re-send that single chapter group once.

---

## Windows Scheduling

Choose **one** of the two modes below. Do **not** run both at the same time (if they overlap, the single-instance lock on `state.lock` safely causes the second process to exit immediately with code `5`).

### Mode A: Periodic `check` via Task Scheduler (Recommended)

Create a scheduled task that runs `manga-watcher check` every 30 minutes:

```powershell
schtasks /create /tn "MangaWatcher" /sc minute /mo 30 /tr "\"C:\tools\manga-watcher\manga-watcher.exe\" check"
```

To prevent a console window from flashing on every run:
1. Open **Task Scheduler** (`taskschd.msc`) and open the `MangaWatcher` task properties.
2. Under the **General** tab, select **"Run whether user is logged on or not"**.

### Mode B: Long-Running `watch` Daemon

1. In **Task Scheduler**, create a task triggered **"At log on"**.
2. Set the action to run `C:\tools\manga-watcher\manga-watcher.exe` with arguments `watch`.
3. Under **General**, select **"Run whether user is logged on or not"** (runs hidden).
4. Under **Settings**, enable **"If the task fails, restart every: 1 minute"**.

---

## Manual Acceptance Steps

1. **Dry-run against the live page**:
   ```powershell
   manga-watcher check --config config.example.toml --dry-run
   ```
   Prints `would establish baseline at Chapter 67.3` (or the current head chapter), makes no Discord POST, and creates no `state.json`.
2. **Verify webhook delivery**:
   ```powershell
   manga-watcher test-webhook
   ```
   Delivers a test embed to your Discord channel without pinging `@everyone`.
3. **End-to-end announcement test**:
   Run `manga-watcher check` once to establish the baseline in `state.json`. Then remove the newest chapter ID from `announced_ids` in `state.json` and run `manga-watcher check` again — it will post exactly one message for that chapter with a working link and exit with code `10`.

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
