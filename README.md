# BesideME

<img src="assets/yamada-256.png" width="88" height="88" alt="Yamada, the BesideME app icon">

**Keep up with manga releases on Manga UP! Global.** BesideME is a Windows desktop app with a command-line checker. It tracks what’s new, opens official chapter links, and sends updates to Discord.

[**Download for Windows**](https://github.com/ppolkadot/BesideME/releases/latest) · [Configuration and CLI](docs/configuration.md) · [Build and test](docs/development.md)

## Get started

1. Download `BesideME-Setup-<version>.exe` from [GitHub Releases](https://github.com/ppolkadot/BesideME/releases/latest). Setup installs for your Windows user and offers background checks every 30 minutes.
2. Press **Ctrl+K** or select **Add manga**. Search for a title or paste its Manga UP! series link, then add it to your shelf.
3. In **Settings**, paste your Discord updates webhook for **Yamada**. Add a separate health webhook for **Sasaki** if you want failure and recovery notices. Use **Send test** to verify each destination, then save.
4. Select **Check now**. The first check records existing chapters quietly. Later checks show newly discovered chapters on your shelf and announce them on Discord.

## Your release shelf

| View or action | What it does |
| --- | --- |
| **All / Unread / Issues** | Browse followed titles, find unread releases, or see titles that need attention. |
| **Open chapter / Mark read** | Open the newest unread chapter on Manga UP! or clear the title’s unread badge. You can open a chapter and keep its badge. |
| **Retry / Check now** | Retry one title or check the full list. A manual check shows progress and shares the background check’s lock and delivery rules. |
| **Background status** | See the last check and the next scheduled time when Windows reports one. |

**Ctrl+K** opens Quick Add, **Esc** returns focus, and **Tab** moves between controls. The three window buttons close, minimize, and zoom. Use **Ctrl++**, **Ctrl+−**, or **Ctrl+0** to adjust the UI scale.

## Discord and local data

Webhook URLs stay in a local `.env` file and are masked by default. Each **Send test** click attempts one labeled message for that destination with mentions disabled. Chapter announcements use the configured mention setting. Set `mention = ""` in `config.toml` for quiet notifications.

Setup stores settings in `%APPDATA%\BesideME` and history in `%LOCALAPPDATA%\BesideME` by default. Upgrading retains your watchlist, webhook settings, and history. See the [configuration guide](docs/configuration.md) for portable setups, older Manga Watcher installs, and troubleshooting.

## Build from source

Use Rust **1.95 or newer**. On Windows, Setup also requires the MSVC tools, Windows SDK, and Inno Setup 6.7 or newer.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File installer\build.ps1
```

The script builds the binaries and writes the installer to `dist\`. For a CLI-only build, use `cargo build --locked --release --no-default-features --bin besideme-cli`.

[Developer guide](docs/development.md) · [Redesign record](docs/redesign-plan.md) · [MIT license](LICENSE)
