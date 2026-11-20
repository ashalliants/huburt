# Frequently asked questions

Short answers first; each links to detail where there is some. If yours isn't here,
[open an issue](https://github.com/ashalliants/huburt/issues).

- [Choosing an app](#choosing-an-app)
- [Memory and speed](#memory-and-speed)
- [Notifications and unread counts](#notifications-and-unread-counts)
- [Accounts and services](#accounts-and-services)
- [Privacy and safety](#privacy-and-safety)
- [Installing and removing](#installing-and-removing)
- [Slack status](#slack-status)
- [Problems](#problems)

## Choosing an app

### What is Huburt?

Huburt is a desktop app for macOS and Windows that shows Slack, Gmail and WhatsApp in one window, with a sidebar to switch
between them. It is built to use very little memory: pages load only while you use them and are closed after you look away.

### Is there a lightweight alternative to Ferdium, Rambox or Franz?

If you only need Slack, Gmail and WhatsApp, yes: Huburt. It doesn't bundle its own browser, and it unloads pages you aren't
using. If you need many more services (Telegram, Discord, Teams, Messenger, Notion and so on), Ferdium and Rambox support far
more and are the better fit. See the [comparison](comparison.md).

### How do I use Slack, Gmail and WhatsApp in one app on Mac or Windows?

Install Huburt, open it, and sign in to each service once. They then sit in a sidebar; click one to switch, or press
Cmd/Ctrl + 1, 2 or 3.

### Why not just use browser tabs, or the separate desktop apps?

You can. Huburt exists because three separate apps, or three pinned tabs, each keep their page loaded all day. Huburt gives you
one window, one icon with one red dot, one set of mute controls, and unloads what you aren't looking at.

### How is this different from Slack's or WhatsApp's own desktop apps?

Those are separate programs, and most are built on Electron (a bundled copy of Chrome). Huburt is one small program that
hosts the web versions using your system's web engine. You trade a few app-specific features (see
[Limitations](../README.md#limitations)) for much lower memory use and a single place for all three.

### Why Tauri and not Electron?

Electron apps ship a full copy of Chromium and run a Node.js process; each one carries that overhead. Tauri apps use the web
engine already installed on your computer (WebKit on macOS, WebView2 on Windows) and a small Rust program for the logic, so the
app itself is a few megabytes and idles at tens of megabytes. Huburt goes further by closing pages you aren't using.

## Memory and speed

### How much memory does Huburt use?

About **26 MB** with the window hidden and every page unloaded (macOS, measured). A page you're actively using costs what it
costs in a browser: a signed-in Slack page was around 600 MB in our measurements, WhatsApp and Gmail less. When you look away
for 5 minutes, that page is closed and its memory returned. See [Memory](../README.md#memory) and
[how it works](DEVELOPING.md#how-it-works).

### Why does Slack use so much memory, and can I reduce it?

The Slack web app is a large single-page application, and keeping it open means keeping all of it in memory. Huburt reduces it
by not keeping it open when you aren't using it. The unread count keeps updating from a light background check.

### Does it slow my computer down in the background?

The background work is a few small HTTPS requests per minute (Slack every 30 s, Gmail every 60 s) and a 45-second WhatsApp
wake every 5 minutes. Nothing runs continuously.

### Does it work on Apple silicon and Intel Macs, and on Windows 10 and 11?

The downloadable Mac build is for Apple silicon. An Intel Mac build from source should work but hasn't been tried. On Windows, CI builds the
installer and runs the unit tests on a Windows machine; hands-on testing on Windows 10 and 11 is still to come.

## Notifications and unread counts

### Will I get notifications while a page is unloaded?

Yes. Slack is checked every 30 seconds and Gmail every 60 seconds using your existing login, and new mentions, DMs and emails
become native banners. WhatsApp has no equivalent light check, so it wakes for about 45 seconds every 5 minutes (not at all if
it's turned off or you're signed out); new WhatsApp messages can arrive up to 5 minutes late while it is asleep. While a WhatsApp page is open you get its normal live
notifications.

### Can I reply to a Slack message from the notification?

Yes. Slack banners have a **Reply** box and a **Mark as Read** button, on macOS and Windows. Gmail and WhatsApp banners don't have
buttons; clicking any banner opens that service. (Button behaviour on Windows is new and hasn't had hands-on testing.)

### How do I stop notifications for a while?

Right-click a service in the sidebar to mute it for an hour or until 8am, or use the menu bar / tray icon's **Pause
Notifications**. Unread counts keep updating while muted. **Hide Red Dot Until Something New** clears the icon's red dot
without marking anything as read.

### Does Huburt keep waking a service I'm signed out of, or have turned off?

No. A service you've turned off (right-click it, **Turn Off**) isn't loaded, checked or woken at all. A service you're signed out of
(the orange "!") is also left completely alone: Huburt looks once, when you first start it, to find out whether you're signed in,
remembers the answer, and then stops, so WhatsApp's QR screen isn't loaded every 5 minutes for nothing. Click the service whenever
you want to sign in: it opens normally, and as soon as you're signed in the background checks start again by themselves.

### What does the grey dot on Slack mean?

There is unread activity in channels you haven't muted, but nothing that mentions you or messages you directly. The red badge
is reserved for DMs and mentions.

### Why doesn't the Gmail count match my inbox?

By default Huburt shows Gmail's "important unread" count, which matches the Inbox badge in Gmail. Right-click Gmail, choose
**Number on the Badge**, and pick **All unread** to include promotions and updates.

## Accounts and services

### Which services does it support?

Slack, Gmail and WhatsApp Web, and only those. The list is fixed in the code; there is no setting to add another. Adding a
service means adding a poller and an entry in `services.rs` (see [Contributing](../CONTRIBUTING.md)).

### Can I use more than one Slack workspace?

Yes. Slack's web app handles workspaces, and Huburt reads unread counts across all the workspaces you're signed in to.

### Can I use more than one Gmail or WhatsApp account?

Not with their own counts and banners: Huburt follows your first Gmail account and one WhatsApp account. You can still switch
Google accounts inside Gmail's own page, but the sidebar badge and notifications follow the first.

### Can I add Telegram, Discord, Microsoft Teams or Messenger?

Not yet. See the [comparison](comparison.md) for apps that support many more services.

### Does Huburt work with work or school Google accounts and Slack with single sign-on?

The sign-in pages for Google, Microsoft, Okta, GitHub, Apple, Auth0, OneLogin and Duo stay inside the app while you sign in, so
common single sign-on setups should work. Other identity providers may open in your browser instead; if yours does, please
[report it](https://github.com/ashalliants/huburt/issues).

## Privacy and safety

### Is Huburt private? Does it send my messages anywhere?

No. There is no Huburt server, account or analytics. The pages in the window talk directly to Slack, Google and WhatsApp, and
so do the background checks. Your logins are kept by your operating system's web engine, on your computer. Full details:
[Privacy](privacy.md).

### Does it read my messages?

Only to show you a banner. When it checks Slack for a new mention it reads that message's sender and text to build the banner
(it does this on your machine, with your own login). The log file records counts and status, never message text or tokens.

### Is it allowed by Slack, Google and WhatsApp?

Huburt shows each service's own web app inside a window, as a browser would. The background unread checks use the same
web endpoints those web apps use for themselves; they are not official public APIs, so they could change. Huburt is an
independent project, not affiliated with those companies. Check your workplace's policy if you're using it with a work account.

### Is it safe to enter my passwords?

You enter them on the real sign-in pages of Slack, Google and WhatsApp, inside the app's web view, as you would in a browser.
Huburt never sees or stores them. The code is open, and CI builds the Windows installers from it.

## Installing and removing

### How do I install it on Windows?

Download the `.exe` or `.msi` installer from [Releases](https://github.com/ashalliants/huburt/releases). It installs for your
user without admin rights. The installers aren't code-signed yet, so SmartScreen shows a warning: **More info**, then
**Run anyway**.

### How do I install it on a Mac?

Download the `macos-arm64.zip` from [Releases](https://github.com/ashalliants/huburt/releases), unzip it, and drag Huburt to
Applications. The first time, approve it once in Terminal with `xattr -dr com.apple.quarantine /Applications/Huburt.app`, or
via **System Settings → Privacy & Security → Open Anyway** (macOS asks because the app isn't notarised yet). Details are in
the [README](../README.md#macos-apple-silicon). It's Apple silicon only; on an Intel Mac, build it from source.

### Why does macOS say Huburt "could not be verified" or "is damaged"?

Apple only lets an app open without a warning if its developer has paid for an Apple developer account and had the app
notarised. Huburt is free and open source and isn't notarised yet, so macOS asks you to approve it once. The command in the
answer above removes the quarantine flag that your browser added to the download. You can check the download against the
checksums on the release page, or build it yourself from source.

### How do I start it at login?

Right-click the menu bar or tray icon and tick **Start Huburt at Login**. It starts hidden in the menu bar / tray.

### Where does it keep its data?

- Logins: your system's web engine storage, one separate store per service.
- Settings: `~/Library/Application Support/Huburt/settings.json` on macOS, `%APPDATA%\Huburt\settings.json` on Windows.
- Log: `~/Library/Logs/Huburt.log` on macOS, `%LOCALAPPDATA%\Huburt\Huburt.log` on Windows.

### How do I uninstall it completely?

On Windows, uninstall **Huburt** from Settings → Apps; that removes the program. To remove everything else, delete the settings
and log locations above and `%APPDATA%\local.huburt.app`. On macOS, quit Huburt, delete `/Applications/Huburt.app`, untick
**Start Huburt at Login** first (or delete `~/Library/LaunchAgents/local.huburt.login.plist`), and delete
`~/Library/Application Support/Huburt`, `~/Library/Logs/Huburt.log` and `~/Library/WebKit/local.huburt.app`.

## Slack status

### Can Huburt keep my Slack status active?

Yes, optionally. Right-click Slack in the sidebar, choose **Show as Online**, and pick **During work hours** (Monday to Friday,
8am to 6pm, your local time) or **Always**. Huburt keeps the Slack page loaded and tells Slack every few minutes that you're
active. Two things to know:

- **It costs memory.** Keeping the Slack page loaded is exactly what Huburt otherwise avoids, so expect several hundred MB
  while it is on.
- **It's your call.** It asks Slack to show you as active; it doesn't type or click for you. Slack may still mark you as away
  after a long period of no input, and some workplaces have policies about presence, so use it knowingly.

It's off by default.

## Problems

### Gmail or WhatsApp shows an orange "!" (signed out)

That means Huburt thinks you're signed out, so it leaves that service alone in the background. Open the service and sign in
again, and the "!" clears by itself within a few seconds for WhatsApp, or at the next check for Gmail, and the background checks
resume. If it doesn't, right-click the service and choose **Reload**.

### Gmail or Slack shows "Can't reach"

The page didn't load within 25 seconds, usually because you're offline. Huburt retries every 30 seconds, or press **Try
Again**.

### I clicked a link and now the page is stuck

Right-click the service and choose **Back to ... Home**. (Links in messages open in your browser, so this shouldn't happen;
if it does, please report which link.)

### macOS keeps asking to allow a keychain item

Huburt's downloadable builds are signed ad hoc, so macOS sees each new release as a new app and asks once to allow WhatsApp's
encryption key. If you build it yourself, the app is split into a small launcher that never changes and the real code in a
separate library, so rebuilding doesn't ask again. (If you edit the launcher, `Info.plist` or the icon you will be asked once
more.) See
[Developing](DEVELOPING.md#why-the-macos-build-has-two-parts).

### Windows says "Windows protected your PC"

The installers aren't code-signed (a signing certificate costs money). Choose **More info**, then **Run anyway**.

### Is there a Linux version?

No. It may build, but notifications, the tray and start-at-login have no Linux implementation, and it hasn't been tested.
