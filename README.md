# nrzsz (nsz / nrz) — trzsz compatible file transfer

A Rust implementation of file transfer, similar to `lrzsz` (rz/sz), compatible with `tmux` and `trzsz-go` (trz/tsz) v1.2.0+.

## Installation

### From source

```bash
cargo build --release
# Binaries at: target/release/nsz, target/release/nrz, target/release/nrzsz
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

**Step 1:** On the **remote** machine, start the receiver:

```bash
nrz /tmp
```

This prints a trigger line like:

```
:::TRZSZ:TRANSFER:R:0.1.0:37692865470000:58263
```

**Step 2:** On the **local** machine, send files:

```bash
nsz file1.txt file2.png ./mydir/
```

### Receive files (remote → local)

**Step 1:** On the **local** machine, start the receiver:

```bash
nrz ~/downloads
```

**Step 2:** On the **remote** machine, send files:

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

### With tmux

Everything works transparently in tmux. The tool automatically detects tmux CC mode and adjusts behavior:

```bash
# Inside tmux, just use as normal
nrz /tmp
nsz myfile.txt
```

### With zmodem (lrzsz compatibility)

Enable zmodem passthrough:

```bash
nrzsz -z ssh user@host sz file.txt
```

### Relay mode

Use as a relay between two hosts:

```bash
nrzsz -r ssh user@host
```

## CLI Reference

### `nsz` — Send files

```
nsz [OPTIONS] [FILES]...

Options:
  -q, --quiet                Quiet (hide progress bar)
  -o, --overwrite            Overwrite existing files
  -b, --binary               Binary transfer mode (faster for binary files)
  -e, --escape               Escape control characters
  -d, --directory            Transfer directories
  -r, --recursive            Same as -d
  -f, --fork                 Background transfer
  -c, --compress <TYPE>      Compression: auto / yes / no [default: auto]
  -B, --bufsize <SIZE>       Buffer chunk size (1K~1G) [default: 10M]
  -t, --timeout <SECS>       Timeout per chunk (0=never) [default: 20]
  -V, --version              Print version
```

### `nrz` — Receive files

```
nrz [OPTIONS] [PATH]

Options:
  -q, --quiet                Quiet (hide progress bar)
  -o, --overwrite            Overwrite existing files
  -b, --binary               Binary transfer mode
  -e, --escape               Escape control characters
  -d, --directory            Transfer directories
  -r, --recursive            Same as -d
  -f, --fork                 Background transfer
  -c, --compress <TYPE>      Compression: auto / yes / no [default: auto]
  -B, --bufsize <SIZE>       Buffer chunk size (1K~1G) [default: 10M]
  -t, --timeout <SECS>       Timeout per chunk (0=never) [default: 20]
  -V, --version              Print version
```

### `nrzsz` — Wrapper with filter

Wraps an SSH or other command to intercept trzsz/nrzsz transfer triggers:

```
nrzsz [OPTIONS] COMMAND [ARGS...]

Options:
  -r, --relay                Run as relay server
  -t, --tracelog             Enable trace log for debugging
  -d, --dragfile             Enable drag file upload
  -z, --zmodem               Enable zmodem lrzsz compatibility
  -o, --osc52                Enable clipboard integration
  -u, --upload <FILE>        Upload file(s) to remote (repeatable)
  -D, --download-dir <DIR>   Directory to save downloaded files
  -V, --version              Print version
```

**Examples:**

```bash
# Wrap SSH with nrzsz filter
nrzsz ssh user@host bash

# Upload specific files on connect
nrzsz -u localfile.txt ssh user@host bash

# Auto-download to a specific directory
nrzsz -D ~/incoming ssh user@host bash

# Enable zmodem support
nrzsz -z ssh user@host bash
```

## Configuration

Create `~/.nrzsz.conf` (JSON format):

```json
{
  "allowedAutoUploadDirs": ["/tmp", "/home/user/shared"],
  "defaultUploadPath": "/tmp",
  "defaultDownloadPath": "~/downloads",
  "dragFileUploadCommand": "kitty +kitten icat --transfer-mode=stream",
  "progressColorPair": "1;32"
}
```

Legacy `key = value` format is also supported:

```
allowedAutoUploadDirs = /tmp, /home/user/shared
defaultUploadPath = /tmp
defaultDownloadPath = ~/downloads
```

## Library API

Use `nrzsz` as a Rust library:

```rust
use nrzsz::{NrzszOptions, upload_files, download_files, start_relay};

// Upload files via TCP tunnel
let opts = NrzszOptions {
    path: Some("/remote/path".into()),
    quiet: false,
    ..Default::default()
};
upload_files(&opts, &["localfile.txt".into()])?;

// Download files
download_files(&opts, &["remotefile.txt".into()])?;

// Start relay
start_relay(&relay_opts)?;
```

## Compatibility

- `trzsz-go` v1.2.0+ (trz / tsz) — the primary compatibility target. The
  `::TRZSZ:` wire namespace and protocol V1–V4 are matched.
- `lrzsz` (rz / sz) — interop via the `-z` zmodem passthrough (implemented):
  a self-contained ZMODEM engine bridges raw streams to a legacy `rz`/`sz`.

## Feature highlights

- **trzsz-go v1.2.0 feature coverage** (see `AGENTS.md` for the full matrix):
  V1–V4 negotiation, zstd+zlib compression, V3 resume, directory transfer,
  fork, tunnel, relay, OSC52, drag upload (incl. iTerm2 `%begin`/`%end`),
  zenity dialogs, tracelog, tmux normal + `-CC` control mode,
  `~/.trzsz.conf`, progress color gradient, lrzsz-style CLI flags.
- Protocol V1–V4 with version negotiation and downgrade
- Zstd + Zlib compression with auto-detection
- V3 resume (prefix hash at 10MB steps)
- Directory transfer with recursive mode
- Background fork transfer
- OSC52 clipboard support
- Zmodem passthrough (`-z`): self-contained engine (no external `rz`/`sz`),
  detects `**\x18B0[01]hex`, sends/receives raw ZMODEM frames, progress +
  cancel. Verified end-to-end against real lrzsz binaries (upload + download).
- Drag file upload support (iTerm2 + Warp + bracketed-paste prefixes)
- Zenity file dialogs
- tmux normal mode + `-CC` control mode integration
- Cross-platform (macOS, Linux, Windows; the Windows build is fixed and the
  ConPTY path is implemented, but runtime on a Windows host is untested here)

## License

MIT
