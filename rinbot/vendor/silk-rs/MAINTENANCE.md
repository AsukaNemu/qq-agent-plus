# Local silk-rs build patch

Source: [`lz1998/silk-rs`](https://github.com/lz1998/silk-rs), crates.io
`silk-rs 0.2.0`. The upstream wrapper's MIT `LICENSE` and Skype's BSD notices
in the native source headers are preserved.

The C codec and Rust runtime implementation are unchanged. The local changes
are limited to:

- `Cargo.toml`: bindgen 0.59 -> 0.72.1, with only its library `runtime` feature.
- `build.rs`: use the current `bindgen::CargoCallbacks::new()` constructor.

The old bindgen CLI/logger defaults pulled in unmaintained `clap 2`,
`ansi_term`, and `atty`. The dependency needs bindgen's library API only.
Upstream has no newer published release, so this small build patch lives here
instead of depending on an unrelated codec implementation or a moving Git ref.

`UPSTREAM-SHA256.json` records hashes of the original files before these two
changes (`Cargo.toml` is the upstream `Cargo.toml.orig`). Packaging metadata,
the upstream `Cargo.lock`, and registry cache markers are deliberately omitted.
Do not edit the codec during build-tool maintenance. Native codec changes need
their own review and compatibility evidence.

The parent crate's ASR tests use a synthetic fixture produced by the original
crates.io release and compare decoded PCM bytes by SHA-256. The normal Rust
1.88/stable CI runs these tests; local macOS/ARM and Linux CI cover both hosts.
