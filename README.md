# nlzsz ( nrz / nsz ) - similar to ( rz / sz ) and compatible with tmux

A Rust implementation of file transfer, similar to `lrzsz` ( rz / sz ), and compatible with `tmux`.

## Features

- Support **tmux**, including tmux normal mode, and tmux command mode.
- Support **transfer directories**, `nrz -d` to upload directories, `nsz -d xxx` to download xxx directories.
- Support **breakpoint resume**, `nrz -y` or `nsz -y xxx` overwrite existing files will auto resume from breakpoint.
- Support **progress bar**, shows the current transferring file name, progress, size, speed, remaining time, etc.
- Better **interactive experience**, shows the transfer results or errors friendly, `ctrl + c` to stop gracefully.
- Support **zstd compression**, automatically compress data during transfer for better performance.
- Support **protocol version negotiation**, compatible with v1-v4 protocols.
- Support **signal handling**, graceful shutdown on SIGTERM/SIGINT.
- Support **pipeline transfer**, parallel data processing for better performance.
- Support **Windows platform**, works on Windows servers and clients.
- Support **client filter**, automatic detection of transfer triggers.

## Installation

### Build from source

```bash
cargo build --release
```

The binaries will be located at:
- `target/release/nsz` (similar to `sz`)
- `target/release/nrz` (similar to `rz`)

### Install with cargo

```bash
cargo install --path nsz
cargo install --path nrz
```

## Usage

### `nrz` upload files to the remote server

```
usage: nrz [-h] [-v] [-q] [-y] [-b] [-e] [-d] [-f] [-c auto] [-B N] [-t N] [path]

Receive file(s), similar to rz and compatible with tmux.

positional arguments:
  path               path to save file(s). (default: current directory)

optional arguments:
  -h, --help         show this help message and exit
  -v, --version      show program's version number and exit
  -q, --quiet        quiet (hide progress bar)
  -y, --overwrite    yes, overwrite existing file(s)
  -b, --binary       binary transfer mode, faster for binary files
  -e, --escape       escape all known control characters
  -d, --directory    transfer directories and files
  -r, --recursive    transfer directories and files, same as -d
  -f, --fork         background transfer (implies -q)
  -c, --compress     compression type (auto/yes/no)
  -B N, --bufsize N  max buffer chunk size (1K<=N<=1G). (default: 10M)
  -t N, --timeout N  timeout ( N seconds ) for each buffer chunk.
                     N <= 0 means never timeout. (default: 20)
```

### `nsz` download files from the remote server

```
usage: nsz [-h] [-v] [-q] [-y] [-b] [-e] [-d] [-f] [-c auto] [-B N] [-t N] file [file ...]

Send file(s), similar to sz and compatible with tmux.

positional arguments:
  file               file(s) to be sent

optional arguments:
  -h, --help         show this help message and exit
  -v, --version      show program's version number and exit
  -q, --quiet        quiet (hide progress bar)
  -y, --overwrite    yes, overwrite existing file(s)
  -b, --binary       binary transfer mode, faster for binary files
  -e, --escape       escape all known control characters
  -d, --directory    transfer directories and files
  -r, --recursive    transfer directories and files, same as -d
  -f, --fork         background transfer (implies -q)
  -c, --compress     compression type (auto/yes/no)
  -B N, --bufsize N  max buffer chunk size (1K<=N<=1G). (default: 10M)
  -t N, --timeout N  timeout ( N seconds ) for each buffer chunk.
                     N <= 0 means never timeout. (default: 20)
```

## Compatibility

nlzsz is compatible with similar file transfer tools. You can use `nsz`/`nrz` on one side and other compatible tools on the other side.

## License

MIT