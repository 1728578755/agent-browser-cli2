<div align="center">

# atb

A browser CLI for agents: a Rust daemon plus a Chrome MV3 extension bridge that drives the user's logged-in Chrome / Edge.

Tab scanning · Page reading · JS execution · Click and fill · Screenshots / PDF · Cookies · Network capture · CDP · Multi-agent isolation

<p>
  <a href="https://github.com/1728578755/atb-cli"><img src="https://img.shields.io/badge/CLI-atb-2ea44f" alt="CLI atb"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-green" alt="License MIT"></a>
  <a href="https://github.com/1728578755/atb-cli/actions/workflows/ci.yml"><img src="https://github.com/1728578755/atb-cli/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/1728578755/atb-cli"><img src="https://img.shields.io/badge/sys-win%2Fmac%2Flinux-0078D6?labelColor=0078D6&color=C0C0C0" alt="sys win/mac/linux"></a>
  <a href="https://github.com/1728578755/atb-cli/releases"><img src="https://img.shields.io/badge/release-v0.7.1-orange" alt="release v0.7.1"></a>
  <a href="https://github.com/1728578755/atb-cli/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs welcome"></a>
</p>

[Quick start](#quick-start) · [Install](#install) · [Capability map](#capability-map) · [Versions](#versions-and-compatibility) · [Security](#security-and-privacy) · [Docs](#documentation-map)

[中文](README.md) | English

</div>

## What it is

`atb` lets an agent use the browser you already have open: a long-running Rust daemon connects to the running Chrome / Edge through a Chrome extension, and each CLI call is a short-lived process that sends one command to the daemon.

Login state, cookies, installed extensions, and the current window stay as they are, because it is your window. One command can target several tabs at once; results come back in input order as JSON, and exit codes separate "the command is wrong" from "retry later", so scripts never parse error text.

The daemon stays resident; it exits after 300 idle seconds by default, but never while an extension is still connected, so consecutive operations do not handshake again for every command.

**What it is not**: not Selenium / Playwright (it never launches a throwaway automation profile and does not own the browser by default; only an explicit `atb launch --restart` restarts it); not a cloud browser (traffic between the CLI, daemon and extension stays on `127.0.0.1`; the pages you drive still talk to their own sites); not a stealth browser (it only reduces this tool's own traces and does not patch fingerprints).

## Quick start

```bash
# 1. Install the CLI — Node >= 18
npm i -g @g1728578755/atb

# 2. Load the extension: chrome://extensions → Developer mode → Load unpacked
#    pick the directory where chrome-extensions.zip was extracted (manifest.json sits directly inside);
#    the repo and npm package also ship the same directory as assets/atb_bridge

# 3. Open a page: a target command starts the daemon automatically
atb open https://example.com

# 4. Confirm the extension bridge
atb tabs --ids

# 5. Read the current page, or run JS in it
atb read --tab current --max-chars 4000
atb exec 'return document.title'

# For a full health check (status does not start the daemon; run the commands above first)
atb status
```

No browser restart is needed after loading the extension. When a command returns `ok:false`, read `error_code` and `hint` (some timeout cases only carry `error` / `reason`); `atb schema --json` gives an agent the arguments of top-level commands.

## How it differs from Selenium / Playwright

| Dimension | atb | Selenium / Playwright |
| --- | --- | --- |
| Session source | Reuses the user's logged-in Chrome / Edge | Starts a fresh profile with no login state |
| Concurrency | One command over many tabs | You write the concurrency and pooling yourself |
| Output | Compact JSON plus structured error_code / exit codes | Native objects and exceptions |
| Detection surface | Read / exec / wait do not attach; snapshot, plain-CSS click/fill, screenshots and PDF try to attach, with automatic fallback when refused (see below) | Automation markers and a debugger by default |
| Best fit | An agent driving the user's own browser | End-to-end suites built from scratch |

It does not replace a test framework: for isolated environments and hundreds of parallel cases, Playwright is the better tool.

## Capability map

| Domain | Representative command | What it does |
| --- | --- | --- |
| Page reading | `atb read` | Main content, links, hidden nodes, iframes, truncated by a character budget |
| Page actions | `atb snapshot` | Emits `@e1` handles used by click, fill, select, upload and scroll |
| Scripts and batches | `atb exec` / `atb scrape` | Run JS, multi-step scripts, cross-tab scraping, async jobs |
| Network and debugging | `atb net` / `atb console` / `atb downloads` | Capture requests, mock responses, console, download watching |
| Challenges and traces | `atb challenge` / `atb stealth` | Challenge detection and active solving; trace audit and scrubbing |
| Multi-browser and ops | `atb browsers` / `atb doctor` / `atb logs` | Multi-browser / Profile selection, diagnostics, logs |

## Install

### npm

```bash
npm i -g @g1728578755/atb
atb status
```

Prebuilt binaries come from `optionalDependencies` per platform, so no Rust toolchain is required. The npm package exposes a single command name, `atb`. The main package is currently held in npm staged publishing; until it is approved, use the Release artifacts or build from source below.

### Build from source

```bash
cargo build --release
./target/release/atb status
```

Point `ATB_BIN` at the built binary to avoid mixing it with a global install.

### Load the extension

1. Download `chrome-extensions.zip` from the [latest Release](https://github.com/1728578755/atb-cli/releases/latest) and extract it into a fixed directory (the zip root is the extension itself, with `manifest.json` directly inside), or use `assets/atb_bridge` from the repo.
2. Open `chrome://extensions` (Edge: `edge://extensions`), enable developer mode, click "Load unpacked", and select the directory that directly contains `manifest.json`.
3. Keep at least one normal web page tab open — not only `about:blank` or `chrome://` pages.
4. **This release requires extension 5.2.9.** After you replace the extension directory the daemon asks the extension to reload on the next handshake; if the daemon is not running, click "Reload" on the extension page yourself — copying files without reloading keeps the old code running in pages.

### Verify

```bash
atb tabs --ids           # run this first: starts the daemon and proves the bridge works
atb status               # then check health; running status first reports daemon_not_running
atb doctor               # use it when nothing connects: ports, token, daemon state
```

### Platform matrix

| Platform | Prebuilt | Notes |
| --- | --- | --- |
| macOS arm64 / x64 | Yes | — |
| Linux glibc x64 / arm64 | Yes | The local Chrome / Chromium must allow loading extensions; WSL 2 also uses this package |
| Windows x64 | Yes | Native Windows; do not use the win32 package inside WSL |
| Windows ARM64 | No | `cargo build --release`, then point `ATB_BIN` at the binary |
| Alpine / musl | No | Same as above, build it yourself |

WSL needs `WSL 2.0.0+`; on Windows 11 22H2+ set `networkingMode=mirrored` in `.wslconfig` so WSL can reach the bridge service on the host `localhost`.

## Core concepts

### Daemon

When the `atb` binary finds no daemon it starts a resident process that listens on `127.0.0.1:18767` and serves the HTTP API; later commands reuse that session. It exits after 300 idle seconds only when no extension is connected; while an extension stays connected it keeps running, with a 60-second reconnect grace after a disconnect. `ATB_IDLE_TTL` changes the idle value. Logs go to `~/.atb/logs/daemon.log`.

### Extension bridge

The extension opens a WebSocket to `127.0.0.1:18765`, using `extension_port` from `~/.atb/config.json`. The daemon sends commands, the extension runs them in the tab and returns JSON. Reloading the extension or restarting the browser invalidates every tab id under that browser and yields `session_gone`: `open` again, or run `tabs` for a fresh list.

### Agent identity

Resolution order: `--agent` → `ATB_AGENT` → terminal session id (`WT_SESSION` / `TMUX_PANE` / `VSCODE_IPC_HOOK_CLI`, …) → `default`. `open` only moves the caller's own default target and `cleanup` only closes tabs this agent opened, so several agents can share one daemon without fighting over a page.

### Target selection

`--tab` accepts a single id, `111,222`, `all`, `@1` / `@1-5`, `url:<substring>`, `title:<substring>`, `group:<title>`, `window:<id>`, and `current`. Multi-target runs execute concurrently, return in input order, and one failure does not affect the others; when writing files `--out` must contain `{tab}` or `{index}` or preflight rejects the command.

### Protocols

Three versions must line up: the CLI ↔ daemon API protocol (currently **5**), the extension message protocol (currently **4**), and the extension file version (currently **5.2.9**). The `compatibility` field of `atb status` reports expected and actual values, with `fixes[]` when they disagree.

## Common workflows

### Read a page

```bash
atb tabs --ids
atb read --tab <tabId> --max-chars 8000
atb read --include-hidden          # include display:none content
atb scan --fast                    # compact text only, fast on large pages
```

### Fill a form and submit

```bash
atb snapshot --limit 200           # produces @e1 / @e2 handles
atb fill @e2 'Jane Doe' --dry-run  # validate arguments and target first
atb fill @e2 'Jane Doe'
atb click @e1
atb wait --selector '.result' --timeout 10
```

`snapshot` by default, and `click` / `fill` with plain CSS selectors, attach the debugger; `snapshot --js` plus `@e` handles use the in-page path and avoid the debug banner.

### Batch scraping

```bash
atb scrape --urls urls.txt --out out.jsonl
atb scrape --urls urls.txt --js-file grab.js --out out.jsonl --batch 6 --concurrency 6 --retry 2
```

`--urls` takes a file (one per line) or a comma-separated list; the default script collects `{title, url, text}`. Everything is orchestrated CLI-side on top of the daemon's open / exec / close.

### Multiple tabs

```bash
atb open https://a.com https://b.com --background
atb read --tab all --max-chars 2000
atb exec --tab @1-3 'return location.href'
atb screenshot --tab all --out 'shots/{index}-{tab}.png'
```

#### When the page blocks debugger injection (automatic fallback)

Chrome refuses `chrome.debugger.attach` on pages that embed an iframe from another extension
(common with assistant/translator extensions). These commands now degrade instead of failing:

| Command | Fallback channel | Marker in the response |
| --- | --- | --- |
| `screenshot` | visible area via `chrome.tabs.captureVisibleTab` (no debugger) | `via:"captureVisibleTab"` |
| `screenshot --full-page` | scroll slice by slice, stitch with OffscreenCanvas | `via:"captureFullPageStitched"`, `stitched:true`, `slices`, `truncated` |
| `save-pdf` | stitched long image wrapped into a single-page PDF (text not selectable) | `via:"stitched-image-pdf"`, `text_selectable:false` |
| `mouse-click` | synthetic mouse events dispatched in the page | `synthetic:true`, `trusted:false` |

`--max-height` caps the stitched image height (device pixels, default 20000); overflow is truncated
and reported as `truncated:true`. Inherent limits of stitching: `position:fixed/sticky` elements repeat
on every slice, and the PDF is image-only. Disable the offending extension temporarily if you need
trusted input events or a searchable PDF.

### Multiple agents

```bash
atb --agent reviewer open https://a.com
ATB_AGENT=reviewer atb read
atb cleanup --dry-run      # preview what would be closed
atb cleanup                # close only tabs this agent opened
```

### Challenge pages

```bash
atb open https://example.com                 # --challenge auto by default: solve actively, then wait
atb challenge status                         # challenge state, widget position, solve verdict
atb challenge solve --timeout 120            # run the active solver again manually
atb challenge widget                         # solver failed: get widget coordinates
atb challenge click --at 320,410             # the agent clicks it itself, no human needed
atb challenge focus                          # only for image/SMS checks that need a human
```

`open` and `challenge solve` do not just wait: they locate the widget, click it with trusted mouse events that include a movement path, then wait for the token or navigation. If the solver fails, the agent can fall back to `widget` + `click --at`; `challenge wait` is the passive path and `focus` is the last resort for a human. Passing is not guaranteed, and this does not bypass access control. Details in [docs/ANTI_DETECTION_AND_CHALLENGE.md](./docs/ANTI_DETECTION_AND_CHALLENGE.md).

## Command reference

```bash
# Reading
atb tabs --ids                            # compact list for target picking
atb read --max-chars 8000 --links
atb snapshot --limit 200                  # element handles @e1 / @e2
atb find --text 'Sign in' --limit 5

# Actions
atb open https://example.com --background
atb click @e1
atb fill @e2 'text'
atb select '#city' --value sh
atb upload 'input[type=file]' ./a.pdf
atb wait --selector '.result' --timeout 10
atb close --tab <tabId>

# Scripts and batches
atb exec 'return document.title'
atb run steps.json                        # many steps in one process
atb batch steps.json --concurrency 8
atb scrape --urls urls.txt --out out.jsonl
atb --async scan --text-only              # returns a job_id immediately
atb job wait <jobId> --timeout 300

# Network and debugging
atb net fetch https://api.example.com/me
atb net hook --capture-bodies
atb net mock /api/user --status 200 --body '{"id":1}'
atb console start
atb downloads --limit 10

# Challenges and traces
atb challenge status
atb stealth audit
atb stealth scrub

# Operations
atb status
atb doctor --fix
atb capabilities
atb schema --json                         # argument schema for top-level commands (use --help for subcommands)
atb logs --tail 100
```

`atb schema --json` is authoritative for parameters; the domain tables live in [skills/atb/references/commands.md](./skills/atb/references/commands.md).

### Exit codes

| Code | Meaning | What to do |
| --- | --- | --- |
| `0` | Success | — |
| `1` | Execution failed | Read `error_code`; retry after `retry_after_ms` when retryable |
| `2` | Command is wrong | Fix selector / JS / arguments / target; do not retry |
| `3` | `job wait` / `download-wait` timed out | Raise `--timeout`, or query again later; a plain `wait` timeout is `1` |
| `4` | Outcome unknown | The action may have run; resend with the same `--idempotency-key` |
| `130` | Job cancelled | `job wait` observed `cancelled` |

## Versions and compatibility

| Item | Value |
| --- | --- |
| CLI / daemon | `0.7.1` |
| CLI ↔ daemon API protocol | `5` |
| Extension message protocol | `4` |
| Extension file version | `5.2.9` |
| Minimum Chrome | `120` |

- Current version: `0.7.1`
- Companion extension: `5.2.9` (display name `ATB`)

```bash
atb status
# compatibility: { ok, cli_protocol, daemon_protocol,
#                  extension_protocol_expected, extension_protocol_connected, fixes[] }
```

| Symptom | What to do |
| --- | --- |
| `compatibility.ok:false` with different protocol numbers | `atb restart`; if it persists, end the process holding `18767` |
| `error_code:"extension_outdated"` | Replace the extension directory with `assets/atb_bridge`, then click "Reload" on the extension page |
| `upgrade_recommended:true` in `status` | Core commands still work; update the extension following `next_steps` |
| `stealth` returns `extension_ok:false` | Extension older than 5.0.0, or files replaced without a reload; follow `fixes`, then re-check |

Upgrade note: a new CLI refuses to talk to an old daemon on a protocol mismatch and prints the fix steps. The config directory stays `~/.atb` on purpose — moving it would split old and new daemons across separate ports.

## Operations

```bash
atb status                       # ok follows healthy; non-zero exit when unhealthy
atb doctor                       # diagnose ports, token, daemon version
atb doctor --fix                 # auto-repair; --force is required to kill processes
atb logs --tail 100
atb restart                      # restart the daemon after protocol or config changes
atb stop                         # stop the daemon; frees 18765, and the extension keeps reconnecting
atb set-extension-port 18766     # change the extension port, writes config.json and restarts
atb install-skill --dry-run      # preview where the skill will land
atb install-skill                # real dir ~/.agents/skills/atb; symlinks into the five agent dirs
```

`18767` is the CLI HTTP API port: fixed, not configurable, and never usable as the extension port. `ATB_EXIT0=1` restores the old always-zero `status` exit code — set it only if a script depends on that.

Upgrading the CLI: `npm i -g @g1728578755/atb@latest`, then update the extension files as described in "Load the extension". Full history in [CHANGELOG.md](./CHANGELOG.md); an AI-driven upgrade flow in [AI_INSTALL.md](./AI_INSTALL.md).

## Security and privacy

- Traffic between the CLI, daemon and extension stays on `127.0.0.1`; no telemetry, no remote code, no ad SDK. The pages you drive still talk to their own sites.
- `trust-extension` is one-way: it constrains who may connect to the daemon, but the extension never verifies that the daemon it talks to is genuine.
- The daemon exits after 300 idle seconds only when no extension is connected; `atb stop` alone frees 18765 while the extension keeps reconnecting, so disable the extension or close the browser to truly close the window.
- The extension declares `<all_urls>`, `cookies`, `debugger`, `scripting`, `management`, `contentSettings` and similar permissions; page data is read only when you or the CLI issue a command; `management` is used for the extension's own reload and to name the extension that blocks the debugger (`blockingExtensions`).
- `challenge` interacts with the page's verification widget (click and wait) but does not guarantee passage; image reading and SMS/email checks still need a human.

Report vulnerabilities per [SECURITY.md](./SECURITY.md); the local data inventory and deletion steps are in [PRIVACY.md](./PRIVACY.md). Both documents promise only what the code can guarantee and mark everything else as a boundary.

## Platforms and limits

Supported: macOS arm64 / x64, Linux glibc x64 / arm64, Windows x64, WSL 2 (use the Linux package), Chrome and Edge 120 or newer.

What it explicitly cannot do:

- No prebuilt packages for Windows ARM64 (`win32-arm64`) or Alpine / musl — build from source.
- The debug banner is Chrome's own UI and cannot be removed at runtime; the only switch is a launch flag, and applying it requires restarting the browser via `atb launch --restart`.
- Firefox / Safari are not supported; it needs a Chromium-based browser plus the extension.
- No fingerprint spoofing: UA, canvas, WebGL and hardware values stay as the browser reports them; `stealth` only removes what this tool wrote into the page.
- No bypassing CAPTCHAs or access control; JS challenges and clickable Turnstile widgets can be handled automatically, while image selection and SMS checks still need a human.
- When the extension bridge is down, no tab can be driven. That is by design, not a defect.

## Documentation map

| Reader | Document | Contents |
| --- | --- | --- |
| Letting an AI install it | [AI_INSTALL.md](./AI_INSTALL.md) | Copyable prompt, step-by-step flow, failure branches, uninstall |
| Writing an agent | [skills/atb/SKILL.md](./skills/atb/SKILL.md) | Standard loop, command selection by intent, failure handling |
| Looking up flags | [commands.md](./skills/atb/references/commands.md) | Per-domain command and parameter tables |
| Wanting recipes | [recipes.md](./skills/atb/references/recipes.md) | Copy-paste scenarios |
| Debugging a failure | [troubleshooting.md](./skills/atb/references/troubleshooting.md) | Symptom → diagnosis → fix |
| Going deeper | [docs/BEST_PRACTICES.md](./docs/BEST_PRACTICES.md) | Selectors, waiting, token budgets |
| Long-running work | [docs/ASYNC_BACKGROUND.md](./docs/ASYNC_BACKGROUND.md) | Async jobs, background tabs, limits |
| Detection and challenges | [docs/ANTI_DETECTION_AND_CHALLENGE.md](./docs/ANTI_DETECTION_AND_CHALLENGE.md) | Trace audit, scrubbing, challenge handling |
| Edge load failures | [docs/EDGE_TROUBLESHOOTING.md](./docs/EDGE_TROUBLESHOOTING.md) | Extension load errors, one by one |
| Feature status | [docs/FEATURE_STATUS.md](./docs/FEATURE_STATUS.md) | Shipped / next / performance and boundaries |
| Release history | [CHANGELOG.md](./CHANGELOG.md) | Change summaries since 0.4.0 |
| Historical record | [docs/CHANGELOG_ARCHIVE.md](./docs/CHANGELOG_ARCHIVE.md) | 0.3.7 and earlier, plus pre-refactor detail |
| Contributing | [CONTRIBUTING.md](./CONTRIBUTING.md) | Layers, test matrix, contracts you cannot break |
| Community norms | [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md) | Expected behaviour, reporting channel |
| Security / privacy / sources | [SECURITY.md](./SECURITY.md) · [PRIVACY.md](./PRIVACY.md) · [SOURCES.md](./SOURCES.md) | Threat model, local data, upstream and dependencies |

## Contributing

Read [CONTRIBUTING.md](./CONTRIBUTING.md) before changing code: exit codes, `error_code` values, JSON fields and both protocols are external contracts. Before submitting, run `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `node --test tests/*.test.mjs`.

Documentation changes ship in the same PR; the Chinese and English READMEs must stay in sync, and version strings are written by `npm run sync-version`.

## Acknowledgements

The browser control capability was extracted and adapted from the Web toolchain in [GenericAgent](https://github.com/lsdefine/GenericAgent), including `TMWebDriver`, `simphtml`, and the `atb_bridge` extension. Upstream sources and dependency licences are listed in [SOURCES.md](./SOURCES.md).

## License

MIT License, Copyright (c) 2026 1728578755. Upstream attribution is in [NOTICE](./NOTICE).
