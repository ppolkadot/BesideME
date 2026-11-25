# BesideME redesign proposal

BesideME should open on the manga you follow and make three questions easy to answer. What has a new chapter, when did the last check finish, and what needs attention?

## Design direction

Use a dark, cover-led reading shelf. Start with an ink canvas (`#0F1115`), a raised surface (`#161920`), bright text (`#F2F2F7`), muted text (`#AAB0BA`), and warm coral (`#FF817A`) with dark text on filled buttons. Verify contrast in the rendered states. Keep Poppins Regular for body copy and Poppins SemiBold for titles. Use an 8-point spacing grid, aligned chapter metadata, and stable 16:9 cover slots. Preserve the custom traffic-light controls with accessible names and visible keyboard focus.

UI/UX Pro Max identified minimalist hierarchy, a restrained accent, visible keyboard focus, inline validation, and actionable empty states. Its general design-system query suggested a promotional page, so this proposal applies the verified desktop-relevant guidance from the targeted style and UX searches. Gemini 4 Neon contributed the unread shelf, status strip, unified Quick Add, separate webhook tests, and an optional tray mode. The phases below adapt those ideas to the current Rust and egui architecture.

## Implementation order

| Phase | Implementation | Acceptance |
| --- | --- | --- |
| 1. Release shelf | Split the existing watchlist and add panels. Give the list the body of the main window. Add All, Unread, and Issues filters. Record a bounded set of newly discovered chapter groups and their official links in the checker's versioned state. Store per-title read markers separately so GUI actions and scheduled checks can overlap. | Upgrading existing state and silently baselining a new series create no unread badges. A subsequent discovery appears once across restarts and retries. Open Chapter and Mark Read are separate keyboard-accessible actions. Cover placeholders hold row height and failure states keep the title visible. |
| 2. Quick Add | Open a single panel with the Add button or Ctrl+K. Route a supported URL to the existing preview service and other input to the cached Manga UP! catalog search. Keep the Add action explicit and associate asynchronous responses with their input. | Escape returns to the previous focus target. The keyboard can pick and add a result. Duplicate series show Already watching. A stale preview cannot be added after the text changes. Catalog failure still allows paste-by-link. |
| 3. Check status | Persist a small run summary under the same lock as checker state. Expose last attempt, last success, and per-title failure reason. Show progress for manual runs and a next-run time only when the Windows task exists and reports one. Prototype single-title retry against the same rate and delivery rules as scheduled checks. | A scheduled run's result appears without restarting the GUI. Disabled and unavailable task states differ from healthy idle. In-progress checks leave navigation usable. A retry targets only the chosen series and cannot bypass the instance lock. |
| 4. Discord verification | Extend the current Settings screen with independent Send Test actions for Yamada and Sasaki. Reuse the existing no-mention test payload and show the last test outcome for each destination. Clear stale results when a URL changes. | Each test sends one clearly labeled message only after a click. Success is shown only after Discord accepts delivery. Invalid or revoked destinations receive inline, actionable feedback. URLs stay masked by default and never appear in logs or diagnostic output. |

## Release checks

- Run the app with an empty watchlist, a populated watchlist, and long or non-Latin titles. Verify list focus order, high-contrast status text, and readable settings at 100%, 125%, 150%, and 200% Windows scaling.
- Exercise existing installs, portable and installed settings, upgrade from older state files, and the CLI-only build. Cover new state transitions, duplicate suppression, and Discord responses with local fixtures.
- Perform a keyboard-only walkthrough of adding a manga, opening a chapter, checking updates, and saving settings. Verify use of the title-bar buttons with Windows accessibility tooling.
- Build and run the Windows installer with and without background checks. Confirm the desktop and headless check agree on the watchlist and webhook settings. Check scheduling after a sign-out and report a next run only when Windows can provide it.

Keep the next release focused on Manga UP! and official browser links. Defer tray mode and Windows toasts until the scheduled-task lifecycle and the traffic-light close behavior have been verified together. Defer an in-app reader, other manga providers, cloud sync, and theme editing.

## Implementation notes

All four phases are implemented; the README describes the behavior. Choices made while implementing the plan:

- **State.** `state.json` stays at schema version 2. The new per-title `discovered`, `last_error` and `retry_not_before_unix` fields and the top-level `last_run` are optional, so existing files and installs load unchanged and older binaries ignore them. Unread markers live in `reads.json` under `reads.lock`, never in `state.json`, so the app and a scheduled check can write at the same time.
- **Discovery.** A group is recorded when a check finds it unannounced on a baselined series, keyed by its newest part. Re-detection (restart, delivery retry, dry run, a group that grows) is idempotent; baselines and re-baselines record nothing. Links are validated by the provider (`Site::is_official_url`) and fall back to the official series page; the app validates again before opening a link.
- **Run summary.** Full, non-dry runs write a start record and a finish record under the instance lock. The app derives the strip from per-title state plus that record, so a single-title retry is reflected without replacing the run.
- **Task status.** Read-only PowerShell query of the `BesideME` task (exists, enabled, next run). A next run is shown only when Windows reports one.
- **Window sizing.** The window keeps its 720 × 680 size but opens smaller on screens that cannot hold it. Very small or heavily zoomed windows scroll the whole page.
- **Skills.** The UI/UX Pro Max skill was not installed in the implementation environment, so the plan's design rules (palette, 8-point grid, 16:9 covers, visible focus, inline validation, actionable empty states) were applied directly and contrast is asserted by unit tests.
