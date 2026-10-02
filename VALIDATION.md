# Current baseline validation — 2026-10-02

The fixed application/dependency baseline was rebuilt and validated against
Fungi master `12f5aca0e7b71b27bced9d066ffe4d484dfd38f3`, containing merged PRs
#78, #79 and #85 (managed Docker removal). Fungi reports 0.7.1, but this is a
source build, not the old v0.7.1 release. The core source was exported from that
exact commit and built using the existing Cargo cache without changing the
other Fungi working branch. `cargo build --offline -p fungi` passed.

Application checks: `cargo fmt --package sftp-wasip2-experiment -- --check`,
`cargo test --locked --offline` (6 passed), and
`cargo build --locked --offline --release --target wasm32-wasip2` passed.
The dependencies remain russh 0.60.0 plus the exact saved patch, and clean
russh-sftp 2.1.1. See dependencies.lock.json for the full revisions and checksum.

The real daemon-managed smoke test passed all 19 checks, including modern scp,
SFTP operations, actual SSHFS mounts, stable data and generated host key across
service/daemon restart, service removal, and resource cleanup. This run also
passed repeat `apply --start`; the old branch limitation recorded below is
resolved by merged #78. The test config no longer contains Docker options.
Linux x86_64, rustc 1.97.1, OpenSSH 9.6p1 and SSHFS 3.7.3 were used.

WASM SHA-256:
`4f00d200a69814a5f61017926adaab447569115408b9cc90cd0affd09e2cca16`.

Only temporary local daemons, service data and mounts were used. The default
daemon and remote devices were not changed or trusted. These results cover
Linux clients, not macOS/Windows GUI clients, public deployment, or WASIp3.
No remote release, catalog publication or upstream PR was made in this check.

The following sections preserve the original historical evidence and failures;
their unmerged-branch and uncommitted-source statements apply to September 19.

# Experimental validation — 2026-09-19

## Fungi recipe follow-up

Added `sftp-wasip2.fungi.md`, using a local release build, localhost SSH,
service-owned appdata, and an automatically generated persistent Ed25519 host
key outside the SFTP file root. This is not yet an official catalog entry.

The first actual `service apply --start` exposed a Fungi bug: `run.env` was
assigned to the launcher environment but was not passed into WASI. The guest
ignored SFTP_FS_ROOT/SFTP_PORT and exited trying to access the default `data`
directory. The local Fungi patch in
`crates/daemon/src/runtime/helpers.rs` now passes each setting as `--env KEY=VALUE`
before the component path, instead of modifying the launcher environment.
The accompanying regression assertion checks placement, spaces/equal signs,
and that guest HOME does not overwrite the Android launcher HOME.

Tested Fungi commit: `054eeb00cc736784555b579a4fef0e4b69ed8f42` plus that local
patch and regression assertion. Fungi reports 0.7.1; the released 0.7.1 binary
does **not** identify this code. Validation:

- Fungi `cargo fmt --all -- --check` and `cargo build --offline`: passed.
- Fungi `cargo test --all --offline`: 313 passed, 0 failed, 4 existing ignored.
  Run outside the sandbox for local socket tests; the initial sandbox run was
  blocked by socket permission errors.
- SFTP `cargo test --locked --offline`: 6 passed, including persistent-key
  reuse and refusing to overwrite an invalid existing key.
- SFTP WASIp2 release build: passed. SHA-256:
  `4f00d200a69814a5f61017926adaab447569115408b9cc90cd0affd09e2cca16`.

Reproduce the daemon-managed run:

```bash
python3 scripts/smoke.py --fungi /path/to/patched/fungi \
  --wasm target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm \
  --recipe sftp-wasip2.fungi.md
```

Passed 19 checks (including repeated lifecycle checks): dry-run without creating
a service; initial apply/start; repeat apply retaining the running service and
local service ID; inspect/logs/connect; all ten client scenarios below; service
stop closing the listener; service start; daemon restart restoring the running
service with unchanged data/ID/generated key; another scp using existing
known_hosts; service removal. All test processes, mounts and temporary files
were cleaned. No default daemon, remote trust, published catalog, or existing
user services were changed.

Known Fungi branch limitation: repeat `apply --start` returns "already running".
This was reproduced and is not fixed here; PR #79 still needs the existing
master PR #78 idempotency fix integrated. The passing update scenario uses
plain `apply` on the running service. No PR #79 merge conflicts were resolved
in this recipe task. No commits, pushes or releases were made.

## Earlier direct-component run

Tested local changes on top of experiment commit
`953da5b042cce8d9074b4b88e61359e444514e3c`.

Environment:

- Linux x86_64, rustc 1.97.1.
- OpenSSH 9.6p1 (Ubuntu), SSHFS 3.7.3, FUSE 3.14.0.
- Fungi 0.7.1 built from `054eeb00cc736784555b579a4fef0e4b69ed8f42`,
  embedding Wasmtime 46.0.1.
- Pinned local dependency sources and russh patch described in README.

Checks:

- `cargo test --locked --offline`: 5 passed. Covers real file-handle identity
  across rename, APPEND ignoring supplied offset, read-only handle truncation
  rejection, failed read-open without directory creation, and native mode updates.
- `cargo build --locked --offline --release --target wasm32-wasip2`: passed.
- `python3 scripts/smoke.py --fungi <tested-fungi> --wasm <built-component>`:
  all 10 client scenarios passed (see below).

```text
PASS scp 2MiB binary upload/download, SHA256 and exit code 0
PASS scp overwrite truncates previous content
PASS scp empty file and multiple files
PASS scp -r upload/download with Unicode and spaces
PASS scp -p modification time in both directions (POSIX modes excluded on WASIp2)
PASS OpenSSH sftp navigation, mkdir, put/get, rename, rm, rmdir
PASS SSHFS mounted read/write, append, random writes and truncate
PASS SSHFS open handle across rename, editor-style replacement and deletion
PASS SSHFS unmount and remount
PASS component restart preserves data and host key accepted by existing known_hosts
PASS 10 scenarios; test mounts/processes/data cleaned
```

SHA-256 of the tested WASM:
`7bbe2ee0649ba5e0f0486d606aa8537d6b45ee09f64b7cd9b10a6af03e895466`.

The baseline transferred files but scp exited 1 because SSH exit-status was
missing. After fixing that, a `scp -p`/restart test exposed an incorrectly
reported read-only mode; the final run above includes that fix and verifies
that downloaded files remain writable.

All clients connected only to localhost, with temporary data, credentials,
known_hosts, and mount directories. Test-created components and SSHFS processes
were stopped, and mounts removed. No default Fungi daemon/configuration, remote
devices, recipes or user services were changed.

This validates the listed Linux client scenarios, not all scp options or all
SFTP clients. Windows/macOS GUI clients, legacy `scp -O`, `scp -R`, WASIp3,
permissions/ownership fidelity, performance and security hardening are not
included. No commits, pushes or releases were made.
