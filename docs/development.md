# Development and verification

## Build and test

Use Rust 1.95 or newer. Windows packages also need the MSVC build tools, Windows SDK, and Inno Setup 6.7 or newer.

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --no-default-features --bin besideme-cli
powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1
```

Setup reads the version from [`Cargo.toml`](../Cargo.toml), uses the checked-in lockfile, and writes the installer to `dist\`. The package includes both executables, user guides, and the MIT and Poppins font licenses. Run `py -3 -I assets/make_icons.py` with Pillow installed to regenerate the icon and DPI-specific wizard artwork.

## Isolated smoke tests

Start from a shell with real webhook environment overrides cleared. Process environment variables take precedence over profile dotenv values. Use a temporary profile, dummy webhook destinations, and local fixtures. The two ignored Manga UP! integration tests are opt-in and contact the live provider.

| Environment variable | Purpose |
| --- | --- |
| `BESIDEME_PROFILE_DIR` | Root for default config, dotenv, state, read markers, and logs. Explicit CLI file paths still take precedence. |
| `BESIDEME_SITE_URL` | HTTPS fixture endpoint or loopback HTTP server for Manga UP!. |
| `BESIDEME_ALLOW_ANY_WEBHOOK=1` | Accept local mock webhook URLs. Use dummy destinations. |
| `BESIDEME_TASK_NAME` | Query a temporary Windows task in the status strip. |

Use a disposable Windows profile or VM for install, upgrade, and uninstall tests. Check keyboard navigation, title-bar controls, native display scaling from 100% to 200%, and scheduled execution across sign-out. Use mock Discord destinations for successful delivery, revoked endpoints, and changes to a URL while its test runs.

## Architecture

- [Provider adapters](../src/sites/mod.rs) resolve official links and parse chapter metadata. The checker extracts HTML and JSON without a browser session. Manga UP! Global is the shipped provider.
- [Detection](../src/detect.rs) produces baseline, no-change, anomaly, and announcement decisions. [The checker](../src/watcher.rs) applies them under the state lock. If the process stops after Discord accepts a group and before the state write completes, that group can be sent again.
- [Checker state](../src/state.rs) keeps up to 24 discovered groups per title. The Discord message cap does not limit shelf discoveries. Silent baselines, anomaly re-baselines, and dry-runs create no new unread entries.
- [Read markers](../src/reads.rs) retain up to 200 identifiers per title under a separate lock. [The desktop app](../src/bin/gui) reads these files and runs network work in background tasks.
- [Discord delivery](../src/notify/discord.rs) controls retries, embed limits, and mention policy. [Webhook tests](../src/webhook_test.rs) report outcomes independently for each destination.

To prototype a provider, implement the `Site` trait, register the adapter, define its official URL boundary, and add fixtures for normal, empty, error, and reordered chapter lists. The [redesign record](redesign-plan.md) documents the desktop scope. The [original implementation prompt](../IMPLEMENTATION-PROMPT.md) retains the first checker design for reference.
