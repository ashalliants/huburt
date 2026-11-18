# Developing Huburt

Huburt is a [Tauri 2](https://tauri.app) app: Rust for the logic and the native parts, and each platform's own web engine
(WebKit on macOS, WebView2 on Windows) for the pages. There is no bundled browser and no JavaScript build step.

## Build and test

You need Rust (stable, <https://rustup.rs>). For the app bundle, also the Tauri CLI:
`cargo install tauri-cli --version "^2" --locked`.

```sh
cd src-tauri
cargo test                       # unit tests (the same ones CI runs on macOS and Windows)
cargo clippy --all-targets -- -D warnings

cd ..
./build.sh                       # macOS: build, sign, install the core library (see below)
./build.sh --install             # macOS: also copy Huburt.app to /Applications/Huburt.app
cargo tauri build --bundles nsis # Windows: installer in src-tauri/target/release/bundle/nsis/
./scripts/package-macos.sh       # macOS: the downloadable zip in dist/ (core inside the app, ad-hoc signed)
```

CI (`.github/workflows/ci.yml`) lints, tests and builds on macOS and Windows for every pull request. Pushing a tag like
`v0.1.1` (it must match the version in `tauri.conf.json`) runs `.github/workflows/release.yml`, which builds the Windows
installers and the macOS zip, attaches them to a GitHub release together with SHA-256 checksums, and marks 0.x releases as
pre-releases. The macOS zip is also built (and uploaded as an artifact) on every pull request.

## How it works

**Pages load on demand.** `hub.rs` keeps a small state machine per service and runs it from a 250 ms scheduler. Selecting a
service loads its page (a web view); leaving it starts a 5-minute timer, after which the page is destroyed and its memory
returns to the system. A service you keep looking at, or that is set to stay online, is never unloaded.

**Unread counts without a page.** While a page is unloaded, a *poller* (`slack.rs`, `gmail.rs`) makes small HTTPS requests with
your own session:

- Slack: the `client.counts` endpoint the Slack web app uses, every 30 seconds, plus a few lookups to turn a new mention into a
  banner ("Sam in #ops: ..."). Reply and Mark as Read from a banner call Slack's `chat.postMessage` and `conversations.mark` the same way.
- Gmail: the inbox Atom feed, every 60 seconds.

Pollers need cookies (and, for Slack, a workspace token) that only a loaded page has, so Huburt loads each page once at start
for a few seconds to pick them up. They are kept in memory only, and refreshed from the page before each poll while it is
open. Pollers are blocking code run on worker threads, and are tested against a mock HTTP layer.

**WhatsApp has no light check.** It follows a sleep cycle instead: wake the page for up to 45 seconds every 5 minutes, let it
sync (the page itself raises notifications through a shim that turns web notifications into native banners), then unload.

**Window layout.** Huburt's own page (`ui/`) runs in two web views that never overlap a service page: the sidebar (a strip on
the left) and a cover shown over the content area only while the selected service isn't ready ("waking up", "can't reach",
"turned off"). The selected service's page sits in the content area once it has drawn; until then it renders off-screen so it
isn't throttled. With the window hidden, Huburt's own web views are destroyed too.

**What pages may do.** Service pages are untrusted. `src-tauri/capabilities/remote.json` allows them only a handful of
commands: report a notification, report a probe result, open an external link and (on Windows) report a shortcut. Link
clicks to other sites are intercepted in the page and opened in the default browser; sign-in providers stay in the app only
on a sign-in screen (`Service::stays_in_app` in `services.rs`).

**Locking rule.** `hub.rs` mutates its state under one lock, collects `Action`s, releases the lock, then performs them (web
view calls, HTTP). Nothing that can block on the main thread runs while the lock is held.

### Source layout

| File | What it does |
|---|---|
| `src-tauri/src/lib.rs` | The app: setup, commands the UI and pages call. On macOS it is built as a library (see below). |
| `src-tauri/src/main.rs` | The launcher. macOS: loads the core library. Elsewhere: just runs the app. |
| `hub.rs` | Per-service state, the scheduler, muting/pausing, Slack presence, the daily greeting. |
| `slack.rs`, `gmail.rs`, `http.rs`, `pollers.rs` | The unread pollers and their HTTP layer. |
| `webviews.rs` | The window, the sidebar and cover web views, the per-service web views, links, downloads, cookies. |
| `services.rs` | The service list, link rules, and the JavaScript injected into pages. |
| `menus.rs`, `tray.rs` | Menu bar / tray icon and menus (macOS app menu too). |
| `banner.rs`, `banner_win.rs` | Native notification banners with Reply / Mark as Read (UserNotifications on macOS, toasts on Windows). |
| `native.rs` | Everything that differs per platform: file locations, opening links, start at login, Dock icon. |
| `settings.rs`, `types.rs` | Settings file; types shared between the scheduler and the pollers. |
| `ui/` | The sidebar and covers (plain HTML, CSS and JS). |
| `tools/measure.py`, `tools/memtest.sh` | Memory measurement. |

