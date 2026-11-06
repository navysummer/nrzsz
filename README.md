# nrzsz

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
![Platform: macOS · Linux · Windows](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)

A Rust implementation of file transfer tools compatible with
[`trzsz-go`](https://github.com/trzsz/trzsz-go) (`trz` / `tsz`) and inspired
by [`lrzsz`](https://github.com/trzsz/lrzsz) (`rz` / `sz`). It provides three
binaries — `nsz` (send), `nrz` (receive) and `nrzsz` (wrapper / relay) — plus
the `nrzsz-core` library crate.

The project implements the trzsz wire protocol (V1–V4) over a TCP tunnel, with
tmux support, drag-and-drop upload, resume, compression and ZMODEM passthrough.

> **Status**: implemented, tested and verified against real `trzsz-go` and
> `lrzsz` binaries. See [AGENTS.md](AGENTS.md) for the complete feature /
> compatibility matrix and the implementation roadmap.

## Features

- **trzsz wire protocol V1–V4** — version negotiation with automatic downgrade.
- **Compression** — zstd + zlib, auto-detected (`-c auto/yes/no`).
- **Resume** — V3 prefix-hash resume (MD5, 10 MB steps).
- **Directory transfer** — recursive, nested, byte-identical (`-d` / `-r`).
- **Background fork** — `-f` detaches the transfer to the background.
- **TCP tunnel** — transfers run over a `::TRZSZ:` tunnel bridge; binary mode
  and fork work over the tunnel.
- **tmux support** — normal mode and `-CC` control mode output routing,
  trigger prefix parsing, drag `%begin`/`%end` acknowledgements.
- **Drag-and-drop upload** — iTerm2 / Warp / bracketed-paste prefixes.
- **OSC52 clipboard** — `-o` integrates with the terminal clipboard.
- **ZMODEM passthrough** — `-z` uses a self-contained ZMODEM engine (no
  external `rz`/`sz` needed) verified against real `lrzsz` binaries.
- **Relay mode** — `-r` bridges two hosts through `NrzszRelay`.
- **Progress UI** — progress bars with optional gradient color pair.
- **Cross-platform** — macOS, Linux and Windows (build fixed; Windows runtime
  untested — see [AGENTS.md](AGENTS.md)).
- **Embedding API** — `NrzszOptions` / `TransferStateHandler` / `NrzszFilter`
  for programmatic use.

## Installation

### From source

```bash
cargo build --release
# Binaries:
#   target/release/nsz
#   target/release/nrz
#   target/release/nrzsz
```

### Via cargo install

```bash
cargo install --path nsz
cargo install --path nrz
cargo install --path nrzsz
```

### Copy to PATH

```bash
cp target/release/{nsz,nrz,nrzsz} /usr/local/bin/
```

## Quick start

### Send files (local → remote)

**Step 1 — on the remote machine**, start the receiver:

```bash
nrz /tmp
```

It prints a trigger line such as:

```
::TRZSZ:TRANSFER:R:0.1.0:37692865470000:58263
```

**Step 2 — on the local machine**, send files:

```bash
nsz file1.txt file2.png ./mydir/
```

### Receive files (remote → local)

**Step 1 — on the local machine**, start the receiver:

```bash
nrz ~/downloads
```

**Step 2 — on the remote machine**, send files:

```bash
nsz bigfile.zip
```

### Through SSH

Wrap the remote command with `nrzsz`:

```bash
# Upload local files to remote
nrzsz ssh user@host nrz /tmp

# Download files from remote
nrzsz ssh user@host nsz file1.txt file2.zip
```

`nrzsz` is the client-side wrapper: it detects the server's trigger, opens the
TCP tunnel and routes the transfer through it.

### With tmux

Everything works transparently inside tmux. The tool auto-detects tmux `-CC`
control mode and adjusts output encoding accordingly:

```bash
# Inside tmux, just use as normal
nrz /tmp
nsz myfile.txt
```

### With zmodem (lrzsz compatibility)

Enable ZMODEM passthrough (raw `rz` / `sz` interop):

```bash
nrzsz -z ssh user@host sz file.txt
```

### Relay mode

Use as a relay between two hosts:

```bash
nrzsz -r ssh user@host
```

## CLI Reference

### `nsz` — send files

```
nsz [OPTIONS] [FILES]...

Options:
  -q, --quiet                 Quiet (hide progress bar)
  -o, --overwrite             Overwrite existing files
  -b, --binary                Binary transfer mode (faster for binary files)
  -e, --escape                Escape all known control characters
  -d, --directory             Transfer directories and files
  -r, --recursive             Same as -d
  -f, --fork                  Background transfer (implies -q)
  -c, --compress <TYPE>       Compression: auto/yes/no [default: auto]
  -B, --bufsize <SIZE>        Max buffer chunk size (1K<=N<=1G) [default: 10M]
  -t, --timeout <SECS>        Timeout per chunk (<=0 = never) [default: 20]
  -a, --ascii                 ASCII (text) mode; forces binary off
      --resume                Resume interrupted file transfer
  -p, --protect               Skip files that already exist
  -w, --windowsize <N>        Window size in bytes, capped by -B
  -Y, --overwrite-or-skip     Overwrite if source newer, otherwise skip
  -E, --rename                Force rename if file already exists
  -+, --append                Append to existing files
  -V, --version               Print version
```

### `nrz` — receive files

```
nrz [OPTIONS] [PATH]

Options:
  -q, --quiet                 Quiet (hide progress bar)
  -o, --overwrite             Overwrite existing files
  -b, --binary                Binary transfer mode
  -e, --escape                Escape all known control characters
  -d, --directory             Transfer directories and files
  -r, --recursive             Same as -d
  -f, --fork                  Background transfer (implies -q)
  -c, --compress <TYPE>       Compression: auto/yes/no [default: auto]
  -B, --bufsize <SIZE>        Max buffer chunk size (1K<=N<=1G) [default: 10M]
  -t, --timeout <SECS>        Timeout per chunk (<=0 = never) [default: 20]
  -a, --ascii                 ASCII (text) mode; forces binary off
      --resume                Resume interrupted file transfer
  -p, --protect               Skip files that already exist
  -w, --windowsize <N>        Window size in bytes, capped by -B
  -Y, --overwrite-or-skip     Overwrite if source newer, otherwise skip
  -E, --rename                Force rename if file already exists
  -+, --append                Append to existing files
  -F, --file <FILE>           Absolute path of a file/dir to upload (repeatable)
  -V, --version               Print version
```

### `nrzsz` — wrapper (filter / relay)

Wraps an SSH or other command to intercept transfer triggers:

```
nrzsz [OPTIONS] COMMAND [ARGS...]

Options:
  -h, --help                  Show help and exit
  -v, --version               Show version and exit
  -r, --relay                 Run as a relay server
  -t, --tracelog              Enable trace log for debugging
  -d, --dragfile              Enable drag file(s) to upload
  -z, --zmodem                Enable zmodem lrzsz (rz / sz)
  -E, --rename                zmodem: rename if the target file exists
  -o, --osc52                 Enable clipboard (OSC52) integration
  -u, --upload <FILE>         Upload file(s) to remote (repeatable)
  -D, --download-dir <DIR>    Directory to save downloaded files
```

**Examples:**

```bash
# Wrap SSH with the nrzsz filter
nrzsz ssh user@host bash

# Upload specific files on connect
nrzsz -u localfile.txt ssh user@host bash

# Auto-download to a specific directory
nrzsz -D ~/incoming ssh user@host bash

# Enable zmodem support
nrzsz -z ssh user@host bash
```

## Configuration

The configuration file is `~/.trzsz.conf` (Go-compatible, `key = value`
format), falling back to `~/.nrzsz.conf` (JSON) and then a legacy
`~/.nrzsz_legacy.conf` (`key = value`) for compatibility.

### `~/.trzsz.conf` (Go-compatible, `key = value`)

```ini
# Comments start with #
allowedAutoUploadDirs = /tmp, /home/user/shared
defaultUploadPath = /tmp
defaultDownloadPath = ~/downloads
dragFileUploadCommand = nrz
progressColorPair = cyan blue
zmodemAutoRename = on
```

Keys are matched case-insensitively (lowercased at parse time).

### `~/.nrzsz.conf` (JSON)

```json
{
  "defaultUploadPath": "/tmp",
  "defaultDownloadPath": "~/downloads",
  "dragFileUploadCommand": "nrz",
  "progressColorPair": ["cyan", "blue"],
  "allowedAutoUploadDirs": ["/tmp", "/home/user/shared"],
  "zmodemAutoRename": false
}
```

### Legacy `~/.nrzsz_legacy.conf` (`key = value`)

```ini
DefaultUploadPath = /tmp
DefaultDownloadPath = ~/downloads
DragFileUploadCommand = nrz
ProgressColorPair = B14FFF 00FFA3
```

## Library API

Use `nrzsz-core` as a Rust library:

```rust
use nrzsz_core::api::{NrzszOptions, TransferStateHandler};
use nrzsz_core::filter::NrzszFilter;
use nrzsz_core::protocol::Config;

// Build a filter from options (wires dragfile/zmodem/osc52 setters)
let opts = NrzszOptions {
    quiet: true,
    compress: Some("auto".to_string()),
    ..Default::default()
};
let mut filter: NrzszFilter = opts.build_filter();

// Wire a transfer-state callback (true = started, false = finished/stopped)
let mut handler = TransferStateHandler::new();
handler.set_callback(Box::new(|started| {
    println!("transfer state: {started}");
}));
handler.apply_to(&mut filter);

// Caller-side configuration is done with the filter's setters:
//   filter.set_tunnel_connector(...);
//   filter.set_upload_files(vec![...]);
//   filter.set_default_download_path("~/downloads".into());

// Or just build a bare Config:
let config: Config = opts.to_config();
```

For relay mode, construct an `NrzszRelay` directly with your own streams:

```rust
use nrzsz_core::relay::NrzszRelay;

let relay = NrzszRelay::new(client_in, client_out, server_in, server_out);
relay.run()?;
```

## Compatibility

### trzsz-go (primary target)

`trzsz-go` v1.2.0+ (`trz` / `tsz`) is the primary compatibility target. The
`::TRZSZ:` wire namespace, protocol V1–V4, JSON framing and the TCP tunnel are
matched. Both `::TRZSZ:` and the legacy `::NRZSZ:` namespace are accepted.

### lrzsz (raw ZMODEM)

`lrzsz` (`rz` / `sz`) is the C reference for raw ZMODEM. `nrzsz` implements
the trzsz wire protocol (a superset adding JSON config/action framing and a
TCP tunnel), so direct raw-ZMODEM interop goes through `-z` passthrough: a
self-contained ZMODEM engine bridges raw streams to a real `rz` / `sz`. The
engine is verified end-to-end against real `lrzsz` binaries.

### Feature coverage vs trzsz-go

See [AGENTS.md](AGENTS.md) for the full comparison matrix (`§5`), the
`lrzsz` flag-by-flag comparison (`§6`) and the implementation roadmap (`§3`).

## Project layout

```
.
├── nrzsz-core/     # Library crate: protocol, transfer, filter, relay, tmux, zmodem…
│   ├── src/
│   │   ├── protocol.rs    # Wire format, Config, framing
│   │   ├── transfer.rs    # V1–V4 transfer engine
│   │   ├── filter.rs      # Wrapper (trigger detection, tunnel bridge)
│   │   ├── relay.rs       # Relay between hosts
│   │   ├── zmodem.rs      # Self-contained ZMODEM engine (-z)
│   │   ├── tmux.rs        # tmux normal mode
│   │   ├── tmuxcc.rs      # tmux -CC control mode
│   │   └── api.rs         # Embedding API (NrzszOptions, TransferStateHandler)
│   └── tests/             # Protocol + zmodem e2e tests
├── nsz/                   # nsz binary (send)
├── nrz/                   # nrz binary (receive)
├── nrzsz/                 # nrzsz binary (wrapper / relay)
├── AGENTS.md              # Full feature / compatibility matrix + roadmap
└── LICENSE                # Apache-2.0
```

## Development

```bash
# Build (release, whole workspace)
cargo build --release --workspace

# Run the full test suite (unit + protocol + tunnel e2e + relay e2e)
cargo test --workspace

# ZMODEM smoke tests need lrzsz (sz/rz) on PATH; they are #[ignore]d:
cargo test --workspace -- --ignored zmodem
```

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

## Acknowledgements

- [trzsz-go](https://github.com/trzsz/trzsz-go) — wire-protocol compatibility
  target.
- [lrzsz](https://github.com/trzsz/lrzsz) — the original `rz` / `sz`; ZMODEM
  reference used to validate the passthrough engine.
