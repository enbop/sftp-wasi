# sftp-wasi validation — 2026-10-02

The published project uses Git dependencies, not sibling source checkouts.

| Component | Tested revision |
| --- | --- |
| russh fork | `enbop/russh` at `e99d6fc8ce41c7b22082b357cd411c2089c22d0b` |
| Fork base | upstream release `v0.63.3`, `f33baf439be8c59c49cb6cb2ae5976c579495bdf` |
| russh-sftp | upstream 2.1.1 at `c2590f9a41cf99c2fb3c672e045a9837a8adcb35`, unmodified |
| Fungi | master `12f5aca0e7b71b27bced9d066ffe4d484dfd38f3` |

The fork adaptation commit is directly based on the release tag, without the
later upstream main commits. The consumer handles the newer russh
`ChannelOpenHandle` API by explicitly accepting session channels.

Environment: Linux x86_64, rustc 1.97.1, OpenSSH 9.6p1, SSHFS 3.7.3.
Fungi reports 0.7.1, but this source build includes merged #78/#79/#85;
the old v0.7.1 release is not equivalent.

Passed application checks:

- `cargo fmt --package sftp-wasi -- --check`.
- `cargo build --locked --release --target wasm32-wasip2`.
- `cargo test --locked`: 6 passed, 0 failed.
- `python3 scripts/package-release.py --check --offline`: Cargo resolves the
  two exact Git revisions from Cargo.toml/Cargo.lock.
- `python3 scripts/smoke.py --fungi <tested-fungi> --wasm <built-wasm>
  --recipe sftp-wasi.fungi.md`: all 19 checks passed.

The real client checks cover 2 MiB binary upload/download with hashes and exit
codes, overwrite/truncate, empty and multiple files, recursive copies with
Unicode/spaces, scp -p timestamps, SFTP navigation/rename/delete, actual SSHFS
mounts with append/random writes/truncate, open handles across rename,
editor-style replacement, and unmount/remount. Fungi checks cover dry-run,
apply/start, repeated apply --start with stable identity, inspect/logs/connect,
stop/start, daemon restart with stable data/host key, and service removal.

WASM SHA-256:
`c5dfd8a70692b0a1d521fa9346979baa4e17b1454dceeb2b6e78b7a465aadf32`.

The russh release-based patch also passed library compilation for WASIp2,
native Linux and browser `wasm32-unknown-unknown`, plus cargo formatting.
WASIp2 used `--cfg tokio_unstable`, no default features, and `ring`.
Browser compilation additionally used `ring/wasm32_unknown_unknown_js`.
Compiler warnings remain; browser execution and the full upstream native test
suite are not claimed. An upstream-owned runtime fixture and CI remain follow-up
work before proposing the adaptation upstream.

Tests used only temporary localhost daemons, data, credentials and mount
directories. Test-created processes/mounts were cleaned; no default Fungi
daemon or remote device trust was modified. macOS/Windows GUI clients, legacy
SCP, WASIp3, exact POSIX ownership/modes, performance and security hardening are
outside this experimental validation. The Fungi recipe is local-build; no
official recipe catalog entry or GitHub binary release is implied.
