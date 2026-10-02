# sftp-wasip2-experiment

Experimental SFTP server compiled to a WASIp2 command component. A guest-owned
TCP listener runs SSH (`russh`) and its SFTP subsystem (`russh-sftp`). The goal is
ordinary OpenSSH `scp`/`sftp` transfers and SSHFS mounts, not a complete sshd.

## Build

This usable baseline deliberately keeps the tested sibling source checkouts.
[dependencies.lock.json](dependencies.lock.json) pins their exact revisions and
the russh patch checksum; Cargo.lock pins registry dependencies. The tested
dependencies are:

- `../russh`: `54857cab01a0b5bf89b7521744a10d0610d2b208` (0.60.0), with
  [`patches/russh-wasip2.patch`](patches/russh-wasip2.patch) applied.
- `../russh-sftp`: `c2590f9a41cf99c2fb3c672e045a9837a8adcb35` (2.1.1), unmodified.

For **fresh dependency directories**, run from this repository:

```bash
git clone https://github.com/Eugeny/russh.git ../russh
git -C ../russh checkout 54857cab01a0b5bf89b7521744a10d0610d2b208
git -C ../russh apply ../sftp-wasip2-experiment/patches/russh-wasip2.patch
git clone https://github.com/AspectUnk/russh-sftp.git ../russh-sftp
git -C ../russh-sftp checkout c2590f9a41cf99c2fb3c672e045a9837a8adcb35
```

Existing workspace checkouts may already have the patch; do not reapply it or
replace unrelated changes. `.cargo/config.toml` enables `tokio_unstable` for
Tokio's WASIp2 networking. Use the committed Cargo.lock:

```bash
rustup target add wasm32-wasip2
cargo test --locked
cargo build --locked --release --target wasm32-wasip2
```

## Package a local bundle

```bash
python3 scripts/package-release.py --check
python3 scripts/package-release.py --offline
```

Packaging requires a clean, committed application tree and exactly the pinned
dependency revisions/patch. It builds the WASIp2 release and creates a new
`dist/sftp-wasip2-<commit>/` directory plus a tar.gz, refusing to overwrite an
existing bundle. The directory contains a prebuilt WASM, a colocated recipe,
SHA256SUMS, build provenance, validation notes, and a source archive. The bundle
recipe does not require this checkout or sibling repositories at runtime.

After extracting the bundle, run `sha256sum -c SHA256SUMS`, then apply its recipe
with a compatible Fungi daemon. No public release or catalog entry is created
by this script. New upstream adaptation is developed separately and does not
change this baseline's dependency pins.

## Run as a Fungi service

The [local recipe](sftp-wasip2.fungi.md) uses Fungi's run-only Wasmtime provider.
It requires merged PR #79 (including the guest-environment forwarding fix).
The current validation uses Fungi master commit
`12f5aca0e7b71b27bced9d066ffe4d484dfd38f3`; the older released 0.7.1 is not sufficient.
See [VALIDATION.md](VALIDATION.md).
With a compatible Fungi daemon running and the component built above:

```bash
fungi service apply sftp-demo ./sftp-wasip2.fungi.md --dry-run
fungi service apply sftp-demo ./sftp-wasip2.fungi.md --start
fungi service inspect sftp-demo --verbose
fungi service logs sftp-demo --tail 30
fungi service connect sftp-demo sftp
```

The default endpoint is `127.0.0.1:2222`, with username/password `demo`/`demo`.
Files live in the service's persistent appdata `files/` subdirectory. A stable
host key is generated alongside that directory on first start, outside the
SFTP file root. Service stop/start, repeated apply, and daemon restart retain
the files and key. There is no need to expose your host home or change Fungi's
host-path allowlist. Storage is under
`<fungi-dir>/appdata/services/<local-service-id>/`; the resolved mount path is
recorded in `<fungi-dir>/services/<local-service-id>/service.yaml`.

```bash
fungi service stop sftp-demo
fungi service start sftp-demo
# When finished, back up any files you want to retain before removal:
fungi service remove sftp-demo
```

This recipe references the local build. It has not been published to the
official recipe catalog, so use the manifest path rather than `--recipe`.
For multiple instances, change both port fields in separate recipe copies.
Repeated `apply --start` is tested on the current Fungi master and preserves
the local service identity, files and host key.

