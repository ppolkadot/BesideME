# BesideME Implementation Prompt

This document describes the original chapter-checking architecture. See [README.md](README.md)
for the current desktop setup and [the redesign proposal](docs/redesign-plan.md) for the next release.

## 0. What you are building

A small, lightweight Rust executable that watches manga series pages and sends a Discord
webhook message when a new chapter appears. Primary target today:
**Manga UP! Global** (`https://global.manga-up.com`), starting with
*Smoking Behind the Supermarket with You* (`/manga/261`).

Adding another site later must mean implementing one trait in one new file, without
touching the polling loop, state handling, HTTP code, or Discord code.

Hard requirements:

- Single binary, low memory, no database, no browser, no headless Chrome.
- Runs on Windows (primary), also builds and runs on Linux (secondary PC / future VPS).
- At most one HTTP GET per series per polling cycle. Series are fetched sequentially,
  never in parallel.
- The Discord message contains the manga title, chapter name, chapter number and a direct
  link to the chapter, and pings `@everyone`.
- Split uploads are first-class. Manga UP! publishes chapters in parts (67.1, 67.2, 67.3,
  68.1, …). Parts of the same chapter go in **one message**, and different chapters go in
  **separate messages** (§4.3, §5).
- Secrets (webhook URLs) live only in a `.env` file, which is gitignored. They never go in
  `config.toml`, logs or error messages.
- Never notify twice for the same chapter, including across restarts. The one accepted
  exception is described in §4.5.
- The first run establishes a baseline silently, so the existing backlog is never posted.
- A broken scraper must not fail silently. If the watcher stops working, the human is told
  in Discord (§4.6).

Non-goals:

- No bypassing login, paywall, region locks, or DRM. Only public HTML is read.
- No downloading or storing chapter images. Never use `firstPageImageUrl`, because it
  points at chapter page content. Only the chapter cover thumbnail (`imageUrl`) may be
  referenced.
- No multi-user server, no web UI, no database.
- Must not become a crawler: no crawling, no searching, no retry storms.

---

## 1. Verified recon (re-verified 2026-11-23)

`GET https://global.manga-up.com/manga/261` returns HTTP 200 and a Next.js SSR document of
about 330 KB, or about 26 KB with gzip. **It needs no cookies, no JS execution and no
login.** The descriptive UA `BesideME/0.1 (+personal use)` gets the same full
response as a browser UA, so there is no need to imitate a browser.

Response headers: `Cache-Control: s-maxage=30, stale-while-revalidate=…`,
`x-nextjs-cache: HIT`, `ETag: "…"`. This is ISR-cached, so a new chapter can appear one
fetch late. That is acceptable.

**A nonexistent title (`/manga/99999999`) and a non-numeric key (`/manga/abc`) do not
return 404: the connection hangs until the client gives up (>60 s observed).** The request
timeout is therefore a correctness feature, not just politeness. A timeout on a series
that has never succeeded most likely means the URL is wrong (§7).

The chapter list is embedded in:

```html
<script id="__NEXT_DATA__" type="application/json">{ ... }</script>
```

JSON shape (`props.pageProps` keys observed: `data`, `error`, `titleId`):

```
props.pageProps.titleId              -> 261
props.pageProps.error                -> present; treat a non-null/truthy value as an error
props.pageProps.data.titleName       -> "Smoking Behind the Supermarket with You"
props.pageProps.data.nextUpdateInfo  -> "Check on the 25th monthly for new chapter updates"
props.pageProps.data.chapters[]      -> newest first, 159 entries observed
```

Chapter entries (newest first, observed):

```json
{ "id": 130422, "mainName": "Chapter 67.3", "subName": "67th Smoke",
  "consumptionType": 1, "price": 80, "badge": 2, "numberOfComments": 98,
  "imageUrl": "/asset/J0O/ja/chapter/130422.webp?h=...&e=1795618800",
  "firstPageImageUrl": "..." }
{ "id": 130413, "mainName": "Chapter 66.2", "subName": "66th Smoke",
  "price": 40, "numberOfComments": 12, "published": "Nov 21, 2026", ... }
{ "id": 28665,  "mainName": "Chapter 1", "subName": "1st Smoke",
  "consumptionType": 3, "published": "Apr 04, 2023", ... }
```

Facts to encode as assumptions. Assert them in tests and log a warning when the live site
violates them:

