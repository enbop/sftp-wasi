# sftp-wasi

Experimental SFTP server compiled to a WASIp2 command component. Supports modern
OpenSSH `scp`, `sftp`, and SSHFS mounts. One component owns its SSH TCP listener
and file handles; it is not a complete sshd and provides no remote shell.

## Build

```sh
git clone https://github.com/enbop/sftp-wasi.git
cd sftp-wasi
rustup target add wasm32-wasip2
cargo test --locked
cargo build --locked --release --target wasm32-wasip2
```

No sibling source checkouts or manually applied patches are required.
`.cargo/config.toml` enables `tokio_unstable` for WASIp2 sockets. Validation uses
Rust 1.97.1; see [VALIDATION.md](VALIDATION.md) for the exact test scope.

Dependencies are fixed by full Git revisions in Cargo.toml and by Cargo.lock:

- [enbop/russh](https://github.com/enbop/russh/tree/wasip2-v0.63.3), revision
  `e99d6fc8ce41c7b22082b357cd411c2089c22d0b`. This WASIp2 adaptation starts
  directly from upstream release **v0.63.3**, not subsequent upstream main.
- [AspectUnk/russh-sftp](https://github.com/AspectUnk/russh-sftp), revision
  `c2590f9a41cf99c2fb3c672e045a9837a8adcb35` (2.1.1), unmodified.

## Run with Fungi

Use a Fungi CLI and daemon containing merged PR #79, including its WASI guest
environment fix. The old v0.7.1 release does not contain these changes. The
tested core commit is recorded in VALIDATION.md.

With the component built and a compatible daemon running:

```sh
fungi service apply sftp-demo ./sftp-wasi.fungi.md --dry-run
fungi service apply sftp-demo ./sftp-wasi.fungi.md --start
fungi service inspect sftp-demo --verbose
fungi service logs sftp-demo --tail 30
fungi service connect sftp-demo sftp
```

The [recipe](sftp-wasi.fungi.md) listens on `127.0.0.1:2222`, with experimental
username/password `demo`/`demo`. Files live in `$fungi.service.data/files`.
On first start a persistent Ed25519 host key is generated beside that directory,
outside the SFTP file root. Repeated apply, service restart and daemon restart
retain the files and key. The recipe requires no host-path allowlist changes.

```sh
scp -P 2222 local.txt demo@127.0.0.1:/remote.txt
scp -P 2222 demo@127.0.0.1:/remote.txt downloaded.txt
scp -P 2222 -r local-directory demo@127.0.0.1:/
sftp -P 2222 demo@127.0.0.1

# Linux with SSHFS/FUSE installed:
mkdir -p /tmp/sftp-wasi-mount
sshfs -p 2222 demo@127.0.0.1:/ /tmp/sftp-wasi-mount
fusermount3 -u /tmp/sftp-wasi-mount
```

Use `fungi service stop sftp-demo` / `start sftp-demo` to control it. Back up
files you want to keep before `fungi service remove sftp-demo`.
For multiple instances, change both `SFTP_PORT` and `publish.sftp.tcp.port`.
This is a local-build recipe, not an official catalog entry; apply the file
path rather than `--recipe sftp-wasi`.

## Direct component execution

```sh
mkdir -p data state
fungi run -Scli -Stcp -Sinherit-network \
  --dir ./data::data --dir ./state::state \
  --env SFTP_BIND_HOST=127.0.0.1 --env SFTP_PORT=2222 \
  --env SFTP_FS_ROOT=data --env SFTP_HOST_KEY=state/host_ed25519 \
  target/wasm32-wasip2/release/sftp-wasi.wasm
```

`wasmtime run` accepts the same runtime options. Guest environment variables
must be passed with `--env`; setting only the host environment is insufficient.

| Variable | Default | Meaning |
| --- | --- | --- |
| `SFTP_BIND_HOST` | `0.0.0.0` | Listener address; recipe/examples use localhost |
| `SFTP_PORT` | `2222` | Listener port |
| `SFTP_USERNAME` | `demo` | Password-auth username |
| `SFTP_PASSWORD` | `demo` | Password-auth password |
| `SFTP_FS_ROOT` | `data` | Guest-visible file root |
| `SFTP_HOST_KEY` | unset | OpenSSH private-key path; creates Ed25519 if missing, parent must exist; unset uses an ephemeral key |

## Reproduce validation and package

Requires Python 3.11+, OpenSSH clients, and SSHFS/FUSE for mount checks.

```sh
python3 scripts/smoke.py --fungi /path/to/compatible/fungi \
  --wasm target/wasm32-wasip2/release/sftp-wasi.wasm \
  --recipe sftp-wasi.fungi.md

python3 scripts/package-release.py --check
python3 scripts/package-release.py --offline
```

The smoke test creates an isolated temporary daemon with no trusted devices or
community relays. It exercises real scp/SFTP/SSHFS, repeated apply/start,
inspect/logs/connect, stop/start, daemon restart and removal, then cleans its
processes, mounts and files. It does not modify the default daemon. Omit
`--recipe` to test direct execution; use `--skip-mount` only to explicitly omit
SSHFS (that does not count as mount validation).

Packaging requires a clean committed tree, checks the Git dependency revisions
against Cargo resolution, and creates `dist/sftp-wasi-<commit>/` plus a tar.gz.
The bundle includes WASM, a colocated recipe, SHA256SUMS, build provenance and
source archive. Verify `sha256sum -c SHA256SUMS` after extraction, then apply
the bundled recipe. The script does not publish a GitHub release or catalog.

## Experimental limits

- Modern scp's SFTP transport only: no legacy `scp -O`, remote shell/exec or
  `scp -R`; password authentication only.
- WASIp2 reports fixed modes (directories 0755, files 0644). chmod/chown are
  accepted as no-ops; timestamps work, exact modes/ownership are not preserved.
- Symlink operations and SFTP extensions such as fsync/statvfs are not provided.
- Synchronous file I/O and eager directory listing; performance and security
  hardening remain future work. Current runtime validation uses Linux clients.
- No WASIp3 or russh-sftp 3.x support is claimed.
