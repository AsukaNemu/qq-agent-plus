<div align="center">

<img src="docs/assets/mark.svg" alt="QQ Agent Plus" width="104" height="104">

# QQ Agent Plus

A QQ group-chat agent for Linux servers, with split-bubble replies, sticker support, long-term memory and a built-in operations CLI.

[![CI](https://github.com/sakurawwwxh/qq-agent-plus/actions/workflows/ci.yml/badge.svg)](https://github.com/sakurawwwxh/qq-agent-plus/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-3da639.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/sakurawwwxh/qq-agent-plus?color=e8b400&label=stars&logo=github)](https://github.com/sakurawwwxh/qq-agent-plus/stargazers)
[![Last commit](https://img.shields.io/github/last-commit/sakurawwwxh/qq-agent-plus?logo=git&logoColor=white)](https://github.com/sakurawwwxh/qq-agent-plus/commits/main)
[![Node](https://img.shields.io/badge/node-%E2%89%A522.13-339933?logo=nodedotjs&logoColor=white)](package.json)
[![Platform](https://img.shields.io/badge/platform-Linux-0b5fff?logo=linux&logoColor=white)](docs/LINUX.md)
[![OneBot](https://img.shields.io/badge/protocol-OneBot%20v11-12b7f5)](https://github.com/botuniverse/onebot-11)
[![LLM](https://img.shields.io/badge/LLM-OpenAI%20%E5%85%BC%E5%AE%B9-6b4fbb)](#highlights)

[简体中文](README.md) ｜ **English**

<img src="docs/assets/console-demo.png" alt="Console: message archive and session management" width="880">

</div>

QQ Agent Plus is a QQ group-chat agent for Linux servers. It connects to an external OneBot v11
service and runs each turn as an isolated OpenAI Chat Completions session; it does not depend on
DSH, MCP, Electron or a Windows runtime.

## Highlights

Every capability below originates from a failure observed in a production group chat; the failure
mode and its resolution are recorded in [CHANGES](docs/CHANGES.md).

- **Conversation behaviour** — split replies across bubbles, judge image sentiment before
  replying, keep the sticker catalogue in the system prompt, self-check before finishing.
- **Send path** — retry transient network failures, QQ system faces, message-id normalisation,
  fallback parsing for inline tool calls.
- **Sticker system** — auto-collect with QQ favourites first, fuzzy lookup fallback, sync guard so
  an API failure cannot wipe the local library.
- **Games & reminders** — hosts mini-games: stateless ones run straight from the prompt (word chain,
  guess-the-number, 20 questions, truth-or-dare), with `dice` plus the group member list for rolls, draws
  and random picks; a **state-machine host** covers turn-based multiplayer games (number bomb,
  who-is-the-spy — turns, vote counting and verdicts live in code, the spy's word travels by private
  message only and never enters the model context; experimental, off by default). "Remind me at X"
  becomes a **persistent reminder** (survives restarts) delivered in its own voice when the time comes.
- **Proactive talk** — three independently switchable links (cold-topic opener / follow-up
  nudge when nobody answers / model-scheduled wake-up), multiple active windows, interval
  guard, skip-reason logging, catch-up for messages missed during restarts.
- **Model access** — thinking levels translated per channel preset, settable per provider and per
  task (Settings -> Model API -> "Advanced: thinking"), retry on provider moderation refusals,
  automatic fallback model.
- **Speech & video input** — voice/audio files and video soundtracks are transcribed before they
  reach the chat model (no multimodal requirement). Hosted ASR by default (SiliconFlow endpoint
  preset: paste one key, pull the model list once); switchable to Groq / Volcano Seed-ASR /
  Alibaba / iFlytek / Tencent / Baidu / self-hosted, most with free tiers for new users. Also
  **keyless local whisper.cpp** (offline, free, one click to install or fully remove in the
  console, or `node scripts/install-asr-local.mjs`). QQ voice (SILK) is decoded locally — no
  protocol-side transcoding; videos also get a 2x2 frame strip for the vision model plus separate
  soundtrack transcription, so it sees and hears. With an OpenAI-compatible TTS endpoint configured (off by default) it can also reply by voice.
  ASR keys are configured separately from search;
  an hourly quota gate applies, and muted-group pre-checks report the reason instead of sending
  blindly.

Configuration examples are in [docs/CONFIG-EXAMPLES.md](docs/CONFIG-EXAMPLES.md). Operations
commands are collected in [src/ops.js](src/ops.js) and documented in [docs/OPS.md](docs/OPS.md).

## Demo

Operations commands ship with the application (`src/ops.js`); read-only commands do not modify
production data. The CLI prints Chinese; the samples below reproduce the actual output format
(excerpt).

```text
$ node src/ops.js audit
===== 1. 服务与定时器 =====
  [正常] qq-agent-linux.service  active
  [正常] qq-agent-linux-update.timer  enabled
===== 3. 源码语法（全部 js） =====
  [正常] 所有 js 文件语法通过（75 个）
===== 4. 未定义调用扫描 =====
  [正常] 可疑未定义调用点: 0
===== 8. 运行态 =====
  [正常] OneBot: connected=true …
===== 自检结论 =====
  全部通过（0 项异常）
（节选：实际共 11 段，含配置 / 数据文件 / 价格缺口 / 最近日志 / 主机资源等）

$ node src/ops.js watch-send --minutes=5
基线：outbox 最新 rowid=17，待处理消息 0 条
      最近一条人类消息：今晚还打不打
SEND_OK：机器人通过工具层成功回话 ｜ group:123456789 ｜ run=3f2c… ｜ state=sent ｜ 打啊
      人类上一条：今晚还打不打
```

The following is an illustrative sample of one turn in a group chat; it is not a real chat log.
After picking up the other member's message, the agent continues with its own remark in a separate
bubble and uses a sticker where appropriate.

```text
member:  playing tonight?
bot:     yes
bot:     just finished dinner, give me ten minutes
bot:     [sticker: stop dawdling]
```

## Architecture

```text
OneBot WebSocket
  -> serialised per conversation into SQLite
  -> SQLite/WAL message state machine
  -> bounded aggregation: random 8-12 s by default, 20 s maximum
  -> legacy / threaded / lifecycle routing
  -> optional persisted threads and checkpoints
  -> one-shot Agent session
  -> session-bound tools
  -> OneBot HTTP
```

A message is acknowledged only after the turn was handled successfully. When a model call fails or
the process restarts, the unsent batch remains pending and is retried automatically. When a send
result cannot be confirmed, the row enters `held` and requires manual review, which prevents
duplicate replies.

## Deployment

### Full-stack installation

A fresh Linux machine should use the interactive installer `deploy-all.sh`. The installer asks for
the deployment directory and ports, installs Docker (with sudo confirmation), downloads SnowLuma,
configures OneBot, deploys QQ Agent and generates or synchronises all service credentials.
SnowLuma already contains OneBot; NapCat and Lagrange must not be installed on top of it.

Installation and runtime must not use root: `deploy-all.sh` refuses to run as root, so use a regular
user (the script calls `sudo` only when installing Docker or enabling linger).

```bash
git clone https://github.com/sakurawwwxh/qq-agent-plus.git
cd qq-agent-plus
bash deploy-all.sh
```

The default deployment directory is `/mnt/data/qq-agent`, with the following public ports:

- `3210`: QQ Agent console;
- `5099`: SnowLuma WebUI;
- `6081`: noVNC, used for the QQ login.

OneBot HTTP `3000` and WebSocket `3001` bind `127.0.0.1` only and are not exposed to the LAN. The
installer does not ask for the host IP; it detects and prints the access addresses on completion.
Generated credentials are stored in `/mnt/data/qq-agent/deployment-access.txt` with mode `0600`.
The script does not modify UFW, firewalld or cloud security groups. For cross-host access, only the
three entries above should be opened to a trusted LAN or VPN; noVNC and OneBot must never be
exposed to the internet.

The first installation also asks for the model base URL, API key, model name and the QQ allowlist.
Once the infrastructure is running, open the printed noVNC address and scan the QQ login QR code,
then return to the terminal and press Enter; the installer verifies the OneBot login and offers to
activate. After confirming that the previous bot has stopped or excludes the same chats, the
following command performs the same step manually:

```bash
/mnt/data/qq-agent/app/manage.sh activate --confirm-exclusive
```

Unattended installation:

```bash
bash deploy-all.sh --yes --root-dir /mnt/data/qq-agent \
  --agent-port 3210 --snowluma-port 5099 --novnc-port 6081 \
  --model-base-url https://api.deepseek.com \
  --model-api-key "$DEEPSEEK_API_KEY" --model deepseek-chat \
  --allow-groups 123456789
```

Unattended mode requires explicit model configuration, or `--skip-model-config` followed by console
configuration. The allowlist may be empty, but the agent answers nothing until a chat is allowed.

Re-running is permitted only for installations managed by this script whose configuration still
matches. Before writing, the installer verifies the Agent configuration against the deployment
record, the systemd service directory, the Compose ownership and data volumes of the SnowLuma
container, and port usage. On detecting an unmanaged installation, an incomplete leftover state or
credentials modified outside the script, it exits with an error without overwriting configuration
or restarting services; neither `--yes` nor `--rotate-credentials` bypasses this protection.

Read-only check (creates no directories, downloads no dependencies, modifies no services):

```bash
bash deploy-all.sh --check-only --root-dir /mnt/data/qq-agent
```

An existing Bridge/SnowLuma production environment should update the Agent with `deploy.sh`,
preserving the actual data directory, listen addresses and OneBot configuration. Existing data must
not be deleted and `.env` must not be forged to bypass the checks. The full-stack check additionally
requires `realpath` and `ss` (iproute2); when Docker is present but its containers cannot be read,
the script exits safely.

Re-running a managed installation preserves SnowLuma data, the QQ login state and existing
credentials. To rotate the Agent, OneBot, SnowLuma WebUI and noVNC credentials together, add
`--rotate-credentials`; when SnowLuma 2FA is enabled, `--snowluma-totp` is also required. The
complete option list is available from:

```bash
bash deploy-all.sh --help
```

### Agent-only installation

Use `deploy.sh` when a compatible OneBot v11 service already exists, or when only the Agent is to be
installed or updated. Requirements:

- Linux with systemd user services;
- `curl`, `tar`, `sha256sum`, `rsync` (if no suitable Node.js is found, the script downloads and
  verifies Node 22 into `INSTALL_DIR/.runtime`);
- a non-root user (the service is registered as that user's systemd user service);
- a running OneBot v11 HTTP and forward WebSocket service;
- an OpenAI Chat Completions compatible model service.

Panel environments (BT Panel / aaPanel) are covered in [docs/BAOTA.md](docs/BAOTA.md): the panel
serves only as a management interface, the process remains under a systemd user service, and it
must not be started through the panel's Node project feature or PM2.

```bash
git clone https://github.com/sakurawwwxh/qq-agent-plus.git
cd qq-agent-plus

bash deploy.sh \
  --install-dir /mnt/data/qq-agent/app \
  --data-dir /mnt/data/qq-agent/data \
  --host 127.0.0.1 \
  --port 3210
```

`deploy.sh` does not install SnowLuma. It performs the following steps:

- validate the arguments, the sources and the `node:sqlite` capability of Node.js;
- install production dependencies;
- detect or install a Node.js 22 runtime;
- initialise the separate data directory and the console token;
- register and enable `qq-agent-linux.service`;
- enable automatic restart on process failure;
- check for port conflicts and validate the systemd unit;
- start a first installation in `observe` mode and preserve the existing mode on updates;
- snapshot the code before an update and restore the previous code, configuration and service on
  failure;
- exclude `.git`, `.dbg`, runtime data, credentials and local debug records.

The complete option list is available from:

```bash
bash deploy.sh --help
```

When an external backup flow already exists, the code snapshot can be skipped explicitly:

```bash
bash deploy.sh --install-dir /mnt/data/qq-agent/app \
  --data-dir /mnt/data/qq-agent/data --host 127.0.0.1 --port 3210 \
  --no-backup
```

A first migration from an old Bridge installation can add:

```bash
  --import-bridge /path/to/old/config.json \
  --credential-file /path/to/credentials.env
```

This copies configuration only; it does not modify the old directory or its data.

Updating an existing installation:

```bash
git pull --ff-only
bash deploy.sh \
  --install-dir /mnt/data/qq-agent/app \
  --data-dir /mnt/data/qq-agent/data \
  --host 127.0.0.1 \
  --port 3210
```

Updates do not reset the run mode, and the rest of the configuration is preserved. `--install-dir`
and `--data-dir` must match the existing installation: a wrong directory is not rejected, it points
the service at a new, empty data directory. `--host` and `--port` may be omitted — the script then
reuses the values recorded in `config.json` and prints a note; when given explicitly they must match
the first installation (a full-stack install uses `0.0.0.0`), otherwise the console becomes
unreachable from outside. The current listen address is on the URL line of `DATA_DIR/console-access.txt`.

A pre-deployment code snapshot is created under `DATA_DIR/deploy-backups/`; if installation,
configuration, systemd validation or the health check fails, the previous code, configuration and
service are restored.

The installer also installs a separate GitHub update service and timer. Automatic updates are
disabled by default and can be enabled from the “控制 -> 更新部署” page after an administrator is
configured. Hints and deployments follow **published Releases** only (drafts, pre-releases and
ordinary commits on `main` do not count) and target the commit of that tag: unit tests run first,
then deployment is delegated to `deploy.sh`. A deployment that already contains the latest Release
is not rolled back, and an indeterminate comparison performs no action. On failure the update rolls
back, disables automatic updates and notifies the administrator by private message.
See [docs/AUTO_UPDATE.md](docs/AUTO_UPDATE.md).

## Operations

### Service management

Run these from the installation directory (`--install-dir`):

```bash
bash manage.sh status
bash manage.sh logs
bash manage.sh health
bash manage.sh token
bash manage.sh restart
bash manage.sh observe
bash manage.sh activate --confirm-exclusive
bash manage.sh update-status
bash manage.sh update-now --confirm
bash manage.sh backup /path/to/new-backup-dir
```

### Operations tooling

Tooling beyond `manage.sh` (read-only health checks, data backup, send and login monitoring, a
process watchdog, sticker-name export, non-interactive deployment and SSH tunnels) is provided by
`src/ops.js` using Node built-ins only:

```bash
node src/ops.js help                     # all subcommands
node src/ops.js audit                    # service + code + data health check (read-only)
node src/ops.js audit-host               # host health check (read-only)
node src/ops.js scan                     # undefined-call scan
node src/ops.js backup --confirm         # stop/start the service, archive data, keep the last 4
node src/ops.js install-timers --print   # show the two systemd user timers
node src/ops.js console --open           # open an SSH tunnel and the console
```

Every subcommand accepts `--help`. Destructive commands require `--confirm` and support `--dry-run`
and `--print` previews. Paths and credentials are read from environment variables (`QQ_AGENT_*`,
`SSH*`); no real addresses or secrets are stored in the repository. Environment variables, common
examples and remote execution are documented in [docs/OPS.md](docs/OPS.md).

### OneBot connection troubleshooting

The console only reports connected or not connected. The actual reason is in the `onebot.error`
field of `/api/status`, which the health check prints:

```bash
node src/ops.js audit --dir=/mnt/data/qq-agent     # the "OneBot: connected=false error=…" line
journalctl --user -u qq-agent-linux -n 80 | grep -i onebot
```

> `ops.js` probes the protocol side on port `3390` by default. When the port was not changed (the
> installer default is `3000`), set `QQ_AGENT_ONEBOT_HTTP_PORT=3000`, otherwise that line reports a
> false unreachable result.

Behaviour by error value:

- `ECONNREFUSED` — the protocol side does not listen on that port. Check the container first:
  `docker ps -a | grep snowluma`, `docker logs --tail 50 qq-agent-snowluma`.
- `401` / `403` — token mismatch. The `accessToken` in the protocol side's `onebot.json` must match
  the WS token under “设置 -> OneBot” in the console (an empty HTTP token falls back to the WS
  token).
- `ENOTFOUND` — the address cannot be resolved; the WS address is wrong (default
  `ws://127.0.0.1:3001`).
- `ETIMEDOUT` — the host is unreachable (address or firewall).
- `404` / `Unexpected server response` — the wrong port was configured; the peer is not a WebSocket
  protocol side (HTTP port `3000` cannot serve as WS).

Three constraints apply:

- Address and token changes take effect only after a restart: the connection is established once at
  service start, saving configuration in the console does not reconnect, and `bash manage.sh restart`
  is required.
- The protocol side must provide a **forward WebSocket server**. This project is a forward WS client
  only; it does not provide a reverse WS server.
- Status dot: green means connected; yellow means it connected and then dropped (the service
  reconnects with backoff, no restart required); grey means it never connected.

A QQ account that is not logged in is not a connection failure: the WS connection is healthy and only
the login information is unavailable. `node src/ops.js watch-login` observes the login state. The
same procedure is documented in more detail in
[Linux Deployment and Operations](docs/LINUX.md).

### Console access

The console listens on `3210` by default. An agent-only installation (`deploy.sh`) binds `127.0.0.1`
only, so local access requires no public port; the full-stack installer (`deploy-all.sh`) starts the
console with `--host 0.0.0.0`, and cross-host access should be limited to a trusted LAN or VPN. On
Windows, [`console-tunnel.bat`](console-tunnel.bat) in the repository root reads the console token
from the server, opens an SSH tunnel and launches the browser without a login prompt; the host is
entered once as `user@host` and remembered. It also forwards `5099` (SnowLuma WebUI) and `6081` (QQ
login). On macOS, Linux or any machine with Node installed, `node src/ops.js console --open` has the
same effect.

The token can be rotated under “设置 -> 系统 -> 控制台安全”. Once the console is reachable from
other hosts, the token is its only credential and must be kept private.

The top-level control page is the unified operations entry point: it provides links and online state
for QQ Agent, DSH, Bridge, SnowLuma and the QQ remote desktop, and links to the model, search,
OneBot and console token settings. It also supports manual updates and pausing or resuming automatic
updates. The SnowLuma login key can be changed on that page; the key is sent with a single request
and is not written to the QQ Agent configuration or to frontend storage. The former `3110` portal is
no longer mapped.

Activation requires that no previous bot handles the same chats, otherwise replies are duplicated.

## Response probability

The response-probability slider under “设置 -> 聊天设置” determines how often the agent answers
ordinary messages; the value on the slider is the probability. `0%` answers only messages that
mention, name or keyword-match the agent, while `100%` answers every message. Batches that mention
or name the agent, or that match a keyword, always receive a reply regardless of the probability.
Each batch is rolled independently; there is no accumulated quota and no “N out of 100” allowance.
A single value can apply to every chat, or the unified switch can be turned off to set a value per
group; groups without their own value and all private chats follow the unified slider.
Configurations from the old four-band slider are converted once, on first load: `0-20` becomes `0%`,
`20-90` maps linearly, and `90+` becomes `100%`. The current model has no state that answers mentions
but not keywords, so an old `0-10` setting also answers keywords after the conversion.

## Personas

“设置 -> 人设” (the persona card library) provides several built-in persona cards (the default
小鲸鱼, a sarcastic friend, a warm companion, a homelab enthusiast and a catgirl), stored one markdown
file per card under [`roles/`](roles); see [docs/PERSONAS.md](docs/PERSONAS.md). Clicking a card
fills the draft; it takes effect after clicking the always-visible “保存设置” at the bottom. The body
is shown section by section (signature traits, AI-flavour blacklist and examples each rendered as
tags or chat bubbles) and every section can be edited on its own, reverted on its own, or the whole
card restored. The behaviour profile is either the original group-chat
style or the natural-and-reliable style, and both the role text and administrator rules can be
edited, or a custom copy created from the current draft. The built-in card text is sourced
from `roles/*.md`: while an instance is still bound to a built-in card, edits to the file take
effect on restart and the console copy is refreshed automatically — no need to re-pick the card
after changing it. Unbound custom text stays exactly as saved.

## Token saver

“设置 -> 省 Token” is a one-click way to cut token usage: it only **caps** a few tunable items and
never rewrites the values you filled in elsewhere, so switching it off restores your settings
immediately. Three levels: off (default), “省” and “很省”; the page lists your value and the
effective value for every item.

| Item | 省 | 很省 |
| --- | --- | --- |
| History read on mention / always-respond | ≤80 | ≤40 |
| History read on keyword / random tier | ≤50 / ≤30 | ≤30 / ≤20 |
| Tool rounds per run | ≤8 | ≤5 |
| Cumulative tokens per run | ≤80k | ≤50k |
| Session handoff injected (chars) | ≤2000 | ≤1200 |
| Global impressions injected (chars) | ≤3000 | ≤1500 |
| Sticker list in the prompt | ≤5 | ≤3 |

Context: the **fixed floor** of every model call (system prompt + 26-29 tool schemas depending on toggles) is about
12k-15k tokens and cannot be changed by settings. Measured over 7 days on a live instance
(867 calls / 18.4M tokens): input is 98.8% of all tokens, and the **uncached** part of the input
accounts for 76% of the cost — so saving tokens means reading less history, running fewer rounds
and making fewer calls, not trimming output. To save more, also lower the response probability,
turn off search or image input when unused, and use time control to stay inside off-peak hours.

## Time control

“设置 -> 时间控制” is disabled by default and, while disabled, ignores every time rule without
changing wake-ups, prompts, model requests or message handling. When enabled it uses Asia/Shanghai
and a global DS off-peak schedule: weekdays `00:00-09:00`, `12:00-14:00` and `18:00-24:00`, and the
whole weekend. Every group and private chat can override the global rule with DS off-peak, custom
weekdays and windows, or always active. Custom windows may cross midnight, for example Friday
`22:00-02:00` extending into Saturday; `00:00-24:00` means all day, and an empty custom schedule
means never active.

Messages and pokes received outside an active window are archived only: they do not trigger the
model and are not queued for a later reply. After the window opens, new messages are processed
according to the configured mode, and archived messages remain available as history. Model
requests, tool rounds, retries, memory work and sending are all gated. Daily summaries and the
console model test follow the global schedule, and the daily summary excludes chats that are
currently inactive; scheduled posts blocked by the schedule are postponed to the next active window.

Rule changes take effect immediately. In-flight requests that cross into an inactive window are
aborted and subsequent requests and sends are blocked; a provider may still bill a request it
already received, and that portion of the tokens cannot be cancelled. `held` records with an
unconfirmed send result are neither discarded nor resent because of a schedule change.

## Data

Data is stored in the `data` directory passed to the deploy scripts:

- `config.json` — configuration and credentials, mode `0600`;
- `messages.sqlite` — messages, leases and outbound state;
- `sessions/` — one record per Agent run;
- `memory/` — long-term impressions and cross-session handoff state;
- `identity-pilot.sqlite` — unified QQ identity index and friend-request approval ledger;
- `slang-pilot.sqlite` — the slang pilot is retired and will never be created;
- `daily-moments.json` — daily summaries, post decisions and publication results;
- `qzone-interactions.json` — unread Qzone feed queue, comment replies and external write state;
- `console-access.txt` — console address and token, mode `0600`.

Chat logs, keys, tokens and runtime data are git-ignored. `node scripts/sanitize-release.mjs`
produces a cleaned copy of the tree before it is shared.

## Verification

```bash
npm ci --omit=dev --ignore-scripts
npm run test:unit     # unit tests
npm run test:local    # local regression (temporary data directory, never production data)
node src/ops.js scan --strict
npm audit --omit=dev
bash -n deploy.sh manage.sh
```

CI (GitHub Actions) runs the syntax check, the strict undefined-call scan, unit tests, the local
regression, and the prompt / render / scroll / usage end-to-end suites on every push and pull request. Two small confirmed issues are open on this baseline,
neither on the main path; see [KNOWN-ISSUES](docs/KNOWN-ISSUES.md) for the current list and the
history. The [Chinese README](README.md) carries the complete option lists and the per-page
description of the console; the [documentation index](docs/README.md) lists all documents.

## License

MIT (see [LICENSE](LICENSE)). Derivation and third-party copyright are documented in
[NOTICE](NOTICE.md). The OneBot protocol side is separate software under its own license.