- `chapters[0]` is the newest. **Array order is the authority for display order. Detection
  does not depend on order at all (§4).**
- The 3 newest entries (67.1–67.3) have no `published` field. They also carry `badge: 2`,
  `consumptionType: 1` and a higher `price` (80 vs 40). Treat a missing `published` as
  early access: listed, but not on general release yet. The other fields are secondary
  signals. Record them for debugging and do not base decisions on them.
- Image host: `https://global-img.manga-up.com` + `imageUrl`. The URL is signed and
  expires (`e=` is a unix timestamp), so a thumbnail may stop loading days after the post.
  That is acceptable.
- Chapter URL: `https://global.manga-up.com/manga/{titleId}/{chapterId}` (verified 200).
- `id` is a JSON number. Store it as a string, because ids are site-scoped and opaque.

Extraction, robust against attribute reordering or added `nonce` / `crossorigin`
attributes:

1. Find `id="__NEXT_DATA__"`, then the next `>`, then the next `</script>`.
2. `serde_json::from_str` the slice into **minimal** serde structs. Only `titleName`,
   `chapters[].id` and `chapters[].mainName` are required. Everything else is
   `Option<…>`, and unknown fields are ignored. Do **not** use `deny_unknown_fields`.
3. Errors are typed: `NextDataMissing`, `InvalidJson`, `SchemaMismatch(path)`,
   `PageError`, `EmptyChapterList`. **Never** map a parse failure to "no new chapters".

---

## 2. Architecture

Lib + thin bin, so `tests/` integration tests can import the logic (a bin-only crate
cannot be imported):

```
src/
  main.rs            clap CLI, tracing init, exit-code mapping only
  lib.rs             re-exports, `run_check`, `run_watch`
  config.rs          TOML load + validation + defaults + webhook resolution
  paths.rs           config/state/log/lock path resolution (`dirs` crate)
  state.rs           persisted state, atomic write, single-instance lock
  http.rs            the ONLY place that performs site GETs (UA, timeout, size cap, ETag)
  detect.rs          pure `decide()`; no I/O
  notify/discord.rs  payload building (pure) + POST client
  watcher.rs         scheduler: per-series next-due, jitter, backoff, health alerts
  sites/mod.rs       `Site` trait + `Registry`
  sites/manga_up.rs  Manga UP! parser (pure)
  model.rs           ChapterRef, SeriesSnapshot, SeriesKey
```

**Sites are pure parsers and do no I/O.** This avoids the async-trait / `dyn` problem
(native `async fn` in traits is not dyn-compatible). It also keeps politeness rules
(UA, timeout, size cap, one GET) in a single place and makes every site testable from a
string.

```rust
pub struct ChapterRef {
    pub id: String,                 // site-scoped, stable, opaque
    pub label: String,              // "Chapter 67.3"
    pub subtitle: Option<String>,   // "67th Smoke"
    pub url: String,                // absolute, clickable
    pub thumbnail: Option<String>,  // absolute
    pub published: Option<String>,  // raw site string, kept as-is
    pub early_access: bool,         // true when `published` is absent
}

pub struct SeriesSnapshot {
    pub title: String,
    pub chapters_newest_first: Vec<ChapterRef>,
}

pub trait Site: Send + Sync {
    /// Stable id used in config and state keys, e.g. "mangaup".
    fn id(&self) -> &'static str;
    /// Human name for the embed author line, e.g. "Manga UP!".
    fn display_name(&self) -> &'static str;
    /// Returns Some(key) if this site owns the URL ("261"), else None.
    fn series_key(&self, url: &reqwest::Url) -> Option<String>;
    /// The single URL fetched per cycle for this series.
    fn series_url(&self, key: &str) -> reqwest::Url;
    /// Parse the fetched body. Must be pure.
    fn parse(&self, key: &str, body: &str) -> Result<SeriesSnapshot, ParseError>;
}
```

`Registry` is built at runtime (`Vec<Box<dyn Site>>`). `MangaUp::new(base_url, img_base)`
takes its hosts as parameters, so tests can point it at a mock server. The production
registry uses the real hosts. Key parsing uses URL path segments: `["manga", "<digits>"]`,
with an optional trailing slash and the query string ignored. Anything else returns `None`.

---

## 3. Config

Path resolution order (use the `dirs` crate and append `BesideME`):

