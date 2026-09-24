# Security Policy

## Reporting a vulnerability

Dymus is a hobby project and has no formal security mechanisms. If you find a
security issue, please report it privately rather than opening a public issue:

- **GitHub:** use the "Report a vulnerability" option on the
  [Security tab](https://github.com/britonmearsty/dymus/security/advisories)
  (draft a private security advisory).
- **Email:** use the email address on your
  [GitHub profile](https://github.com/britonmearsty/dymus) for a direct report.

Please include the Dymus version you tested, the affected feature (stream
resolution, playback, config parsing, etc.), and reproduction steps.

## Scope

This project runs entirely on the user's own machine. It downloads and plays
audio from YouTube Music and Radio Browser, and controls a local mpv process.
There are no servers, accounts beyond the user's own YouTube/Last.fm
credentials, or remote code-execution surfaces. Credentials are handled locally
and are never transmitted to or stored by the project.

## Response

Reports are acknowledged when seen and handled as time allows. If a fix is
relevant, a release note will be attached to the advisory.