# sftp-wasip2-experiment

Minimal SFTP server experiment for `wasm32-wasip2` and Wasmtime, built on top of local `russh` and `russh-sftp` checkouts.

This app is a raw TCP service, so it uses a guest-owned listener with `wasmtime run -Sinherit-network`. It is not a `wasmtime serve`/WASI HTTP app.

## Scope

- Password auth, defaulting to `demo` / `demo`.
- SFTP subsystem over `russh-sftp`.
- Basic `std::fs` backend rooted at a WASI-preopened directory.
- Core SFTP operations: open, close, read, write, stat, directory listing, mkdir, remove, rename, and truncate.

## Current Limits

- The host key is generated on every startup.
- Public key auth is rejected.
- Symlink and SFTP extension requests are not implemented.
- `setstat`/`fsetstat` only handle file size changes.
- File IO uses synchronous `std::fs` through WASI filesystem hostcalls.
- Tokio networking for WASIp2 currently needs `RUSTFLAGS="--cfg tokio_unstable"`.

## Native Run

```bash
cargo run
```

Optional environment variables:

```bash
SFTP_BIND_HOST=0.0.0.0
SFTP_PORT=2222
SFTP_USERNAME=demo
SFTP_PASSWORD=demo
SFTP_FS_ROOT=./data
```

## WASI Build

```bash
rustup target add wasm32-wasip2
RUSTFLAGS="--cfg tokio_unstable" cargo build --release --target wasm32-wasip2
```

## WASI Run

```bash
mkdir -p data
RUSTFLAGS="--cfg tokio_unstable" \
SFTP_FS_ROOT=data \
wasmtime run -Sinherit-network --dir ./data::data \
  target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm
```

Then connect with an SFTP client:

```bash
sftp -P 2222 demo@127.0.0.1
```
