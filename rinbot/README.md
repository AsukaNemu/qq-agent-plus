# RInBot Rust project

This directory is the isolated RInBot implementation described by
`docs/RPD-QQ-Bot-Rust-Refactor.md`. It is deliberately separate from the
existing Node.js runtime while the migration is in progress.

The first prototype is observation-only by default:

- it uses `rinbot/var/` as an independent SQLite data directory;
- it listens on `127.0.0.1:3211`, so it cannot take the current console port;
- it does not connect to NapCat unless `--connect-onebot` is explicitly passed;
- it never sends OneBot actions in observation mode;
- the in-memory work queue contains inbox IDs in observation mode, or conversation wake keys in active mode, never message payloads.
- non-loopback API binding is rejected unless `QQBOTD_API_TOKEN` is configured;
  authenticated API calls use `Authorization: Bearer ...` or a session cookie.

The local console is served by the same Rust process. Its read-only API includes
`/api/health`, `/api/status`, `/api/snapshot`, `/api/config`, paged
`/api/messages`, `/api/tasks`, `/api/outbox/unknown`, and `/api/events`;
`/api/config` masks secret values and exposes only whether tokens are configured.
`GET /api/settings/queue` reads the active debounce/batch limits, while
`POST /api/settings/queue/update` requires the administrator bearer token,
validates bounded values, applies them live, and stores them in SQLite for the
next start. The configuration archive page includes these queue controls; its
password input is used only for the save request and is not stored in browser
storage.
The task API lists durable message-inbox work; an administrator token is
required to cancel a queued item (`POST /api/tasks/inbox-<id>/cancel`). It refuses
to cancel an item already leased by a worker. Proactive scheduling has its own
operational API and page: `GET /api/proactive/status`, authenticated reads for
`/api/proactive/settings` and `/api/proactive/wakes`, and authenticated writes
for lifecycle/settings updates and cancelling an individual wake. These APIs
manage the Rust durable wake ledger; they do not expose the legacy Node process's
in-memory `scheduledWakes` map for migration capture.

Run from this directory:

```sh
cargo test
cargo run --bin RInBot -- --help
cargo run --bin RInBot -- --host 127.0.0.1 --port 3211 \
  --data-dir /tmp/rinbot-observe-test
```

The example starts an isolated observation-only instance and does not connect
to OneBot. Do not add `--connect-onebot` to a local smoke command: its default
WebSocket target is `ws://127.0.0.1:3001/onebot/v11/ws`, which may be the
currently running production NapCat instance.

For an isolated stability replay, the ignored `performance_replay` test can
write a reviewable JSON artifact when `RINBOT_PERF_TEST_REPORT_PATH` points to
a new absolute path in an existing directory. The file is created exclusively
(never overwritten) and contains every timestamped RSS/API/queue sample plus
the aggregate ledger reconciliation and tested binary SHA-256. A 24-hour run
uses 8,064 synthetic messages and 1,441 one-minute samples so the first-to-last
sample window itself covers at least 24 hours. `qqbotctl preflight` binds the
evidence JSON to this artifact hash and cross-checks each health sample, time
gap, queue limit, RSS statistics, ledger counts, and candidate binary hash.
This uses mock OneBot/model
dependencies and is not real NapCat/QQ acceptance.

For a privacy-preserving, read-only inventory of a legacy Node data directory,
run:

```sh
cargo run --bin rinbot-migrate -- audit --source-dir /path/to/node/data
```

After stopping the Node writer, create a verified local snapshot (the destination
must not already exist and must be outside the source tree):

```sh
cargo run --bin rinbot-migrate -- snapshot \
  --source-dir /path/to/node/data \
  --destination-dir /path/to/new/node-data-snapshot \
  --node-stopped
```

