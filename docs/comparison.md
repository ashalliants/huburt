# Huburt compared with Ferdium, Rambox, Franz and other options

An honest guide to when Huburt is the right choice and when something else is. These products change, so check their current
features; where this page makes a claim about another product it sticks to things that are true by design.

**Short version:** Huburt is for people who use exactly Slack, Gmail and WhatsApp and want it to cost almost nothing in
memory. If you need many services, pick Ferdium or Rambox. If you only need one of the three, its own app or a browser tab
is simpler.

## At a glance

| | Huburt | Ferdium / Rambox / Franz | Each service's own desktop app | Browser tabs or pinned PWAs |
|---|---|---|---|---|
| Services | Slack, Gmail, WhatsApp (fixed) | Dozens, and you can add custom ones | One each | Anything |
| Built on | Tauri (Rust) and your system's web engine | Electron (bundles Chromium) | Varies; Slack's desktop app is built on Electron | Your browser |
| Pages kept loaded when unused | No: unloaded after 5 min away, with light background checks for Slack and Gmail | Yes by default; optional "hibernation" frees idle services, which generally stops live messages from them until woken | Yes | Browsers may discard background tabs |
| Memory when idle | About 26 MB (macOS, measured) | Depends on how many services are open | Each app keeps its own page loaded | Depends on the browser |
| Notifications while a page is unloaded | Yes, via light background checks (WhatsApp up to 5 min late) | Yes, the page stays live | Yes | Only while the tab/PWA exists |
| Reply from the notification (Slack) | Yes | Depends on the service | Depends on the app | Browser-dependent |
| Platforms | macOS, Windows | macOS, Windows, Linux | Varies | All |
| Cost | Free, open source | Ferdium and Rambox have free tiers or are free; Franz is paid for some plans | Free | Free |

## Choose Huburt if

- Slack, Gmail and WhatsApp are your whole communication stack.
- You're on a laptop with limited RAM, or you keep many other things open, and a messaging app shouldn't be the heaviest thing
  running.
- You like one window, one icon and one red dot, and fast mute controls.
- You'd rather read open, small source code than trust a large app.

## Choose Ferdium, Rambox or Franz if

- You use more than these three services: Telegram, Discord, Microsoft Teams, Messenger, Slack plus several others.
- You want workspaces, multiple accounts of the same service, custom service definitions, or a large community of recipes.
- You'd rather have every service live all the time than unloaded pages with a short delay for WhatsApp.

## Choose the service's own app or a browser tab if

- You only use one of the three. An all-in-one app adds little for a single service.
- You rely on features Huburt doesn't cover: for example calls and huddles haven't been tested in Huburt.

## Why Huburt can be so much smaller

By default most all-in-one messengers keep every service's page loaded so messages arrive instantly. That is simple and
reliable, and it is where the memory goes: a signed-in Slack page alone was about 600 MB in our measurements, in any app that
keeps it open. Ferdium, Rambox and Franz offer optional "hibernation" to free idle services, which saves memory but generally
means that service stops delivering messages until it wakes.

Huburt's approach is to close the page when you look away and keep counts and banners coming from a few small HTTPS requests
instead. The trade-off is that this only works for services where such a check exists: Slack and Gmail have one, so they stay
live; WhatsApp has none, so it wakes for about 45 seconds every five minutes and can be up to five minutes late while it is asleep.
See [how it works](DEVELOPING.md#how-it-works).

## Questions people ask

- *"Is there something like Ferdium that doesn't use so much RAM?"* This is it, for the three services above.
- *"What's the best app for WhatsApp, Slack and Gmail in one window?"* Depends on whether you need more services (Ferdium,
  Rambox) or the lowest memory use (Huburt).
- *"Is there a Franz alternative that's free and open source?"* Ferdium is a fork of Franz that is free and open source;
  Huburt is free and open source too, with a narrower scope.