1. `--config <path>`
2. `dirs::config_dir()/BesideME/config.toml`. On Windows this is
   `%APPDATA%\BesideME\config.toml`, on Linux `~/.config/BesideME/config.toml`.
3. `./config.toml`

```toml
# Global defaults; per-series values override.
interval_minutes = 30          # minimum 10 (clamped, with a warning)
jitter_seconds = 120           # uniform random in [-jitter, +jitter]
timeout_seconds = 20
user_agent = "BesideME/0.1 (+personal use)"
require_published = false      # announce early-access (undated) chapters too
max_messages_per_cycle = 3     # max chapter-group messages per series per cycle, see §4.3
alert_after_failures = 6       # health alert after N consecutive failures; 0 = off
mention = "@everyone"          # message content for chapter announcements; "" = no ping
# thread_id = "123"            # optional: post into a thread / forum post

[[series]]
url = "https://global.manga-up.com/manga/261"
# site = "mangaup"             # optional; if given, must match the site resolved from url
# label = "Smoking..."         # optional display override for the manga title
# webhook_env = "BESIDEME_WEBHOOK_SMOKING"  # optional: NAME of an env var to use instead
# thread_id = "..."            # optional per-series override
# interval_minutes = 15        # optional per-series override (still >= 10)
# require_published = true     # optional per-series override
```

`config.toml` contains **no secrets** and may be committed. Webhooks come from the
environment, normally loaded from a `.env` file:

```dotenv
# .env  — gitignored, never committed
BESIDEME_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>          # chapter announcements (required)
BESIDEME_HEALTH_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>   # health alerts (optional)
```

Rules:

- **`.env` loading** (`dotenvy`), first file found wins: `--env-file <path>`, else `.env`
  in the same directory as the resolved `config.toml`, else `./.env`. A missing `.env` is
  fine. Variables already set in the real process environment take precedence over
  `.env` (dotenvy's default, non-overriding behaviour). Log the path loaded, never its
  values.
- **Announcement webhook for a series:** the env var named by the per-series
  `webhook_env` if set, otherwise `BESIDEME_WEBHOOK`. If it is unresolved, that is a
  config error, except under `check --dry-run`.
- **Health webhook:** `BESIDEME_HEALTH_WEBHOOK`, a **separate** channel. If it is
  unset, health alerts are disabled and startup logs a warning. Never fall back to the
  announcement webhook. Same validation rules as below.
- Repo hygiene, created in step 1 of §10:
  - `.gitignore` with `/target`, `.env`, `.env.*`, `!.env.example`, `state.json`,
    `*.log`
  - a committed `.env.example` with placeholder values
  - a committed `config.example.toml`
- Webhook validation: scheme `https`, host one of `discord.com`, `discordapp.com`,
  `ptb.discord.com`, `canary.discord.com`, path `/api/webhooks/<digits>/<token>`.
  `--allow-any-webhook` lifts the host check, and allows `http` **only** for loopback
  hosts (for local mocks).
- Never log a webhook. `Display`/`Debug` for the webhook type always prints
  `https://discord.com/api/webhooks/***`. Implement it as a newtype so it cannot leak by
  accident.
- Duplicate series (same site + key) is a config error.
- A URL that matches no registered site is a config error, not a runtime skip.
- Unknown TOML keys are an error (`deny_unknown_fields` on the **config** structs), so
  typos are caught.
- Validation never touches the network.
- Unknown TOML keys are rejected (see above), so an old-style `discord_webhook` /
  `webhook` key in `config.toml` is rejected with a hint to move it to `.env`.

---

## 4. State and detection

### 4.1 State file

Path: `--state <path>`, else `dirs::data_local_dir()/BesideME/state.json`. On Windows
that is `%LOCALAPPDATA%\BesideME\state.json`, on Linux
`~/.local/share/BesideME/state.json`.

```json
{
  "version": 2,
  "series": {
    "mangaup:261": {
      "announced_ids": ["28665", "...", "130422"],
      "last_announced_label": "Chapter 67.3",
      "last_snapshot_len": 159,
      "last_checked_unix": 1795618800,
      "last_success_unix": 1795618800,
      "consecutive_failures": 0,
      "alert_sent": false,
      "etag": "\"sln4x33xql78nt\""
    }
  }
}
```

**Why a set of announced ids instead of a single "last id" pointer:** with a pointer, if
the site removes or hides the newest chapter, the new head is not the pointer and the
pointer is not found in the list, so the old chapter is falsely announced as new. The same
happens with reordering or a temporarily truncated list. With a set, a chapter is
announced only if its id has never been announced. Removals, reordering and truncation
then cannot cause false positives. About 160 ids is trivial in size. Hard cap 5 000 ids;
when over the cap, prune ids absent from the latest snapshot first.

A series is "initialized" when its key exists in `series`.

Writes are atomic: write `state.json.tmp` in the same directory, `sync_all`, then
`std::fs::rename` (which replaces on Windows too). Retry the rename up to 3 times with
100 ms sleeps, because antivirus or the indexer can briefly hold the file on Windows.

Corrupt or unreadable state: exit with code 1 and a message naming the file and the
`reset` command. **Do not** silently re-baseline: that hides the problem and can swallow a
real new chapter. Unknown `version` is also an error. Write a migration if v1 ever
existed. The plan starts at v2, so just reject other versions.

**Single instance.** Take an exclusive `File::try_lock()` (std, Rust ≥ 1.89) on
`state.lock` next to the state file for the whole run. If the lock is held, exit with
code 5 ("another instance is running"). This prevents a scheduled `check` and a running
`watch` from both announcing the same chapter.

### 4.2 Pure decision function

```rust
fn decide(state: Option<&SeriesState>, snap: &SeriesSnapshot, policy: &Policy) -> Decision

enum Decision {
    Baseline { ids: Vec<String>, head_label: String },
    NoChange,
    Announce {
        groups: Vec<ChapterGroup>,   // oldest -> newest, each group = one POST (≤ 10 parts)
        folded: Vec<String>,         // ids marked announced without their own message
        held_back: usize,            // new but filtered out by require_published
    },
    Anomaly(String),                 // do not announce, do not change announced_ids
}

struct ChapterGroup {
    number: Option<u32>,             // 67; None for unnumbered entries
    parts: Vec<ChapterRef>,          // oldest -> newest, 1..=10
}
```

### 4.3 Algorithm per series per cycle

1. Fetch and parse. On any error, go to failure handling (§4.6). Nothing below runs.
2. `chapters` empty → error `EmptyChapterList`, which counts as a failure.
3. **Uninitialized** → `Baseline`: put **every** current id (including early-access ones)
   into `announced_ids` and log `baseline established at <head label>`. Send nothing.
   `--announce-on-first-run` instead baselines everything except `chapters[0]`, then
   continues to step 4 (for testing).
4. `new` = chapters whose id is not in `announced_ids`, in list order.
5. If `new` is empty → `NoChange`.
6. **Sanity guard:** if `new.len() > 20`, or `snap.len() < last_snapshot_len / 2` →
   `Anomaly` (site renumbered or served a broken page). Log a warning and send nothing.
   On the 3rd consecutive anomaly, **re-baseline** with a warning, so the series cannot
   stay stuck forever.
7. `require_published` filter: drop `new` entries with `published == None`. They are not
   added to `announced_ids`, so they are announced later when a date appears.
   `held_back` = number dropped.
8. Order the remaining entries oldest → newest (reverse list order) and **group split
   parts**:
   - The group key is the integer chapter number parsed from `label`: the first number in
     the label, matched by regex `(\d+)(?:\.(\d+))?`. "Chapter 67.3" → `67`, part `3`.
     "Chapter 68" → `68`, no part.
   - A group is a run of **consecutive** entries (in list order) with the same key.
     Example: `67.1 67.2 67.3 68.1 68.2 68.3` → groups `[67.1–67.3]`, `[68.1–68.3]`.
   - If the label has no parsable number (e.g. "Extra", "Special"), the entry is its own
     group.
   - A group with more than 10 parts is split into chunks of 10 (the Discord embed limit).
     Each chunk is a separate message.
   - Parts of one chapter that arrive in different cycles get separate messages (67.1
     today, 67.2 tomorrow → two messages). This is expected; do not hold parts back
     waiting for siblings.
9. Cap: `groups.len() <= max_messages_per_cycle` (default 3) → one message per group,
   oldest group first. Otherwise → announce only the **newest** `max_messages_per_cycle`
   groups. The ids of every older group go into `folded` (marked announced without a
   message). The first message sent gets footer `+N earlier chapters since last check`,
   where N is the folded part count.

### 4.4 Commit rules

- After **each** successful group POST (2xx), add all that group's part ids to
  `announced_ids` and persist immediately. The `folded` ids are added together with the
  first successful POST. A failure partway through a multi-group run then re-sends only
  the groups not yet delivered.
- On POST failure, stop announcing that series for this cycle. Unsent ids stay out of the
  set, so they are retried next cycle.
- Also persist once at the end of each cycle (`last_checked_unix`, failure counters,
  `etag`).
- `--dry-run` **never writes state**, not even a baseline. It prints what *would* happen,
  including "would establish baseline at X".

### 4.5 Accepted duplicate

Killing the process between a successful POST and the following state write can produce
one duplicate message. Document this. Do not build a pending-announcement journal unless
the human asks.

### 4.6 Failure handling and health alerts

- Any fetch/parse/HTTP failure increments `consecutive_failures`. A success resets it to 0.
- Health messages go **only** to `BESIDEME_HEALTH_WEBHOOK` (separate channel), never
  ping anyone, and are skipped (with a log line) when that variable is unset.
- When `consecutive_failures` reaches `alert_after_failures` and `alert_sent == false`,
  send **one** ops embed (grey) to the health webhook: `⚠ Watcher can't read <title>:
  <error kind>. Last success <relative time>.` Then set `alert_sent = true`.
- On the next success after an alert, send one `✅ Watcher recovered for <title>` embed
  and reset `alert_sent`.
- The announcement webhook returning `WebhookGone` also triggers one health alert ("the
  announcement webhook was deleted or revoked; update `.env`").
- This works in both `check` (schtasks) and `watch` mode, because the counters are
  persisted.

---

## 5. Discord message

POST to `<webhook>?wait=true[&thread_id=…]`. With `wait=true`, Discord returns `200` with
the message body, which confirms delivery.

**One message per chapter group** (§4.3). The message has `content` = the configured
`mention` (default `@everyone`) and **one embed per part**, oldest part first (max 10).
Example for a 67.1–67.3 drop (only the last embed shown in full):

```json
{
  "username": "Yamada",
  "content": "@everyone New chapter 67 (3 parts) of **Smoking Behind the Supermarket with You**",
  "allowed_mentions": { "parse": ["everyone"] },
  "embeds": [
    { "title": "Chapter 67.1 — 67th Smoke", "url": "https://global.manga-up.com/manga/261/130416", "...": "..." },
    { "title": "Chapter 67.2 — 67th Smoke", "url": "https://global.manga-up.com/manga/261/130419", "...": "..." },
    {
      "title": "Chapter 67.3 — 67th Smoke",
      "url": "https://global.manga-up.com/manga/261/130422",
      "description": "New chapter of **Smoking Behind the Supermarket with You**",
      "color": 3447003,
      "timestamp": "2026-11-23T12:00:00Z",
      "author": { "name": "Manga UP!" },
      "thumbnail": { "url": "https://global-img.manga-up.com/asset/J0O/ja/chapter/130422.webp?h=...&e=..." },
      "footer": { "text": "Early access — no general release date yet" }
    }
  ]
}
```

Rules:

- Title starts with the chapter label, so it is readable in the notification preview.
  Without a subtitle, the title is the label alone. The manga title goes in the
  description and uses `label` from config if set.
- `content` = `mention` + ` New chapter <n>` + ` (<k> parts)` when k > 1 + ` of **<title>**`.
  For an unnumbered group, use `New release: <label> of **<title>**`.
- `allowed_mentions` is **always** sent explicitly. It is `{"parse": ["everyone"]}` only
  when `mention == "@everyone"`, and `{"parse": []}` otherwise (always for health and test
  messages). Strip `@everyone` / `@here` / `<@…>` from every site-provided string before
  putting it in `content`, so only the configured mention can ever ping.
- README note: if the ping does not fire, the channel must allow the webhook to
  "Mention @everyone, @here and All Roles".
- Escape Discord markdown (`* _ ~ \` | >`) in site-provided text inside `description`.
  Strip control characters from all fields. Truncate on char boundaries (title 256,
  description 4096, footer 2048, author 256), with the total under 6000.
- `timestamp` = detection time (UTC, RFC 3339).
- Footer: early-access note and/or `+N earlier chapters since last check`, joined with
  ` · `.
- One POST per chapter group, sequentially, with a 1 s gap between them.
- `429`: read `retry_after` from the JSON body (seconds, may be fractional), falling back
  to the `Retry-After` header. Wait if it is ≤ 60 s and retry **once**. Otherwise give up
  for this cycle.
- `401/403/404` → `WebhookGone`: log a clear config-fix instruction (without the URL) and
  stop using that webhook for the rest of the process. Other 4xx → `WebhookRejected`
  (log the response body, truncated, since it usually names the bad field). 5xx/network →
  transient.
- `test-webhook` sends a clearly labelled test embed using the exact same code path, with
  no ping.

---

## 6. CLI

```
besideme-cli check        [--config P] [--state P] [--dry-run] [--json] [--announce-on-first-run]
besideme-cli watch        [--config P] [--state P]
besideme-cli test-webhook [--config P] [--series URL] [--health]   # default: every distinct announcement webhook once; --health tests the health webhook
besideme-cli list         [--config P] [--state P] [--offline]
besideme-cli reset        (<series-url> | --all) [--state P]
Global: -v / -q, --allow-any-webhook, --env-file P
```

- `check`: one pass over all series, sequential, **no retries within the run**.
- `watch`: long-running loop (§7). Ctrl-C (and SIGTERM on Linux) finishes or aborts the
  current request, persists state and exits 0.
- `list`: configured series, resolved site/key, initialized?, last announced label, last
  success, failure count, and (unless `--offline`) the current head chapter from the
  network. It also reports orphaned state entries (state keys with no config series).
- `reset`: removes the series' state entry so the next run re-baselines. It takes the lock.

Exit codes for `check`, worst wins: `1` config/state error > `5` another instance running
> `4` webhook error > `2` parse error > `3` network error > `10` news announced > `0` ok.
`watch` returns 0 on clean shutdown and 1 on startup config errors. Note in the README
that Task Scheduler shows exit code 10 as "last result 0xA", which is normal.

Logging: `tracing` to stderr plus a file in `dirs::data_local_dir()/BesideME/logs/`,
using `tracing-appender` daily rotation with `max_log_files(7)`. Size-based rotation is
not built into `tracing-appender` and is not worth a custom writer. Default level `info`,
`-v` = debug, `-q` = warn, and `RUST_LOG` overrides. `--json` switches `check` stdout to
one JSON object per series. Logs always go to stderr/file, so stdout stays clean for
scripting.

---

## 7. Politeness, robustness, scheduling

- All site GETs go through `http.rs`. Settings: `reqwest` with `rustls-tls`,
  gzip/brotli, `timeout_seconds` total timeout plus a 10 s connect timeout, max 2
  redirects, https only.
- **Response size cap 5 MB**, enforced by streaming the body. Do not call an unbounded
  `.text()`. Drop the body after parsing.
- Send the configured descriptive User-Agent. Do not imitate a browser (verified
  unnecessary, §1).
- **Conditional GET**: send `If-None-Match` with the stored `etag`. A `304` counts as
  success + `NoChange`. Expect the ISR ETag to change often; this is a cheap bonus, not a
  requirement.
- Failure scheduling in `watch` mode (the next attempt time per series, never immediate
  in-request retries):
  - timeout / connect error / 5xx → 30 s, 90 s, 270 s, then the normal interval.
  - 403 / 429 → `max(Retry-After, interval)`, and double the interval per further
    consecutive 403/429 up to 6 h. Never retry a block quickly.
  - parse errors → normal interval. Retrying faster will not fix a schema change.
  - timeout on a series that has **never** succeeded → log `URL is probably wrong
    (Manga UP hangs instead of returning 404)`.
- One series failure never stops the others.
- **Scheduler:** each series has a `next_due` wall-clock time (interval ± jitter). The loop
  sleeps `min(time until earliest next_due, 60 s)` and re-checks the wall clock, so after a
  laptop resumes from sleep or hibernate the overdue checks run promptly instead of
  waiting out a stale monotonic timer. Due series run sequentially with ≥ 2 s between
  requests. On startup, run the first check immediately.
- Windows scheduling (document in README, don't implement a service):
  - Simple: `schtasks /create /tn "BesideME" /sc minute /mo 30 /tr "\"C:\tools\besideme-cli.exe\" check"`.
    A console window flashes on every run. Document configuring the task as "Run whether
    user is logged on or not", which runs it hidden.
  - Long-running: Task Scheduler "At log on" running `watch`, hidden, "restart on
    failure". Do **not** combine both. The instance lock (§4.1) makes accidental overlap
    safe, but the second instance just exits with code 5.
- Memory budget: well under 20 MB RSS. Use the `current_thread` tokio runtime. Nothing
  here needs multi-threading.

---

## 8. Testing

No test may touch the network. Use `wiremock` + `#[tokio::test]` and `tempfile`.

1. **Fixtures**: build them **synthetically** in the real shape. Do not commit the
   330 KB real page (copyrighted text, noise). `tests/fixtures/` holds:
   `manga261_base.html` (5 chapters, newest 2 without `published`),
   `manga261_new.html` (+1 chapter), `manga261_head_removed.html`,
   `manga261_attr_reordered.html` (`<script type=… id="__NEXT_DATA__" nonce=…>`),
   `manga261_160.html` (generated in the test, >150 entries), and
   `manga261_page_error.html` (`pageProps.error` set, `data` null).
2. **Parser**: title, ordered chapters, ids as strings, absolute urls and thumbnails;
   missing `__NEXT_DATA__` → `NextDataMissing`; malformed JSON → `InvalidJson` (never
   empty Ok); no `chapters` → `SchemaMismatch`; empty list → `EmptyChapterList`; chapter
   without `subName` / `published` / `imageUrl` parses; unknown extra fields are ignored;
   attribute reordering is handled; series key parsing for `/manga/261`, `/manga/261/`,
   `/manga/261?x=1`, plus rejection of `/manga/abc` and `/manga/261/130422`.
3. **`decide()`** (pure): uninitialized → Baseline with all ids; identical → NoChange;
   1 new → that one; 3 new → 3, oldest first; 5 new with cap 3 → newest only, 4 folded;
   **head removed → NoChange (no false announce)**; reordered list → NoChange;
   `require_published` + new undated head → nothing, `held_back = 1`, id not marked;
   same chapter later gains a date → announced once; 25 new → Anomaly; list shrinks by
   more than half → Anomaly; `--announce-on-first-run` → exactly 1.
   **Grouping:** `67.1 67.2 67.3` → 1 group of 3; `67.1–67.3 + 68.1–68.3` → 2 groups,
   67 first; `67.3` alone (67.1–67.2 already announced) → 1 group of 1; `68` (no part) +
   `67.2` → 2 groups; unnumbered "Extra" → its own group; 12 parts of one chapter → 2
   messages (10 + 2); 5 groups with `max_messages_per_cycle = 3` → newest 3 groups sent,
   older 2 folded, footer count correct; label parsing for "Chapter 67.3",
   "Chapter 68", "Ch. 5.1", "Extra".
4. **Discord**: golden JSON (`serde_json::Value`) for normal, multi-part group,
   early-access, folded and no-subtitle cases; `content` starts with `@everyone` and
   `allowed_mentions.parse == ["everyone"]` for announcements; with `mention = ""` there
   is no content ping and parse is `[]`; health/test payloads never ping; a site title
   containing `@here` is neutralized; markdown escaping;
   truncation on multi-byte chars; webhook newtype `Debug`/`Display` redacted (also
   assert that a captured `tracing` log never contains the token); 429 with body
   `retry_after: 0.5` → one retry; 429 with 120 s → give up; 404 → `WebhookGone`;
   400 → `WebhookRejected`.
5. **Orchestrator** (mock site + mock webhook, temp state dir): first `run_check` → 0
   POSTs, state written; second → 0 POSTs; swap to the new fixture → exactly 1 POST,
   exit 10; webhook returns 500 → state unchanged, next run re-sends; 3 new groups with
   the webhook failing on the 2nd → the 1st is committed and only the 2nd and 3rd are
   re-sent; site returns garbage `alert_after_failures` times → exactly one alert POST
   **to the health mock (none to the announcement mock)**, then recovery → one recovery
   POST; health webhook unset → no alert, warning logged; `--dry-run` → no state file
   created; held lock → exit 5.
   **Env:** webhooks are read from a temp `.env`; a real env var overrides `.env`;
   `webhook_env` per-series override; a `discord_webhook` key in TOML is rejected; the
   token never appears in logs or `Debug` output.
6. **State**: atomic write round-trip; corrupt file → error (not re-baseline); unknown
   version → error; pruning at the cap.
7. **Live contract test**, `#[ignore]`: fetches the real `/manga/261` and asserts the
   §1 assumptions. Run it manually with `cargo test -- --ignored` to detect site drift.
8. Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
   on Windows **and** Linux (GitHub Actions matrix if the repo is pushed).

Manual acceptance (the human will do this):

- `check --dry-run` against the real `/manga/261` prints "would establish baseline at
  Chapter 67.3" (or the current head), makes no POST and creates no state file.
- `test-webhook` delivers to the real Discord server.
- After a real first run, deleting the newest id from `announced_ids` in the state file
  produces exactly one message naming that chapter, with a working link.

---

## 9. Cargo / project conventions

```toml
[package]
edition = "2024"
rust-version = "1.89"            # std File::try_lock

[lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"            # applies to the lib crate's public items

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
module_name_repetitions = "allow"
missing_errors_doc = "allow"     # errors documented at the type level instead

[profile.release]
strip = true
lto = "thin"
codegen-units = 1
panic = "abort"
opt-level = "s"
```

Dependencies (keep the tree small; no `scraper`/`select`, because extraction is JSON only):

```
tokio        default-features = false, features = ["rt", "macros", "signal", "time"]
reqwest      default-features = false, features = ["json", "gzip", "brotli", "rustls-tls", "stream"]
futures-util (only for the streaming size cap; or use `Response::chunk()` and skip it)
serde (derive), serde_json, toml
clap (derive)
thiserror (per module), anyhow (CLI boundary only)
tracing, tracing-subscriber (env-filter, fmt), tracing-appender
dirs
dotenvy                          (.env loading)
regex-lite                       (chapter-number parsing; or hand-rolled, no full `regex`)
fastrand                         (jitter; smaller than rand)
time (formatting, macros)        (RFC 3339 only)
[dev-dependencies] wiremock, tempfile, tokio (full for tests)
```

Code style: no `unwrap`/`expect`/`panic!` outside tests; `thiserror` per module, `anyhow`
only in `main.rs`; every public lib item documented; no `unsafe`.

---

## 10. Build order for the implementer

1. Scaffold the lib + bin crate. Add `.gitignore`, `.env.example` and
   `config.example.toml` first. Then `paths`, `config` + `.env` loading (with validation
   tests) and `state` (atomic write, lock, tests). No network.
2. `sites/manga_up.rs` parser against synthetic fixtures; all parser tests green.
3. Pure `decide()` + tests.
4. Discord payload builder (pure) + golden tests, then the client + wiremock tests, then
   `test-webhook`.
5. `http.rs` (UA, timeout, size cap, ETag) + wiremock tests.
6. `run_check` + `check` command + orchestrator tests. Then `watch` scheduler with
   backoff, jitter, wall-clock re-check, health alerts and Ctrl-C.
7. `list`, `reset`.
8. README: install, config, Windows scheduling (both modes, hidden window), exit codes,
   the accepted-duplicate note, the "secret in config" warning, and "How to add a site"
   with a ~30-line stub `Site` implementation plus fixture + test.
9. Only then: run the `#[ignore]` live contract test, `check --dry-run` against the real
   URL, a real `test-webhook`, and hand over for the first real `check`.

Definition of done: all tests green on Windows (and Linux if available), clippy clean,
fmt clean, release binary under ~8 MB, README complete, `cargo test` makes no network
access, and the manual acceptance steps in §8 are listed in the README.

---

## 11. Decisions already made (change only if the human says so)

- Polling every 30 minutes with ±2 min jitter, minimum 10 minutes.
- Notify on listed-but-undated (early access) chapters unless `require_published = true`.
- Baseline on first run, never a backlog dump. Early-access chapters present at baseline
  are not announced later when they get a date.
- Detection by **set of announced ids**, not a last-id pointer.
- State in JSON under the local app-data dir, not SQLite; single-instance lock.
- Discord: one message per chapter group (split parts 67.1–67.3 together, 68.x
  separately), one embed per part, at most 3 group messages per series per cycle (older
  groups folded), `@everyone` ping in `content`, `allowed_mentions` explicit.
- Webhooks only in `.env` (gitignored). `config.toml` holds no secrets.
- Health alerts after 6 consecutive failures go to a **separate** health webhook, without
  a ping.
- Corrupt state → hard error, never a silent re-baseline.
- `mangaup` is the only registered site; sites are pure parsers.

- HTTP stack: tokio (`current_thread`) + reqwest.

## 12. Open questions

None. If something is genuinely ambiguous during implementation, choose the option most
consistent with §0 and §11, note it in the final report, and keep going. Do not change
anything listed in §11.