The JSON report (format v5) includes SQLite table/column names, aggregate row and state
counts, config leaf types, effective enabled feature names (after Node defaults
are applied), file counts/bytes for known data directories, and import-validity
plus fixed collection counts for the daily-moments and Qzone-interactions JSON archives. Archive
counts expose no record contents. Feature names
are the only configuration values exposed; secrets and other values are not
reported, and dynamic identifiers in config field paths are replaced with
`[redacted]`. The audit does not report message/config/file contents and does not
create, copy, or modify files. Its feature list uses the same logic as import,
so it can be reviewed before stopping Node. Run the eventual snapshot/import only after the Node writer has
been stopped; this inventory is not a consistent migration snapshot.
For database entries, `integrity_ok: null` means the database is absent and no
integrity check was applicable; `true`/`false` means the file was present and checked.
The explicit flag is not accepted as proof by itself: on macOS the command
also verifies that the `com.local.qq-agent-plus` LaunchAgent is unloaded and
that the configured strict `127.0.0.1:3210` listener is closed. Other
platforms verify the configured listener but cannot query launchd. Snapshot
writes files with user-only permissions, refuses an existing destination or
source-tree target, and verifies the staged copy before committing it. This
runtime stopped-writer check has not been exercised against production data.

The first-import command builds a new Rust data root: an unchanged, verified
copy of the complete old data tree is kept under `legacy/`, while the normalized
chat history, runs, outbox (including `unknown`), and the new Rust schema live
in `rinbot.sqlite`. It refuses an existing destination and requires the same
stopped-writer check. The process itself never stops Node; capture any required
in-memory scheduler projection first, then cleanly stop Node and keep it stopped
through the final snapshot/import.

```sh
cargo run --bin rinbot-migrate -- import \
  --source-dir /path/to/stopped/node/data \
  --destination-dir /path/to/new/rinbot-data \
  --node-stopped
```

`--connect-onebot` is for an isolated/replay environment until the migration
gate in the RPD has been completed. It must not be pointed at production data
or used alongside a production writer.

The Rust binary also has an explicitly gated text-reply path for isolated
replay (`--active`). It is disabled by default and refuses to start without
`--connect-onebot`, a model endpoint/model ID, and explicit numeric private or
group allowlists. Active mode must not be pointed at the production NapCat
instance before feature parity, data import, comparison, rollback, and stability
gates are complete. The codebase now includes feature-owned runtime modules and
management APIs beyond the initial text pipeline, but it is not yet equivalent
to the Node runtime. Basic conversation history and the
Asia/Shanghai `always` / `custom` / `deepseek-offpeak` time gate are supported;
the latter is supplied as `QQBOTD_TIME_CONTROL_JSON` and remains disabled by
default. Active mode now has durable per-conversation debounce and batch claiming:
the defaults mirror Node's 8–12 second wake delay, 20 second maximum batch wait,
100-message limit, and 32,000-character cap. Messages in a claimed batch share
one model call and one durable outbox item; private chats are not gated by the
group random-response percentage. These core semantics have unit and mock-NapCat
end-to-end coverage. Unresolved enabled features and missing imported feature
data are persisted as startup blockers; active mode refuses to start until both
audits are complete. A read-only audit on 2026-10-07 found 20 effective Node
features and no audit failures, but this is not a consistent snapshot and does
not prove feature parity. A stopped-writer import must report no unresolved
feature or data blockers, and production NapCat validation plus full crash/
rollback acceptance remain required before cutover.

The `qqbotctl` binary provides an offline `preflight`, a gated Rust-to-Rust
`restart-rust`, and a separate first-cutover `cutover-node-to-rust` operation.
Both operations default to preview and require every evidence gate, `--apply`,
and an exact confirmation string. Rust-to-Rust restart journals progress and
attempts to restore the previous retained Rust release if candidate readiness
or success-journal persistence fails. The first-cutover operation requires the
candidate already staged under the retained release root and the stable active
symlink already pointing to it; it requires Node to be stopped after final
snapshot/import, confirms launchd is unloaded and the write port is closed, then
revalidates the snapshot, imported data, outbox, candidate and LaunchAgent
evidence while Node is stopped. If that final check fails, it restores and
health-checks Node without starting RInBot. It then starts RInBot only after the
post-stop checks pass. It does not create the migration snapshot or import data.
Rust-to-Rust recovery also has an explicit isolated macOS launchd drill using
the retained RInBot binary, private API authentication and local mock OneBot.
The first Node-to-Rust path still requires its own isolated migration/recovery
drill. Both `--apply` modes also
share a private per-user OS file lock so concurrent qqbotctl invocations cannot
interleave service operations.

