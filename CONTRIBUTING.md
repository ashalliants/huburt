# Contributing

Thanks for wanting to help. Huburt is small on purpose, so the best contributions keep it that way.

## Good first things

- **Try it on Windows** and [open an issue](https://github.com/ashalliants/huburt/issues) with what you see, especially around
  unread counts, notification buttons and window layout. It is the least-tested part.
- **Bugs with a clear way to reproduce.** Say your OS version, the service, and what you expected. The log
  (`Huburt.log`, see [Developing](docs/DEVELOPING.md#logs-settings-and-test-switches)) contains counts and status only, so it
  is safe to attach.
- **Fixes for Slack or Gmail changing their web endpoints.** The pollers use the same endpoints the web apps use; when one
  changes, the fix is a small change in `slack.rs` or `gmail.rs` plus a test with a canned response.

## Adding a service

Adding Telegram, Discord and so on is welcome if it can keep the memory promise. A service is an entry in
`src-tauri/src/services.rs` (URL, hosts, how to read its unread count) and, ideally, a light poller like `slack.rs` so its page
can be unloaded. Without a poller it can only use the wake cycle that WhatsApp uses. Please open an issue first to agree the
approach.

## Pull requests

1. `cd src-tauri && cargo clippy --all-targets -- -D warnings && cargo test` must pass. CI runs both on macOS and Windows.
2. Keep one change per pull request. Tests go with the change.
3. If you change anything a user sees, say how you checked it (screenshots help). The UI states can be previewed in any browser:
   serve the repository root and open `ui/index.html?dark#mock=badges` (also `waking`, `error`, `off`, `signedout`).
4. Don't commit credentials, tokens, cookies or logs from your own accounts.

On macOS, read [the note about the launcher](docs/DEVELOPING.md#why-the-macos-build-has-two-parts) before touching
`src-tauri/src/main.rs`, `Info.plist` or the icon.
