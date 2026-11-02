<div align="center">

# agent-browser-cli2

A browser perception and control CLI for agents, turning a real Chrome session into reusable tab scanning, page JavaScript, Cookie, CDP, and screenshot capabilities.

The repository and npm package are named `agent-browser-cli2`; the CLI command, binary, and Skill name remain `agent-browser-cli`, with an `agent-browser-cli2` alias installed as well.

Browser perception · Page control · Chrome session reuse · CDP · Conditional wait · Agent Skill integration

<p>
  <a href="https://github.com/1728578755/agent-browser-cli2"><img src="https://img.shields.io/badge/CLI-agentbrowsercli2-2ea44f" alt="CLI agentbrowsercli2"></a>
  <a href="https://github.com/1728578755/agent-browser-cli2/blob/main/LICENSE"><img src="https://img.shields.io/badge/License-MIT-green" alt="License MIT"></a>
  <a href="https://github.com/1728578755/agent-browser-cli2"><img src="https://img.shields.io/badge/sys-win%2Fmac%2Flinux-0078D6?labelColor=0078D6&color=C0C0C0" alt="sys win/mac/linux"></a>
  <a href="https://github.com/1728578755/agent-browser-cli2/releases"><img src="https://img.shields.io/badge/release-v0.3.8-orange" alt="release v0.3.8"></a>
  <a href="https://github.com/1728578755/agent-browser-cli2/pulls"><img src="https://img.shields.io/badge/PRs-welcome-brightgreen" alt="PRs welcome"></a>
</p>