## Run the component directly

Create storage and, optionally, a stable host key. The key is outside the exposed
SFTP data directory. Generate it once, then reuse it across restarts:

```bash
mkdir -p data state
ssh-keygen -q -t ed25519 -N '' -f state/host_ed25519

fungi run -Scli -Stcp -Sinherit-network \
  --dir ./data::data --dir ./state::state \
  --env SFTP_BIND_HOST=127.0.0.1 --env SFTP_PORT=2222 \
  --env SFTP_FS_ROOT=data --env SFTP_HOST_KEY=state/host_ed25519 \
  target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm
```

`wasmtime run` accepts the same runtime options. Environment variables must be
passed with `--env` to enter the guest; setting them only on the host command is
not sufficient.

Configuration (guest environment):

| Variable | Default | Meaning |
| --- | --- | --- |
| `SFTP_BIND_HOST` | `0.0.0.0` | Listen address; examples bind localhost |
| `SFTP_PORT` | `2222` | TCP port |
| `SFTP_USERNAME` | `demo` | Password-auth username |
| `SFTP_PASSWORD` | `demo` | Password-auth password |
| `SFTP_FS_ROOT` | `data` | Guest-visible shared directory |
| `SFTP_HOST_KEY` | unset | Unencrypted OpenSSH private key path; generates and saves Ed25519 if missing (parent must exist); unset generates an ephemeral key |

For native development, set these variables on `cargo run --locked` directly.

## Use ordinary clients

Enter `demo` when prompted for the password:

```bash
scp -P 2222 local.txt demo@127.0.0.1:/remote.txt
scp -P 2222 demo@127.0.0.1:/remote.txt downloaded.txt
scp -P 2222 -r local-directory demo@127.0.0.1:/
sftp -P 2222 demo@127.0.0.1
```

OpenSSH 9.0+ scp uses SFTP by default. Legacy `scp -O`, remote shell/exec commands,
and `scp -R` remote execution are not implemented.

For a filesystem mount on Linux with SSHFS/FUSE available:

```bash
mkdir -p /tmp/sftp-experiment-mount
sshfs -p 2222 demo@127.0.0.1:/ /tmp/sftp-experiment-mount
# Browse, read/write, append, truncate, rename and delete through the mount.
fusermount3 -u /tmp/sftp-experiment-mount
```

## Reproduce client validation

Requirements: Python 3, `ssh`, `scp`, `sftp`, `ssh-keygen`, and for mounts,
`sshfs`, `fusermount3`, and an accessible `/dev/fuse`.

```bash
python3 scripts/smoke.py --fungi /path/to/fungi \
  --wasm target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm

# Exercise the same clients through Fungi's actual service manager:
python3 scripts/smoke.py --fungi /path/to/fungi \
  --wasm target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm \
  --recipe sftp-wasip2.fungi.md
```

The script uses temporary data, a temporary known_hosts file, a generated host
key, and localhost. It exercises real WASIp2 execution and checks hashes, client
exit codes, filesystem operations, mounts, and restarts. Test processes and
mounts are cleaned up. Pass `--skip-mount` only to explicitly omit SSHFS testing;
this does not count as mount validation. See [VALIDATION.md](VALIDATION.md).

The recipe variant starts its own temporary daemon/configuration with community
relays disabled and no trusted devices. It stages the WASM at the recipe's
relative source path and substitutes a free port in a temporary recipe copy,
then checks dry-run, apply/start,
inspect/logs/connect, stop/start, daemon restart and removal. It never uses or
restarts your default daemon. The first SSH client connection records the key;
subsequent connections reject changed keys.

## Deliberate experimental limits

- Password authentication only; no public-key auth, shell, or legacy SCP.
- WASIp2 reports fixed directory/file modes (0755/0644); chmod/chown are accepted
  as no-ops. `scp -p` timestamps work, but exact POSIX modes/ownership are not
  preserved. Native Unix SETSTAT can update modes.
- Files, timestamps, and open file handles are real. Symlink operations and SFTP
  extensions such as fsync/statvfs are not implemented.
- Synchronous file I/O, eager directory listing, and resource/security hardening
  remain future work. This is an isolated experiment, not a hardened service.
- The current two dependency versions are intentionally retained. New upstream
  versions and WASIp3 are separate follow-up experiments.