### Why the macOS build has two parts

macOS remembers "Always Allow" for WhatsApp's encryption key (stored by WebKit in the keychain) by the app's exact code, so
signing with a stable certificate isn't enough: every ordinary rebuild changed the app's hash and meant a new prompt. So:

- `main.rs` is a tiny launcher. It is the signed `Huburt.app` executable and never changes. It loads the real app and calls
  `huburt_main()`.
- `lib.rs` is the real app, built as `libhuburt_core.dylib`. The launcher looks for it in this order: `$HUBURT_CORE`; inside
  the app bundle (`Contents/Frameworks`, how the downloadable zip ships it); then `~/Library/Application Support/Huburt/`
  (how a local `./build.sh` updates it without touching the signed app).

`build.sh` only replaces the library in Application Support, and leaves an installed `Huburt.app` alone while the launcher's hash
is unchanged. `scripts/package-macos.sh` instead builds a self-contained, ad-hoc-signed copy for download (it never touches
the development bundle). Signing for real, and notarising, would need an Apple Developer account: set `SIGN_IDENTITY` for
the first part; `xcrun notarytool` isn't wired in yet.
Changing `main.rs`, `Info.plist` or the icon changes the hash and costs one "Always Allow", so don't edit the launcher
casually. Sign with a stable local certificate ("Hub Local Signing" in `build.sh`; ad-hoc signing makes every build look
new). Windows has no keychain approval to protect, so there it is one ordinary program.

## Logs, settings and test switches

- Log: `~/Library/Logs/Huburt.log` (Windows: `%LOCALAPPDATA%\Huburt\Huburt.log`). Counts and status only.
- Settings: `~/Library/Application Support/Huburt/settings.json` (Windows: `%APPDATA%\Huburt\settings.json`);
  `HUB_SETTINGS_DIR` overrides the folder.
- `--hidden`: start in the menu bar / tray only.
- `HUB_IDLE_SECONDS`, `HUB_WAKE_INTERVAL`, `HUB_WAKE_AWAKE`, `HUB_WAKE_SETTLE`: shorten the timers.
- `HUB_TEST_STEPS="select:slack:20,probe:slack:5,hide:30,show:5"` walks through states (seconds each; also `online:work`,
  `disable:gmail`, `enable:gmail`). `HUB_TEST_GREETING=1` sends the daily greeting after 8 seconds.
- `HUB_DEBUG_COOKIES=1` logs how many cookies each page's store holds (never their values).
- macOS: run the bundle with `open -n --env NAME=value --stdout f --stderr f Huburt.app`. `HUBURT_CORE=<path>` loads a
  different core library.

## Measuring memory (macOS)

`python3 tools/measure.py snapshot > before.json` before launching, then `python3 tools/measure.py total <pid> before.json`
prints macOS physical footprint for the app plus the WebKit helper processes that appeared since (they're launched through
XPC, so they can't be found through the process tree). `tools/memtest.sh` walks the app through each service and then hides the
window, measuring after each step.

## Platform notes

### Windows

Same features as macOS, with these differences:

- **Notification area, not menu bar.** The tray icon is white or black to suit the theme; left-click shows or hides the
  window, right-click opens the menu. Closing the window hides it to the tray. Hover text is cut to Windows' 127 characters.
- **Toast notifications** with a Reply box and Mark as Read for Slack messages, built from toast XML
  (`toast_xml` in `banner.rs`, unit-tested on every platform). Huburt registers its name for toasts with a per-user registry
  key. A toast left in the Action Center from an earlier run can't be routed back after Huburt restarts, so it does nothing
  when clicked.
- **Shortcuts:** Ctrl+1..9 switch service, Ctrl+R reloads, Ctrl+W / Ctrl+H hide to the tray. There is no menu bar in the
  window; the pages report the shortcuts themselves (`shortcut` command).
- **Start at login** is the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` registry entry, switched from the tray menu
  and turned on at the first launch of an installed copy.
- Pages use WebView2's own browser identity (no user-agent override). Each service has its own profile folder under
  `%APPDATA%\local.huburt.app\stores\`.

### macOS

- The menu bar icon is drawn in code (white on dark, black on light menu bars) and doesn't follow the wallpaper tint.
- The yellow minimise button minimises normally; Cmd+H, Cmd+W and the red cross hide to the menu bar. Cmd+Q does nothing: only
  **Quit Huburt** quits.
- Notification permission and the WebKit keychain prompt are asked once, on first use.

## Not yet tested

- **Windows, hands-on.** Built and unit-tested by CI, not yet used on a real Windows machine. Most worth checking: how the
  sidebar, cover and service pages sit in the window, reading the pages' cookies (Gmail and Slack unread counts), the toast
  buttons, and memory use.
- **Banner buttons** (Reply, Mark as Read) haven't been clicked on either platform.
- Calls, huddles and screen sharing; Intel Macs; Windows 10.