[AI One-Line Install](#ai-one-line-install) · [Manual Installation](#manual-installation) · [Chrome Extension](#chrome-extension) · [Update](#update) · [Changelog](CHANGELOG.md) · [Uninstall](#uninstall) · [Friendly Links](#friendly-links)

[中文](README.md) | English

</div>

`agent-browser-cli` is a browser perception and control tool for agents. It connects to the user's real Chrome browser through a Chrome extension, preserving login state and cookies while providing tab scanning, page JavaScript execution, cookie reading, CDP control, screenshots, file uploads, dropdown clicks, and related capabilities.

This project is not Selenium or Playwright. It is better suited for helping agents read pages accurately and perform actions inside an existing browser session.

## Project Info

- Current version: `0.3.8`
- Supported platforms: macOS (arm64 / x64), Linux (x64 / arm64), Windows (x64, including WSL)
- No prebuilt package: Windows ARM64 (`win32-arm64`) requires `cargo build --release` and `AGENT_BROWSER_CLI_BIN` pointing at the produced binary
- Browser: Chrome; Edge also works with its own extension install
- Linux prerequisite: the local Chrome / Chromium build must support loading extensions
- WSL prerequisite: use `WSL 2.0.0+`, and preferably enable `networkingMode=mirrored` on Windows `11 22H2+` so WSL can reach the host Chrome bridge service on `localhost`

## Acknowledgements

This project continues from the 0.3.7 codebase of [sleepinginsummer/agent-browser-cli](https://github.com/sleepinginsummer/agent-browser-cli), which in turn derives from the Web toolchain in [GenericAgent](https://github.com/lsdefine/GenericAgent), including ideas and implementation around `TMWebDriver`, `simphtml`, and the `tmwd_cdp_bridge` extension.

This project is an **unofficial derivative work** and is not affiliated with or endorsed by either upstream project. Thanks to both projects for the browser bridge, page simplification, CDP control, and practical SOPs; changes on top of 0.3.7 are listed under v0.3.8 in [CHANGELOG.md](./CHANGELOG.md).

See [SOURCES.md](./SOURCES.md) for the full derivation chain, secondary-development scope, specification references, and dependency licenses. The upstream MIT copyright notices and license text are reproduced in [NOTICE](./NOTICE).

## AI One-Line Install

```text
Please read https://github.com/1728578755/agent-browser-cli2/blob/main/AI_INSTALL.md, follow the instructions to install the CLI, load the Chrome extension, and add `skills/agent-browser-cli/SKILL.md`.
```

## Improvements over GenericAgent (0.3.7 baseline)

The items below are changes in the 0.3.7 baseline relative to GenericAgent; this repository's changes on top of 0.3.7 are listed in CHANGELOG.

- Extracted browser control capability from GenericAgent and exposed it as a CLI for Codex, Claude Code, and OpenCode. The GenericAgent browser extension can be reused and does not need to be reinstalled.
- Avoids reinitializing the browser connection for every command.
- Adds a startup lock to avoid repeated low-level port binding when multiple CLI commands start concurrently.
- Adds the skill `skills/agent-browser-cli/SKILL.md` for AI usage reference.
- Includes several optimizations to reduce command execution time.
- Optimizes the CDP debugger attach/detach flow to reduce page flicker and stale attach state after abnormal exits.
- Rust implementation for the CLI side.

## What It Can Do

1. Automated testing
   It can reuse a real browser environment for page-flow validation, form submission, button clicks, navigation checks, and login-state page testing.
2. Frontend page debugging
   It can read the DOM, execute JS, inspect page state, and capture screenshots to help locate frontend interaction, rendering, and data issues, including backend API integration problems.
3. Page style debugging
   It can execute JS on real pages to modify DOM / CSS and temporarily validate styles, layout, and interaction effects. It is more of a debugging assistant than a complete design tool.
4. Web data extraction
   It can read page content, tables, lists, cookies, and API-related state, making it suitable for extracting data from pages that require login state.
5. Browser operation scripting
   It can chain operations such as opening pages, switching tabs, executing JS, taking screenshots, and uploading files into scripts for repetitive web tasks.
6. Agent-assisted web admin operation
   It is suitable for letting AI Agents operate existing web tools such as admin consoles, configuration pages, low-code platforms, and form systems.
7. Page structure analysis
   It can simplify HTML and identify main content areas and list structures, helping Agents understand complex pages faster.
8. Security research and reverse-engineering assistance
   It can observe page behavior in a real browser session, execute debugging scripts, and read frontend state to assist analysis of frontend logic and API calls.

## Its Capabilities

1. Scan current Chrome tabs and get page titles, URLs, and tab IDs.
2. Switch to a specified tab and reuse existing pages and login state.
3. Open new tabs and directly visit target URLs.
4. Execute JavaScript in pages and read DOM, forms, state, and page data.
5. Read cookies from the current page for login-state related tasks.
6. Call Chrome CDP capabilities for lower-level page control.
7. Capture page screenshots for visual inspection and page confirmation.
8. Upload local files to web file picker inputs.
9. Operate common page elements such as dropdowns, buttons, and forms.
10. Background execution: `open --background` creates a tab without stealing focus; long-running work can be submitted with `--async` and collected through the `job` commands so it never blocks the caller.

## Background and Async

`open` is foreground by default (it switches to the new tab). With `--background` the tab is created without switching or stealing focus, and it still becomes the default target for subsequent commands.

```bash
agent-browser-cli open https://example.com              # foreground
agent-browser-cli open https://example.com --background # background
agent-browser-cli exec 'return location.href'            # acts on the new tab directly
agent-browser-cli --async scan --text-only              # submit to the queue, get a job_id back
agent-browser-cli job wait <jobId> --timeout 300        # collect the result when you need it
agent-browser-cli run steps.json                        # run several steps in one process
```

For details and limitations, see [docs/ASYNC_BACKGROUND.md](./docs/ASYNC_BACKGROUND.md); for what is implemented, what comes next, and the boundary/perf data, see [docs/FEATURE_STATUS.md](./docs/FEATURE_STATUS.md).

When the extension changes, reload the unpacked directory once in `chrome://extensions`. After that (since 2.3) the extension watches the content hash of `background.js` and reloads itself while idle, so you do not have to click reload repeatedly. If you are unsure whether the browser is running the current extension, run `agent-browser-cli status`: `extension.stale: true` or a `warning` means it has not been reloaded yet.

## Layout

```text
.
├── Cargo.toml                    # Rust project config
├── src/                          # Rust CLI / daemon / bridge
├── assets/tmwd_cdp_bridge/       # Chrome MV3 extension
├── assets/page_*.js              # Master copies of the page scripts
├── assets/simphtml_opt.js        # Page simplification script
├── scripts/                      # Build/sync helpers (sync-extension-assets.mjs, ...)
├── npm/                          # npm launcher scripts
├── skills/agent-browser-cli/     # Agent Skill
├── tests/                        # Node-side tests, mock extension, manual acceptance scripts
├── docs/                         # Behaviour notes and capability status
└── .github/workflows/            # CI and release pipelines
```

`assets/page_*.js` is copied byte-for-byte into `assets/tmwd_cdp_bridge/ops/` by `npm run sync:extension`: the first copy is bundled into the daemon for the eval path, the second is injected by the extension on strict-CSP pages. **Edit only the `assets/` copy**, then run the sync script. `npm run check:extension` compares without writing (CI runs it as a consistency guard) and `prepack` syncs automatically.

## Manual Installation

### npm

```bash
npm install -g agent-browser-cli2
agent-browser-cli tabs
```

> The primary command remains `agent-browser-cli` for compatibility with existing scripts and the Skill; an `agent-browser-cli2` alias is installed as well. The source-built binary is also named `agent-browser-cli`.

### Build From Source

```bash
cargo build --release
./target/release/agent-browser-cli tabs
```


## Chrome Extension

1. Recommended: download `chrome-extensions.zip` from the [latest Release](https://github.com/1728578755/agent-browser-cli2/releases/latest), extract it, open `chrome://extensions` in Chrome, enable `Developer mode`, click `Load unpacked`, and select the extracted `tmwd_cdp_bridge` directory.

2. When building from local source, you can also load this extension directory directly:

```text
assets/tmwd_cdp_bridge
```

3. Chrome needs at least one normal web page tab open. Do not leave it only on `about:blank` or `chrome://` pages.
4. After the extension is connected, a Chrome extension tip badge appears on the right side of the page. The badge position is draggable and expands on hover. It auto-hides after 10 seconds without commands, and you can also click `Hide for this session` to hide it manually. Manual hiding lasts for the current service connection cycle and resets after the service disconnects after about 300 seconds and reconnects.
5. By default the daemon only checks that the WebSocket handshake comes from *some* Chrome extension, i.e. "install the extension and it works". To harden it, use `agent-browser-cli trust-extension <extensionId>` to allow-list extension IDs and/or `--token <value>` to require a handshake token; when both are set, both must match. `--clear` returns to compatibility mode. See [SKILL.md](./skills/agent-browser-cli/SKILL.md#加固-websocket-身份可选).

### Custom Chrome Extension WebSocket Port

- `18765`: default extension WebSocket port, used by the Chrome extension. It can be changed with `agent-browser-cli set-extension-port <port>`.
- `18767`: CLI HTTP API port, used by the CLI to reuse the session. It cannot be used as the extension port.

Change the extension port from CLI:

```bash
agent-browser-cli set-extension-port 18766
```

This command writes the config file. If the daemon is running, it restarts the daemon so the new port takes effect immediately.

You can also edit the config file manually. The config file is `~/.agent-browser-cli/config.json`. It is created automatically when missing:

```json
{
  "extension_port": 18765
}
```

Manual edit example:

```json
{
  "extension_port": 18766
}
```

After manually editing the config file, run `agent-browser-cli restart` so the daemon listens on the new port.

The Chrome extension popup can also update the extension port and reconnect immediately. The popup port must match the CLI `extension_port` config.

### Profile Label

When multiple Chrome Profiles or browser instances are connected, `profile_id` and `browser_id` are long. You can set a short label for each Chrome Profile and then use `--profile <label>`.

```bash
agent-browser-cli lookup tab <tabId>
agent-browser-cli lookup browser <browser_id>
agent-browser-cli profile-label set work --profile <profile_id>
agent-browser-cli tabs --profile work
```

You can also set Profile Label from the extension popup in the corresponding Chrome Profile. The label is only an alias; internal routing still uses `browser_id:profile_id:tab_id`. If a label matches multiple profiles in the current daemon, the CLI reports ambiguity. Prefer setting labels via CLI because it checks uniqueness across currently connected profiles; the popup is a local convenience entry and cannot guarantee cross-profile uniqueness. `tabtree` truncates URLs and omits `session_key` by default to save tokens; pass `--full` for the complete fields.

### Dialog Suppression

The extension no longer rewrites page `alert` / `confirm` / `prompt` globally. Native dialogs are suppressed only while a CLI page script command is running, then restored after the command finishes, so long-lived pages keep their original global functions.

## Quick Check

```bash
agent-browser-cli tabs
agent-browser-cli open https://www.baidu.com
```

On success `tabs` returns (while `open` returns `opened_tab_id` / `opened_session_key` / `ready`):

```json
{
  "ok": true,
  "result": {
    "status": "success",
    "metadata": {
      "tabs_count": 1
    }
  }
}
```

## Common Commands

The README only keeps the quick entry point. For the full command list and browser operation SOP, see [skills/agent-browser-cli/SKILL.md](./skills/agent-browser-cli/SKILL.md).

```bash
agent-browser-cli tabs
agent-browser-cli scan
agent-browser-cli exec 'document.title'
agent-browser-cli snapshot
agent-browser-cli click @e1
agent-browser-cli fill @e2 'text'
agent-browser-cli send-keys Enter --target @e2
agent-browser-cli mouse-click @e1
agent-browser-cli screenshot --full-page
agent-browser-cli save-pdf
agent-browser-cli network start
agent-browser-cli network list
agent-browser-cli console start
agent-browser-cli console list
agent-browser-cli cookies
agent-browser-cli upload 'input[type=file]' ./a.pdf
agent-browser-cli run steps.json
agent-browser-cli batch steps.json --concurrency 8   # concurrent batch (across tabs)
agent-browser-cli read --max-chars 8000 --links      # structured page read
agent-browser-cli find 'a[href]' --limit 20           # find elements
agent-browser-cli find --text '登录'                   # find by visible text
agent-browser-cli hover '#menu'
agent-browser-cli select '#city' --value sh
agent-browser-cli check 'input[name=agree]'
agent-browser-cli scroll --to bottom
agent-browser-cli wait --selector '.result' --timeout 10
agent-browser-cli back
agent-browser-cli reload
agent-browser-cli frames                              # list iframes
agent-browser-cli exec --frame <frameId> '...'        # execute inside an iframe
agent-browser-cli inject 'window.__x = 1'             # one-shot injection
agent-browser-cli inject --persist hook --match '*://*.example.com/*' '...'
agent-browser-cli inject --list
agent-browser-cli inject --remove hook

# Reading: hidden content / large pages / iframes
agent-browser-cli read --include-hidden                  # include display:none content
agent-browser-cli read --max-nodes 20000 --budget-ms 800 # rate-limit huge pages
agent-browser-cli scan --fast                            # compact text only, still fast on 100k-node pages
agent-browser-cli read --frame <frameId>                 # read content inside an iframe

# Page network: send requests / record / mock
agent-browser-cli net fetch https://api.example.com/me -H 'Accept: application/json'
agent-browser-cli net hook --capture-bodies
agent-browser-cli net hook --persist                      # survives reloads
agent-browser-cli net mock /api/user --request-header 'x-token: t' --response-status 201
agent-browser-cli net log --limit 20
agent-browser-cli net mock /api/user --status 200 --body '{"id":1,"name":"mock"}'
agent-browser-cli net routes
agent-browser-cli net unroute /api/user                      # remove page rules
agent-browser-cli net unroute /api/user --engine cdp          # remove CDP rules
agent-browser-cli net reset

# CAPTCHA / CF challenge: detect, wait, hand over (no automated bypass)
agent-browser-cli challenge status
agent-browser-cli challenge solve                             # waits up to 120s by default
agent-browser-cli challenge wait                              # waits up to 30s by default
agent-browser-cli challenge focus

# Download watching
agent-browser-cli downloads --limit 10
agent-browser-cli download https://example.com/report.pdf --filename report.pdf
agent-browser-cli download-wait --id 7 --timeout 120

# Multi-browser orchestration (one extension install per Chrome / Edge)
agent-browser-cli browsers
agent-browser-cli broadcast exec --script 'return location.href'
agent-browser-cli broadcast read --args '{"max_chars":2000}' --browser chrome --browser edge
agent-browser-cli open https://a.com https://b.com https://c.com --background  # open several tabs concurrently
agent-browser-cli tabs --full
agent-browser-cli open https://example.com --window --focus
agent-browser-cli open https://example.com --group-title demo
agent-browser-cli close --tab <tabId>
agent-browser-cli --async exec 'return document.title'
agent-browser-cli job list
agent-browser-cli job wait <jobId>
agent-browser-cli tabtree
agent-browser-cli tabtree --full
agent-browser-cli tabtree --profile <profile_label>
agent-browser-cli tabtree --tab <tabId>
agent-browser-cli lookup tab <tabId>
agent-browser-cli lookup browser <browser_id>
agent-browser-cli profile-label set work --profile <profile_id>
agent-browser-cli profile-label clear --profile <profile_id>
agent-browser-cli status
agent-browser-cli doctor
agent-browser-cli logs --tail 100
agent-browser-cli install-skill --dry-run

# Optional WebSocket identity hardening (off by default)
agent-browser-cli trust-extension <extensionId>   # only allowed extension IDs may connect
agent-browser-cli trust-extension --token <value> # the handshake must carry ?token=<value>
agent-browser-cli trust-extension --list
```

> **This hardening is one-way — do not treat it as a security boundary.** `trust-extension` controls *who may connect to the daemon*; the reverse direction is not checked — **the extension never verifies that the daemon it talks to is genuine**.
>
> The daemon exits after 5 idle minutes, freeing port 18765. During that window any local process can bind the port; the extension will reconnect on its own schedule and still execute whatever commands that process sends (including reading cookies for any site and calling arbitrary CDP methods). In other words: **a token protects your daemon from other extensions, not your extension from other processes.**
>
> Practical impact: this amplifies local privilege escalation / lateral movement, and requires an attacker who can already execute code on the machine. Run sensitive work while you are present, and `agent-browser-cli stop` when you are done to shorten the window.

## Exit Codes

When a command returns `ok:false`, the exit code is non-zero, so scripts and agents can rely on `$?` / `&&` instead of parsing JSON.

```text
0    success
1    business failure (ok:false); includes challenge wait/solve timeouts and page script errors under exec --monitor
3    wait timeout: job wait timed out, download-wait timed out
130  cancelled: job wait observed a cancelled job
```

`status` follows `healthy` — a stopped daemon, a port mismatch, a disconnected extension, or no usable page all produce `ok:false` plus a non-zero exit code. Set `AGENT_BROWSER_CLI_EXIT0` (any value, including `0`) to restore the old always-zero behaviour; do not set it unless something depends on it.

The daemon exits after 5 idle minutes by default; override this with `AGENT_BROWSER_CLI_IDLE_TTL` (seconds). A shorter window reduces the chance of another local process grabbing the port.

## Update

For the full version history, see [CHANGELOG.md](./CHANGELOG.md).

AI one-line update:

```text
Please read https://github.com/1728578755/agent-browser-cli2/blob/main/AI_INSTALL.md, follow the instructions to update the CLI, download the extension zip again to the user-specified location, ask the user to manually load the Chrome extension, and update the related SKILL.md.
```

If the Chrome extension has updates, download the zip again, overwrite the existing files, and reload the `Agent Browser CLI Bridge` extension in `chrome://extensions`.

### Upgrading from an earlier version

- **`change.top_change` is gone**: `--monitor` now returns `change = {changed, added, removed, attributes, text, samples}` only. Switch to `changed` / `samples`; failed observation adds `monitor.reason` (`navigated` / `superseded` / `unsupported`).
- **`status` changed its exit code**: `ok` follows `healthy`, so an unhealthy daemon no longer exits 0. Scripts that gate on `status && ...` should either read `summary` or set `AGENT_BROWSER_CLI_EXIT0=1` temporarily.
- **Failures no longer pretend to succeed**: page script errors under `exec --monitor`, `job wait` on `failed` / `cancelled`, `download-wait` timeouts, and `challenge wait/solve` timeouts all return non-zero.
- **`trust-extension` is optional**: leaving it unconfigured keeps the "install and go" behaviour. Configure it when several extensions or several people share one machine. Note it only constrains *who may connect to the daemon*, not *who the extension connects to* — see the hardening note above.
- **Keep the extension assets in sync**: `assets/page_*.js` and `assets/tmwd_cdp_bridge/ops/` are two copies of the same scripts; when building from source run `npm run sync:extension` first, otherwise strict-CSP pages keep running the old code.

## Uninstall

Stop the long-lived service first:

```bash
agent-browser-cli stop
```

Then clean up as needed:

```bash
npm uninstall -g agent-browser-cli2
rm -rf ~/.agent-browser-cli
rm -rf ~/.agents/skills/agent-browser-cli
```

`~/.agent-browser-cli/` holds `config.json`, `api-token`, `ws-trust.json`, `net-hooks.json`, `daemon.lock`, and logs — that is the local token and network-rule state. Check that nothing in there needs to be kept before deleting it.

Finally, remove the `Agent Browser CLI Bridge` extension from Chrome's extension management page, or remove the loaded `assets/tmwd_cdp_bridge` extension configuration.

## Privacy and Data

Everything runs on your machine: the CLI and the daemon talk over `127.0.0.1`, and the extension only connects to a local port. Page content, cookies, and screenshots are never uploaded.

The data boundaries worth knowing:

- **Cookies and page content**: commands such as `cookies`, `read`, and `exec` return their results to the caller, which usually means the agent's conversation context, and may then be sent to the corresponding model provider. Judge for yourself on sensitive sites.
- **Network interception**: `net hook --capture-bodies` records request/response bodies and `net mock` rewrites responses; everything stays local and `net reset` clears it.
- **Local files**: config, tokens, persisted network rules, and logs live in `~/.agent-browser-cli/`; logs may contain URLs and error messages.
- **Extension permissions**: see [PRIVACY.md](./PRIVACY.md) for the purpose of permissions such as `cookies`, `debugger`, and `downloads`.

## Responsible Use

This tool is meant for automated testing, frontend debugging, data collection, and security research within authorized scope. Users must comply with the target site's terms of service, its robots policy, and applicable local law, and must not use it for unauthorized access, bypassing access controls, or infringing on other people's data. The project ships no automated CAPTCHA bypass: the `challenge` commands only detect, wait, and hand over to a human.

## Contributing

Issues and PRs are welcome. Read [CONTRIBUTING.md](./CONTRIBUTING.md) before submitting; report security problems privately as described in [SECURITY.md](./SECURITY.md) rather than posting exploitable details in public.

## Friendly Links

- [LINUX DO - A New Ideal Community](https://linux.do/)
- [GenericAgent - a self-evolving agent in roughly 3K lines](https://github.com/lsdefine/GenericAgent/tree/main)

## License

MIT License. See [LICENSE](./LICENSE).
