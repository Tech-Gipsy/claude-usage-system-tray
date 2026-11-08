# Claude Usage Meter

A minimal system-tray (menu-bar) app for Windows, macOS, and Linux that shows your Claude usage at a glance.

The tray icon is a live progress ring of your **session (5-hour) limit** — green, amber, or red as you approach it. Hover the icon (or, on Linux, right-click → **Show usage…**) for the full picture:

<p align="center">
  <img src="assets/popup.png" width="340" alt="Hover popup: session and weekly limit bars with reset times, today's tokens and cost, 7-day sparkline, 30-day total">
</p>

## Features

- **Session & weekly limits** — the same numbers as Claude Code's `/usage`, fetched with your existing Claude Code login. No setup.
- **Local usage stats** — today's tokens and cost, a 7-day sparkline, and a 30-day total, computed from Claude Code's local transcript data (cost for older history is a blended-rate estimate, marked `≈`).
- **API key spend (optional)** — month-to-date organization spend via the Anthropic Admin API. Save an admin key (`sk-ant-admin…`) in Settings; it's stored in your OS secret store (Windows Credential Manager / macOS Keychain / Linux Secret Service), never on disk.
- **Kimi Code (optional)** — appears automatically when `~/.kimi-code` exists. Weekly + 5-hour quota from Kimi Code's own login, Moonshot API balance, and local token stats. When Kimi Code is signed in with a subscription, the tray ring becomes a split ring: left = Claude weekly, right = Kimi weekly. The popup gets **Claude | Kimi** tabs.
- **Tray niceties** — on Windows and macOS, hover to peek, click to pin, `Esc` to dismiss. On Linux, right-click the tray icon → **Show usage…** (the Linux tray has no hover/left-click events). Right-click everywhere for Refresh now, Settings, Start at login, Quit.
- Lightweight: ~3 MB installer, ~45 MB RAM, no Electron.

## Requirements

- Windows 10/11, macOS 12+, or a Linux desktop with a system tray (StatusNotifier/AppIndicator) and a Secret Service keyring (gnome-keyring or KWallet) for the optional admin-key feature
- [Claude Code](https://claude.com/claude-code) installed and signed in (limits and local stats read from `~/.claude`)

## Install

Grab the binary for your OS from [Releases](../../releases):

- **Windows** — `.exe` NSIS installer
- **macOS** — `.dmg`
- **Linux** — `.deb` (Debian/Ubuntu/Mint)

The release binaries are **unsigned** (open-source, no paid signing certs yet):

- **macOS:** on first launch macOS may say *"Claude Usage Meter is damaged and can't be opened."* It isn't damaged — that's Gatekeeper blocking an unsigned app downloaded from the internet (especially on Apple Silicon). Drag the app to **Applications**, then clear the quarantine flag:
  ```sh
  xattr -cr "/Applications/Claude Usage Meter.app"
  ```
  Then open it normally. (Right-click → **Open** does *not* clear the "damaged" state for unsigned Apple-Silicon apps — use the command above.)
- **Windows:** SmartScreen may warn. Click **More info → Run anyway**.

Or build from source:

```sh
# prerequisites: Rust (stable), Node 20+
npm install
npm run tauri build
```

For development: `npm run tauri dev`.

## How it works

| Data | Source | Refresh |
|---|---|---|
| Session / weekly % | Anthropic OAuth usage endpoint, using the token Claude Code already stores in `~/.claude/.credentials.json` | 60 s |
| Session / weekly %, when the Claude Display companion app is installed | read from that app's `usage-cache.json` while it is under 30 min old, so only one app on the machine asks the endpoint; the meter falls back to its own fetch, at most every 5 min, when the file is missing or stale |
| Today / 7-day / 30-day stats | `~/.claude/projects/**/*.jsonl` transcripts (mtime-bounded, per-file parse cache) + `stats-cache.json` history | 30 s |
| API spend | `GET /v1/organizations/cost_report` with your admin key | 10 min |
| Kimi quota (5-hour / weekly) | `GET /usages` on Kimi Code's coding endpoint (the `base_url` set in Kimi Code's `config.toml`, e.g. `https://api.kimi.ai/coding/v1`), with Kimi Code's login token from `~/.kimi-code/credentials/`, read-only | at most every 5 min |
| Kimi local stats | `~/.kimi-code/sessions/**/wire.jsonl` | 30 s |
| Moonshot balance | `GET /v1/users/me/balance` with the key from Kimi Code's `config.toml`, or an optional key saved in Settings | 10 min |

Everything degrades gracefully: network down or token expired shows last-known data with a `stale` badge — no error dialogs. The Kimi login is never refreshed by the meter, so if Kimi Code hasn't run recently its numbers show last-known values marked stale. The app never sends your data anywhere except to Anthropic's own APIs and, for Kimi, to Moonshot/Kimi's APIs.

## Privacy & safety notes

- The OAuth token is read from (and on expiry, refreshed back into) Claude Code's own credentials file — writes are atomic and yield if Claude Code refreshed first.
- The Admin API key lives in your OS secret store (Windows Credential Manager / macOS Keychain / Linux Secret Service) under `claude-usage-meter`.
- Kimi Code's login is read-only: the meter reads it from `~/.kimi-code/credentials/` and never writes or refreshes it. It is only sent to `kimi.com`, `kimi.ai`, `moonshot.ai`, or `moonshot.cn` hosts, over https only.
- An optional Moonshot API key saved in Settings is stored in the same OS secret store under `claude-usage-meter`.
- No telemetry. The only third-party endpoints are Anthropic's, Kimi's, and Moonshot's, and only when the matching feature is in use.

## Tests

```powershell
cd src-tauri
cargo test   # 77 tests: JSONL parsing/dedup, pricing, OAuth refresh paths (mocked), cost-report pagination, icon rendering, Kimi config/local stats
```

## License

[GPL-2.0](LICENSE)
