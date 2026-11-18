# Privacy

Huburt has no server, no account, no analytics and no telemetry. This page says what it does with your data, so you can check it
against the code.

## What stays where

- **Your logins** (cookies and site storage for Slack, Gmail and WhatsApp) are kept by your operating system's web engine, on
  your computer: WebKit's storage on macOS, a WebView2 profile folder on Windows. Each service has its own separate store.
  Huburt never sees your passwords: you type them into the real sign-in pages.
- **Settings** (service order, which services are on, mute and pause times, your Slack online and Gmail count choices) are a small
  JSON file: `~/Library/Application Support/Huburt/settings.json` on macOS, `%APPDATA%\Huburt\settings.json` on Windows.
- **The log** (`~/Library/Logs/Huburt.log` on macOS, `%LOCALAPPDATA%\Huburt\Huburt.log` on Windows) records counts and status
  such as "slack poll: ok unread=2", never message text, names or tokens.

## What goes over the network

- The pages in the window talk to Slack, Google and WhatsApp directly, exactly as in a browser.
- The **background unread checks** talk to the same places, with your own session:
  - Slack: the endpoints Slack's own web app uses (`client.counts` and a few lookups to describe a new mention) on your
    workspace's Slack address.
  - Gmail: your inbox's Atom feed on `mail.google.com`.
  - Slack "show as online" (if you turn it on): Slack's presence calls.
- Nothing is sent to Huburt's author or any third party. There are no analytics libraries; the only network code is in the
  Slack and Gmail checkers (`src-tauri/src/slack.rs`, `gmail.rs`) and the web views themselves.

## What is held in memory

To make those checks while a page is closed, Huburt keeps your session cookies for Slack and Gmail and the Slack workspace token
**in memory only**, picked up from the page when it loads. They are never written to disk or to the log, and they are gone when
Huburt quits.

## Message content

When Slack reports a new mention, Huburt reads that message's sender and text so it can show a banner ("Sam in #ops: ..."). This
happens on your computer and the text is shown to the operating system's notification centre, not stored by Huburt. If you use a
Reply box in a banner, the reply is sent to Slack with your session.

## What the web pages can ask Huburt to do

Pages are untrusted web content, so Huburt grants them only three things (see `src-tauri/capabilities/remote.json`): show a
notification, report a "has this page finished loading" check, and open a link in your default browser (and, on Windows, report a
keyboard shortcut). Only web, mail and similar links are ever handed to your operating system.

## Reporting a problem

See [SECURITY.md](../SECURITY.md).
