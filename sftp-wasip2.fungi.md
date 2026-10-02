---
fungi: service/v1
id: sftp-wasip2

run:
  provider: wasmtime
  source:
    file: ./target/wasm32-wasip2/release/sftp-wasip2-experiment.wasm
  env:
    SFTP_BIND_HOST: 127.0.0.1
    SFTP_PORT: "2222"
    SFTP_FS_ROOT: appdata/files
    SFTP_HOST_KEY: appdata/host_ed25519
  mounts:
    - from: $fungi.service.data
      to: appdata

publish:
  sftp:
    tcp:
      port: 2222
    client:
      kind: ssh
---

# SFTP WASIp2 experiment

Local-build recipe for Fungi's run-only Wasmtime provider (merged PR #79).
Tested on core commit 12f5aca0e7b71b27bced9d066ffe4d484dfd38f3; see VALIDATION.md.
Build the component in this repository before applying this file. The WASM path
is relative to this recipe, so apply it from any working directory.

```bash
cargo build --locked --release --target wasm32-wasip2
fungi service apply sftp-demo ./sftp-wasip2.fungi.md --dry-run
fungi service apply sftp-demo ./sftp-wasip2.fungi.md --start
fungi service inspect sftp-demo --verbose
fungi service connect sftp-demo sftp
```

Use modern OpenSSH scp, sftp, or SSHFS at the printed address. The experimental
defaults are username `demo`, password `demo`; the listener binds localhost.
`client.kind: ssh` describes the SSH transport; no shell or exec is available.
Legacy `scp -O` is unsupported. If changing the port, update both SFTP_PORT and
publish.sftp.tcp.port. Each instance needs a distinct port.

Files are stored in `$fungi.service.data/files`. On first start, the server
creates `$fungi.service.data/host_ed25519`, outside the SFTP file root, and reuses
it across service and daemon restarts. No host-path allowlist changes are needed.
The resolved host mount path is recorded in
`<fungi-dir>/services/<local-service-id>/service.yaml`.
Back up files before removing the service; this recipe does not define backup
or retention behavior.

This is a local recipe, not a published catalog entry: use the file path,
not `--recipe sftp-wasip2`. Release artifact publication and a catalog entry
can follow when the experiment is ready to distribute.
