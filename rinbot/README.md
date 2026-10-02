# RInBot Rust project

This directory is the isolated RInBot implementation described by
`docs/RPD-QQ-Bot-Rust-Refactor.md`. It is deliberately separate from the
existing Node.js runtime while the migration is in progress.

The first prototype is observation-only by default:

- it uses `rinbot/var/` as an independent SQLite data directory;
- it listens on `127.0.0.1:3211`, so it cannot take the current console port;
- it does not connect to NapCat unless `--connect-onebot` is explicitly passed;
- it never sends OneBot actions in observation mode;
- the in-memory work queue contains inbox IDs, not message payloads.
- non-loopback API binding is rejected unless `QQBOTD_API_TOKEN` is configured;
  authenticated API calls use `Authorization: Bearer ...` or a session cookie.

The local console is served by the same Rust process. Its read-only API includes
`/api/health`, `/api/status`, `/api/snapshot`, `/api/config`, paged
`/api/messages`, `/api/outbox/unknown`, and `/api/events`; `/api/config` masks
secret values and exposes only whether tokens are configured.

Once a Rust toolchain is available, run from this directory:

```sh
cargo test
cargo run --bin RInBot -- --help
cargo run --bin RInBot -- --connect-onebot
```

`--connect-onebot` is for an isolated/replay environment until the migration
gate in the RPD has been completed. It must not be pointed at production data
or used alongside a production writer.
