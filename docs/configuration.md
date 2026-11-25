# Configuration and CLI

Begin with the [Windows quick start](../README.md#get-started). These settings also apply to the standalone checker.

## Discord destinations

In Discord, open the destination channel’s **Edit Channel → Integrations → Webhooks** and copy its URL. You can also copy [`.env.example`](../.env.example) to `.env` and fill in these variables.

| Variable | Purpose |
| --- | --- |
| `BESIDEME_WEBHOOK` | Yamada’s chapter announcements. Normal checks need this or a per-series override. |
| `BESIDEME_HEALTH_WEBHOOK` | Sasaki’s optional failure and recovery alerts. Use a separate webhook from Yamada’s. |

The Settings screen tests the URL currently entered, including unsaved edits. An accepted test means Discord accepted the labeled message. Replace a revoked URL from the channel’s Integrations settings. Tests and health alerts disable mentions. Chapter announcements follow `mention` in the config, which defaults to `@everyone`. Set it to `""` to disable the ping.

Keep URLs in `.env` or supply them through the process environment. TOML accepts `webhook_env` for the **name** of an additional variable. It rejects credential fields such as `discord_webhook`. Environment variables take precedence over the dotenv file. Legacy `MANGA_WATCHER_WEBHOOK` and `MANGA_WATCHER_HEALTH_WEBHOOK` names are accepted for earlier setups.

## Watchlist and files

The installer starts with an empty watchlist. To configure the CLI by hand, copy [`config.example.toml`](../config.example.toml) to `config.toml` and edit its `[[series]]` entries. Each entry needs a Manga UP! Global series URL. The sample explains labels, per-series webhook variables, Discord thread IDs, polling intervals, and published-only filtering.

| File | Default Windows location | Purpose |
| --- | --- | --- |
| `config.toml` | `%APPDATA%\BesideME` | Watchlist and checker settings. |
| `.env` | Next to the active config | Discord credentials. |
| `state.json` and `state.lock` | `%LOCALAPPDATA%\BesideME` | Check history, delivery IDs, and release discovery under an exclusive lock. |
| `reads.json` and `reads.lock` | Next to the active state file | Local read markers. |
| `logs\` | `%LOCALAPPDATA%\BesideME` | Daily logs, up to seven files. |

On Linux the usual defaults are `~/.config/BesideME` for settings and `~/.local/share/BesideME` for data, subject to XDG overrides.

Configuration resolution starts with `--config <path>`, then checks the BesideME and legacy Manga Watcher user config directories, the executable directory, and the working directory. The dotenv search checks `--env-file <path>`, the directory of the active config, and the working directory. State uses `--state <path>` when supplied. It falls back to the earlier Manga Watcher state file if the BesideME state file does not exist.

For a portable CLI setup, place the executable, `config.toml`, and `.env` together. Pass `--config` explicitly if a user-wide config exists and you want the portable copy.

## CLI commands

```text
besideme-cli check [--config PATH] [--state PATH] [--dry-run] [--json] [--announce-on-first-run]
besideme-cli watch [--config PATH] [--state PATH]
besideme-cli list [--config PATH] [--state PATH] [--offline]
besideme-cli test-webhook [--config PATH] [--series URL] [--health]
besideme-cli reset [--state PATH] <SERIES_URL|--all>
```

Use `--help` on any command. A dry-run fetches and parses chapters without writing state or sending messages. Use `list --offline` to inspect local state without fetching. **Reset** discards the specified checker history and the next check establishes a new baseline. `watch` continuously polls and records a summary for each completed pass.

| CLI exit | Meaning |
| --- | --- |
| `0` | Check completed without new announcements. |
| `10` | A new chapter announcement was sent successfully. |
| `1` | Configuration or state error. |
| `2` | Provider parse error. |
| `3` | Provider network error. |
| `4` | Discord delivery error. |
| `5` | Another process holds the state lock. |

A run with several results uses severity order `1 > 5 > 4 > 2 > 3 > 10 > 0`. The windowless `BesideME.exe --check` command returns `0` on success and `1` on failure.

## Background checks and upgrades

Setup can register a user-level **BesideME** task that runs `BesideME.exe --check` every 30 minutes. Rerun Setup or configure the task if your scheduling preferences change. Run either scheduled checks or continuous CLI `watch` against a given state file. Single-title retry waits at least 30 seconds after that title’s last check and respects provider rate limits.

For scripted installs, pass `/VERYSILENT /TASKS=background` to Setup. The optional `/WEBHOOK` and `/HEALTHWEBHOOK` switches can initialize a missing dotenv file. Use Settings for later edits. When upgrading from Manga Watcher, Setup keeps the existing installation directory and copies earlier config and dotenv files when their BesideME counterparts are missing. The legacy state fallback keeps earlier history readable. The uninstaller removes the task and offers to remove current BesideME user data in interactive mode. Legacy source directories are kept.

Existing state files remain readable and previously announced chapters do not become unread. Rolling back to an older build that rewrites the state may discard discovery and run-summary fields.