The offline preflight requires the Rust LaunchAgent to bind an explicit absolute
`--ui-dir` (or `QQBOTD_UI_DIR`) containing a non-empty, regular `index.html`;
this prevents an otherwise healthy API process from passing the service check
while its static admin UI points at a missing relative build directory.
The service must also configure `--api-token` or `QQBOTD_API_TOKEN` so that
administrator writes are available. The controller reads this credential from
the target plist and passes it to curl through stdin for readiness checks; it
does not put the credential in curl's command line or recovery journals.
The launchd review evidence is bound to both exact plist files, Node console
configuration, the candidate executable and a deterministic hash of the entire
bounded UI asset tree. Changing credentials, paths, ports or any JS/CSS asset
invalidates the old review. Generate the static review without service contact:

```sh
qqbotctl review-launchd \
  --node-data-dir /path/to/node-data \
  --rust-data-dir /path/to/staged-rinbot-data \
  --node-plist /path/to/node-agent.plist \
  --rust-plist /path/to/rinbot-agent.plist
```

This prints credential-free JSON evidence only. It does not attest to real QQ
acceptance, a completed data import, or a successful first handoff.

Example Rust restart:

```sh
cargo run --bin qqbotctl -- restart-rust \
  --node-data-dir /path/to/node-data \
  --rust-data-dir /path/to/imported-rinbot-data \
  --readiness-report /path/to/readiness.json \
  --node-plist /path/to/node-agent.plist \
  --rust-plist /path/to/rinbot-agent.plist \
  --active-link /path/to/releases/current \
  --candidate /path/to/releases/v-next/RInBot \
  --release-root /path/to/releases \
  --journal /path/to/operations/restart.json
```

Omit `--apply` to preview. Rust restart additionally requires
`--confirm RESTART-RUST-AFTER-GATES`. The first Node-to-Rust handoff uses the
same path/evidence arguments plus `--active-link`, `--candidate`,
`--release-root`, and a new `--journal`, with
`--confirm CUTOVER-NODE-TO-RUST-AFTER-GATES`. Neither command has been run
against production. Real QQ acceptance, final stopped-writer migration and the
separate first-cutover/Node recovery drill remain required.

The real launchd test uses a unique temporary label, an ephemeral local port,
temporary data, mock OneBot, and a failing executable to verify ten successful
restarts and automatic previous-version recovery. It cleans up the temporary
service and confirms its listener is closed. Run explicitly on macOS:

```sh
RINBOT_LAUNCHD_TEST_BINARY=/absolute/retained/RInBot \
RINBOT_LAUNCHD_TEST_REPORT_PATH=/absolute/new/launchd-restarts.json \
cargo test --manifest-path rinbot/Cargo.toml --release --locked --bin qqbotctl \
  isolated_launchd_rinbot_restarts_and_bad_candidate_restore_authenticated_readiness \
  -- --ignored --nocapture
```

`scripts/stage-api-auth.mjs` copies an existing Node console credential to a
repository-local `_staging/` RInBot plist. If Node has no console token, it
creates a private staged token and a mode-0600 `console-access.txt` beside the
plist. It does not load a service or modify Node configuration. Credentials
are never printed. The staged Rust data directory still requires final import.

A separate opt-in live transport probe uses only login-info reads and two
observe-only WebSocket sessions against explicit local NapCat URLs. It never
constructs a model, executor or production database, and saves only counts and
hashes. It does not certify text/image/voice sending or full feature acceptance:

```sh
RINBOT_NAPCAT_READ_CONFIG=/absolute/Node/config.json \
RINBOT_NAPCAT_READ_REPORT=/absolute/new/napcat-live-read.json \
cargo test --manifest-path rinbot/Cargo.toml --release --locked --test napcat_live_read \
  -- --include-ignored --nocapture
```
