# Huburt

Slack, Gmail and WhatsApp in one window, with the smallest memory footprint we can manage. A like-for-like rewrite of the
Swift app in `../Hub` using Tauri 2: Rust for the logic, each platform's own web engine for the pages (WebKit on macOS,
WebView2 on Windows), so there is no bundled browser.

Why it's small: pages are loaded only while you look at them. A sleeping service is just its unread poller (a few HTTP
requests) or, for WhatsApp, a short wake every 5 minutes. With the window hidden and nothing loaded, the whole app is the
Rust process plus WebKit's two shared helper processes.

## Build and install

    ./build.sh             # build, sign, and install the core library into ~/Library/Application Support/Huburt/
    ./build.sh --install   # also copies Huburt.app to /Applications/Huburt.app (never touches /Applications/Hub.app)

**Why there are two parts.** macOS remembers "Always Allow" for WhatsApp's encryption key (stored by WebKit in the keychain)
by the app's exact code, so signing with a stable certificate is not enough: every normal rebuild changed the hash and meant
a prompt. So, like the Swift Hub, Huburt is split:

- `src-tauri/src/main.rs` is a tiny launcher. It is the signed `Huburt.app` executable and never changes. It loads the real
  app and calls `huburt_main()`.
- `src-tauri/src/lib.rs` is the real app, built as `libhuburt_core.dylib` and loaded by the launcher from
  `~/Library/Application Support/Huburt/` (`HUBURT_CORE=<path>` overrides it).

`build.sh` only replaces the library, and leaves an installed `Huburt.app` alone while the launcher's hash is unchanged.
(Changing `main.rs`, `Info.plist` or the icon changes the hash and costs one "Always Allow". Don't edit the launcher casually.)
The signing certificate is "Hub Local Signing", the same as the Swift app's (ad-hoc signing makes every build look new).

Needs Rust (stable) and `cargo install tauri-cli --version "^2" --locked`. Tests: `cd src-tauri && cargo test`
(50 tests: the Slack and Gmail pollers against canned responses, the load/unload/wake-cycle state machine, and link rules).

## First run

Huburt keeps its own logins (it does not share the Swift app's), so sign in to Slack, Gmail and WhatsApp (scan the QR
code) once. macOS also asks to allow notifications.

## Layout

- `src-tauri/src/main.rs`: the launcher (see above). Everything else in `src-tauri/src/` is the core library.
- `src-tauri/src/hub.rs`: the brain. Per-service state and a 250 ms scheduler: lazy load/unload, polling, WhatsApp wake
  cycle, "show as online", daily greeting, muting and pausing. Mutate under the lock, collect actions, run them after.
- `src-tauri/src/slack.rs`, `gmail.rs`, `http.rs`: the pollers (blocking HTTP, worker threads), tested with a mock `Http`.
- `src-tauri/src/webviews.rs`: the window, the sidebar web view and the per-service web views (layout, links, downloads).
- `src-tauri/src/services.rs`: the service list and the injected JavaScript (dark mode filter, notification shim).
- `src-tauri/src/menus.rs`, `tray.rs`, `banner.rs` (native notification banners with Reply / Mark as Read), `native.rs`.
- `ui/`: the sidebar and the "waking up / can't reach / turned off" covers (plain HTML/CSS/JS, no build step).
- `src-tauri/capabilities/`: what the pages may call. Service pages can only report notifications, probe results and
  external links.
- `tools/measure.py`, `tools/memtest.sh`: memory measurement (see below).

## Logs and test switches

- Log: `~/Library/Logs/Huburt.log`. Counts and status only; never message text or tokens.
- Settings: `~/Library/Application Support/Huburt/settings.json` (`HUB_SETTINGS_DIR` overrides the folder).
- Launch hidden (menu bar only): `--hidden`.
- `HUB_IDLE_SECONDS`, `HUB_WAKE_INTERVAL`, `HUB_WAKE_AWAKE`, `HUB_WAKE_SETTLE`: shorten the timers.
- `HUB_TEST_STEPS="select:slack:20,probe:slack:5,hide:30,show:5"` walks through states (seconds each; also
  `online:work`, `disable:gmail`, `enable:gmail`). `HUB_TEST_GREETING=1` sends the daily greeting after 8 s.
- Run the bundle with `open -n --env NAME=value --stdout f --stderr f Huburt.app` (running the bare binary from a
  sandboxed shell crashes in macOS image decoding).

## Memory

Measured with `tools/measure.py` (macOS physical footprint, the "Memory" column in Activity Monitor, summed over the app
and the WebKit helper processes it owns), Nov 2026, both apps launched hidden in the menu bar:

| State | Swift Hub | Huburt |
|---|---|---|
| Idle, window hidden, every page unloaded | 37.5 MB (2 processes) | 26-28 MB (1 process) |
| App process alone | 28-39 MB | 26-32 MB |
| Window open | no extra process | +25 MB (the sidebar's web view) |

Pages cost the same in both, because both use WebKit: a signed-out Slack page was 100-200 MB, WhatsApp's QR page about
70 MB, a Gmail sign-in page about 45 MB. When the window is hidden the sidebar's web view is destroyed, which is why the idle
numbers don't include it. Caveats: the Swift numbers are with real signed-in accounts and Huburt's are signed out (so
its Slack and Gmail pages were only loaded briefly to look for credentials). Keeping Slack loaded for "show as online"
costs 0.6-1.4 GB in either app, since it's the page itself.

`tools/memtest.sh` repeats the walk-through (select each service, then hide the window, measuring after each step).

## Known differences from the Swift app

- Separate logins (see First run).
- On launch Slack and Gmail pages are loaded once, briefly, to pick up cookies and the Slack token (the Swift app could read
  cookies without a page). They are kept in memory only.
- "Can't reach" is detected by a 25 s timeout and retried every 30 s (no "network is back" hook).
- Links: clicks on links to other sites open in the browser, and `window.open` is handled natively; redirects that the page
  makes itself stay in the app, as in the Swift app.
- The yellow minimise button minimises normally (it doesn't hide to the menu bar); Cmd+H, Cmd+W and the red cross do.
  Cmd+Q does nothing, as only "Quit Huburt" quits. The window position isn't remembered.
- The menu bar icon is drawn in code (white on dark, black on light menu bars). It doesn't follow the wallpaper tint.

## Not tested

Everything that needs real accounts was only tested against canned responses: the Slack and Gmail pollers, credential pickup,
banner buttons, the reply and mark-as-read actions, WhatsApp's wake cycle with a signed-in session. Windows is not built
yet (the data-folder-per-service branch is in place; banners, the start-at-login entry and the tray need Windows versions).
