# Security

Huburt hosts web pages that hold your Slack, Gmail and WhatsApp sessions, so security reports are taken seriously.

## Reporting a vulnerability

Please **don't open a public issue** for a security problem. Use GitHub's private reporting instead: on the repository's
**Security** tab choose **Report a vulnerability**. Include what you found, how to reproduce it, and which version or commit.

You'll get an acknowledgement as soon as the report is seen. Please allow time to fix it before disclosing publicly.

## What's in scope

- A web page (Slack, Gmail, WhatsApp or anything it links to) being able to do more than its
  [allowed commands](src-tauri/capabilities/remote.json).
- Session cookies or tokens being written to disk, logged, or sent anywhere other than the service they belong to.
- Links or notification content causing the operating system to open something other than a web or mail link.

## How Huburt treats secrets

See [Privacy](docs/privacy.md): sessions stay in the operating system's web storage, the Slack token and the cookies used by the
background checks are held in memory only, and the log contains counts and status, never message text or tokens.
