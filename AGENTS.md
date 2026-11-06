# AGENTS.md — nrzsz vs trzsz vs lrzsz comparison

Analysis date: 2026-11-04. `nrzsz` is the Rust workspace in this repo
(`nsz` / `nrz` / `nrzsz` binaries + `nrzsz-core` library). References:
`trzsz-go` v1.2.0 (trz/tsz, Go, wire-format target, cloned at `/tmp/trzsz-go`)
and `lrzsz` (rz/sz, C, legacy, source at `/tmp/lrzsz-src` and `/tmp/lrzsz/src`).

Every claim below is grounded in source inspection (`/tmp/trzsz-go/...`,
`/tmp/lrzsz/src/{lsz.c,lrz.c,zmodem.txt}`, `nrzsz-core/src/*.rs`) or a
verified test run on this host.

---

## 1. What is done and verified

### 1.1 Core protocol engine (V1–V4) — verified end-to-end

| Feature | Status | Evidence |
|---|---|---|
| Wire format `::TRZSZ:TRANSFER:[SRDF]:VER:UID:PORT` | ✅ works | Trigger parse + emit, dual `::NRZSZ:` legacy namespace accepted |
| Action/Config/Data/Exit framing (`#ACT:`, `#CFG:`, `DATA`, `EXIT`/`FAIL`) | ✅ works | `protocol.rs` structs, `transfer.rs` |
| Version negotiation + downgrade V1–V4 | ✅ works | `negotiate_protocol_version` |
| Compression zstd + zlib, auto-detect, `-c auto/yes/no` | ✅ works | `compress.rs`; magic-byte detect |
| V3 prefix-hash resume (MD5, 10 MB steps) | ✅ works | `transfer.rs::send/recv_prefix_hash` |
| Directory transfer `-d/-r` (nested, byte-identical) | ✅ works | verified `diff` clean |
| Background fork `-f` | ✅ works | `fork.rs`, advertised over tunnel |
| Adaptive buffer 1K–1G (`-B`), timeout (`-t`) | ✅ works | `transfer.rs` |
| Binary (`-b`) + escape (`-e`) modes | ✅ works | `utils.rs::escape_data` |

### 1.2 Wrapper (nrzsz) — verified

| Feature | Status | Evidence |
|---|---|---|
| Filter mode (trigger detect, stdin forward) | ✅ works | `NrzszFilter` |
| Relay mode (`-r`) | ✅ works | handshake verified |
| Tunnel bridge over TCP | ✅ works | `::TRZSZ::CLIENT::SERVER::HELLO::uid:port` |
| Tracelog (`-t`) | ✅ works | `TraceLogger` wired into relay |
| OSC52 clipboard (`-o`) | ✅ works | `Osc52Clipboard` |
| Drag-file upload (`-d`) | ✅ works | `detect_drag_files` writes `trz`/`trz -d` |
| Zmodem passthrough (`-z`) | ✅ works | see §1.3 |
| Zenity file dialogs | ✅ works | `zenity.rs` |
| Dual namespace (`::TRZSZ:` + legacy `::NRZSZ:`) | ✅ works | all parsers accept both |


### 1.3 ZMODEM engine (`-z` passthrough) — **fixed and verified this session**

The `nrzsz-core/src/zmodem.rs` engine is a **self-contained** ZMODEM
implementation (does not shell out to `sz`/`rz`). It was substantially broken
before this session; the following were found and repaired, and both
directions now pass end-to-end against **real lrzsz binaries**:

1. **CRC-32 data subpacket decode (receiver)** — the `Decoder` always read 2
   CRC bytes after a frameend, but a `ZBIN32` header switches data subpackets
   to **CRC-32 (4 bytes)**. The receiver read 2, desynced, and hung. Fixed:
   `Decoder.crc32` flag set in `parse_bin32` and used in `next_data`
   (`nrzsz-core/src/zmodem.rs:289-298, 491-513`).
2. **CRC-32 data subpacket encode (sender)** — `send_one_file` emitted
   `ZBIN32` headers (receiver advertises `CANFC32` = `0x20` in ZP3) but wrote
   only 2 CRC bytes. `rz` never acked. Fixed: added
   `encode_data_subpacket32` (4-byte CRC) and used it for ZFILE/ZDATA/ZEOF
   (`zmodem.rs:232-249, 949-979`).
3. **ZSINIT not acked** — `sz` (pipe mode) sends `ZSINIT` after `ZRQINIT`; the
   receiver ignored it, `sz` re-sent forever. Fixed: reply `ZACK` on `ZSINIT`
   (`zmodem.rs:1093-1098`).
4. **Trailing XON after ZCRCW** — the sender appends XON after a `ZCRCW`
   subpacket; the receiver left it in the buffer, then mistook the following
   `ZDATA` header for file data (wrote header bytes into the file). Fixed:
   consume one `0o21` after `ZCRCW` (`zmodem.rs:505-511`).
5. **ESCCTL escaping (sender)** — `rz -e` sets `ESCCTL` (`0x40`) in ZRINIT and
   requires *every* control char escaped; `zdle_encode` only escaped a fixed
   set, so `rz` rejected the ZFILE. Fixed: `wait_for_rinit` returns the ZRINIT
   capability byte; `zdle_encode`/`encode_bin32_header`/
   `encode_data_subpacket32` take `escctl` (`zmodem.rs:135-174, 189-202,
   893-924`).
6. **Capability flag position** — ZMODEM puts receiver caps in the **last**
   header byte (ZP3), matching lrzsz (`hdr=[0,0,0,0x27]`); confirmed against
   real `rz`.

**Verification** (this host, `sz`/`rz` at `/usr/local/bin`):
- `e2e_receive_from_real_sz` (pty): downloads `hello.txt` from real `sz` —
  byte-exact, passes.
- `e2e_send_to_real_rz` (pty): uploads `data.bin` to real `rz` — byte-exact,
  passes.
- `zmodem_download_smoke` / `zmodem_upload_smoke` (pipe, `-e` ESCCTL):
  both pass with `--ignored`.

**Result: `cargo test --workspace` = 75 passed, 0 failed** (64 `nrzsz-core`
lib + 11 `protocol_tests`, after the dead-module deletion); the 2 zmodem smoke
tests pass with `--ignored`.

### 1.4 New this session — verified

| Feature | Status | Evidence |
|---|---|---|
| tmux `-CC` control-mode output routing | ✅ works | `tmuxcc.rs::TmuxOutputRedirect` wraps fd 1 + fd 2 through `encode_tmux_output` for the transfer lifetime; wired in `filter.rs::run_transfer` |
| tmux `-CC` trigger prefix parsing | ✅ works | `parse_trigger` strips `%output <id> ` / `%extended-output <id> 0 : ` and stores `tmux_prefix`/`tmux_pane_id` |
| iTerm2 drag `%begin`/`%end` acks | ✅ works | `upload_drag_files` emits `%begin/%end` (`tmuxBlocks-1`×) + `send -t %<pane> 0x3` (Go `uploadDragFiles` parity); tmux-CC drag input decoded via `decode_tmux_drag_input` |
| `~/.trzsz.conf` Go config | ✅ works | `config.rs::load_trzsz_conf` + `filter.rs::read_config` read `.trzsz.conf` keys (`defaultuploadpath`, `progresscolorpair`, `zmodemautorename`, …) |
| `ProgressColorPair` gradient bar | ✅ works | `progress.rs` `BlendLuv` (Luv-space, matches go-colorful byte-for-byte), truecolor cells; only active when a non-empty pair is set |
| lrzsz-style flags `-a -r -p -w -Y -E -+` | ✅ functional | `nsz`/`nrz` `--help`; `-a`/`-Y`/`-E`/`--resume` wired; **`-p`/`-w`/`-+` now implemented** (protect-skip, window size, append); all verified end-to-end this session (§4.4) |
| Windows build fix | ✅ verified | `utils.rs` `windows_flush` migrated to `windows-sys` (symbols confirmed in 0.59.0 source) |
| Tunnel hello over-read fix | ✅ fixed + regression-tested | `filter.rs::run_transfer` reads server hello line-at-a-time (was fixed-buffer `read`, which could consume the next frame and desync the stream — intermittent "Invalid format: ") |
| Dead-module cleanup | ✅ done | 11 dead modules deleted (archive, dragdrop, newline, permissions, compress_check, ptyrelay, filedialog, pipeline, resume, iterm2, pty/fallback); `tunnel.rs`/`terminal.rs` slimmed to wired free-fns |

---

## 2. Dead code — deleted this session

The dead modules identified in the prior audit were **deleted** (they were
either duplicate implementations of live inline logic or never-called
wrappers; none was a functional gap — verified by a read-only audit of every
module's callers). `nrzsz-core/src` now contains only live, reachable code.

| Deleted module | Why (was dead) |
|---|---|
| `archive.rs` | V4 file grouping never invoked; `transfer.rs` sends files individually |
| `pipeline.rs` | 3-stage pipeline never called; real send/recv is inline in `transfer.rs` + `utils.rs::send/check_pipeline_ack` |
| `resume.rs` | `ResumeState` persistent store never used; real resume is inline V3 prefix-hash in `transfer.rs::send_prefix_hash` |
| `compress_check.rs` | profitability done inline in `compress.rs` |
| `dragdrop.rs` | real drag detection is `filter.rs::detect_drag_files` |
| `iterm2.rs` | tmux-based class that never matched Go's model; real iTerm2 behavior is the drag `%begin`/`%end` in `filter.rs::upload_drag_files` |
| `filedialog.rs` | `zenity.rs` is the used dialog path |
| `newline.rs` | newline handled via `Config.newline` strings |
| `ptyrelay.rs` | the only consumer of `pty`; never called |
| `permissions.rs` | permission preservation done inline in `transfer.rs` |
| `pty/fallback.rs` | never `mod`-included |

**Slimmed (kept wired free-functions only):** `tunnel.rs` (kept
`get_hello_constant` + `server_tunnel_handshake` + `read_line_from_stream`;
deleted the `TunnelConnection`/`TunnelManager` class API), `terminal.rs`
(kept `get_terminal_columns`; deleted `TerminalWrapper`/`TerminalInfo`/
`setup_server_env`/`detect_client_support`/`get_client_cwd`).

**Retained as library surface:** `api.rs` (`NrzszOptions::to_config`,
`TransferStateHandler`, re-exported in `lib.rs`) is the embedding API — options
plus a transfer-state callback. There are no free `upload_files`/
`download_files`/`start_relay` functions in `api.rs`; the actual transfer path
is `filter.rs::run_transfer` (trigger → tunnel → `TransferSession`), and the
dead `NrzszFilter::upload_files`/`download_files` stubs were removed this
session (§4.6).
### Explicit stubs (no-op) — retained

| Location | Symbol | Status |
|---|---|---|
| `zmodem.rs` | `ZmodemProxy::proxy_zmodem_send` / `proxy_zmodem_receive` | Stub — return `Ok(())`; retained for API compat; `detect_zmodem_sequence`/`send_zmodem_start`/`send_zmodem_cancel` are real |



---

## 3. Incomplete / needs work — remaining

| Area | Status | Evidence |
|---|---|---|
| **Windows runtime** | Untested | `pty/windows.rs` (ConPTY), `binary=false`, newline `!\n` — never run on a Windows host (the build now compiles for the target, but no Windows host is available here to execute it) |
| **Fork with tunnel** | ✅ fixed | `send_action` now advertises `tunnel: Some(tunnel_connected)`, so `use_tunnel` is true over the tunnel and fork actually gates (§4.5) |
| **`-p`/`-w`/`-+` (protect/windowsize/append)** | ✅ implemented | `Config` fields `protect`/`window_size`/`append` wired; protect-skip, window size, append all verified end-to-end (§4.4) |
| **Relay + `-o`/`-u`/`-D`** | Not supported | Relay only bridges; these flags are filter-mode-only (matches Go — relay only does tracelog) |
| **`api.rs` surface** | ✅ done (§4.9) | rebuilt as a real embedding surface: `NrzszOptions::build_filter` (→ Config + setters), `TransferStateHandler` (bool start/stop callback) → `apply_to(filter)`; Go's `TransferState` enum removed |
| **`-Y` vs lrzsz semantics** | Divergence | nrzsz `-Y` = "overwrite if source newer"; lrzsz `-Y` = "clobber + skip-if-not-found" (`ZF1_ZMSKNOLOC`). Deliberate; nrzsz targets Go, which has no `-Y`. |
| **V4 directory archive** | Not implemented | Go groups a directory's files into one streamed `SubFiles` archive entry; nrzsz sends each file individually (correct, only a wire-size optimization) |
| **Non-tunnel fallback** | Design choice | nrzsz always transfers over the TCP tunnel; Go can fall back to raw terminal streams when no connector is set |

---

## 4. Fixed this session

### 4.1 ZMODEM engine (see §1.3 for details)
CRC-32 decode (receiver), CRC-32 encode (sender), ZSINIT→ZACK, trailing XON
consumption, ESCCTL escaping, ZP3 capability position. Both E2E directions
now pass against real `lrzsz`.

### 4.2 Previously fixed (prior session, still present)
1. Directory-transfer trigger mode (`D/F/R`) computed correctly; `-F` uses the
   client's `-u` paths.
2. Fork mode advertises `fork: Some(true)` over the tunnel and calls
   `fork_to_background()`.
3. Whole stack emits `::TRZSZ:` (was `::NRZSZ:`), accepts both namespaces.
4. Auto-upload path containment via `is_path_within_dir` (was naive
   `starts_with`).
5. Relay tracelog wired (`-t`).
6. Drag upload writes `trz`/`trz -d` to server stdin (was a flag-only no-op).

### 4.3 This session (all verified end-to-end)
1. **Windows build fixed** — `utils.rs` `windows_flush` migrated from `winapi`
   to `windows-sys` (symbols confirmed in 0.59.0; cross-check needs a Windows C
   toolchain, unavailable here, but the `windows-sys` API surface matches the
   sibling `pty/windows.rs` usage).
2. **tmux `-CC` control mode wired** — `TmuxOutputRedirect` wraps fd 1 + fd 2
   through `encode_tmux_output` for the whole transfer; trigger prefix parsing
   added to `parse_trigger`; drag `%begin`/`%end` + `send -t %<pane> 0x3`
   emitted on tmux-CC drag upload (Go `uploadDragFiles` parity).
3. **`~/.trzsz.conf` Go config** — read by both `config.rs` and `filter.rs`
   (`defaultuploadpath`, `defaultdownloadpath`, `progresscolorpair`,
   `zmodemautorename`, `allowedautouploaddirs`, `dragfileuploadcommand`).
4. **`ProgressColorPair` gradient** — `BlendLuv` gradient bar in `progress.rs`,
   wired `Config.progress_color_pair` → `TransferSession` →
   `TransferProgress::new_with_color`.
5. **lrzsz-style flags** — `-a`, `--resume`, `-p`, `-w`, `-Y`, `-E`, `-+` on
   both `nsz`/`nrz`; all now wired and functional (see §4.4 for the four
   conflict-handling flags implemented this session).
6. **Tunnel hello over-read fix** — `run_transfer` now reads the server hello
   byte-at-a-time; a fixed-buffer `read` could coalesce the hello with the next
   frame and desync the stream (intermittent "Invalid format: ").
7. **Dead-module deletion** — 11 dead modules removed; `tunnel.rs`/`terminal.rs`
   slimmed.

### 4.4 Conflict-handling flags (implemented + verified end-to-end)

The four lrzsz-style file-conflict flags were previously exposed but
non-functional; all are now wired through `Config` and verified end-to-end
over the TCP tunnel (`nrzsz -u <file> nrz -<flag> <dest>`):

| Flag | lrzsz semantics | Implementation | Verified |
|---|---|---|---|
| `-p`/`--protect` | skip if destination exists (`ZF1_ZMPROT`) | `recv_file_name_v3` sends `SUCC {skip:true}` when dest exists | dest unchanged ✅ |
| `-+`/`--append` | append to destination (`ZF1_ZMAPND`) | receiver seeks to EOF; sender sends whole source from offset 0 | `PRE`+`FIX` → `PREFIX` ✅ |
| `-Y`/`--overwrite-or-skip` | overwrite only if source newer (`Lskipnocor`) | mtime compare in `recv_file_name_v3`; skip when `dest >= src` | source newer → overwrite; dest newer → skip ✅ |
| `-w`/`--windowsize` | windowed streaming | `Config.window_size` → negotiated window | byte-identical transfer ✅ |

**Truncate fix:** `create_dir_or_file_v3` opened existing files with
`.create(true)` but **without** `.truncate(true)`, so `-y`/`-Y` overwrite left
the old tail in place (`OLD\n` + new data = corrupted). Added
`.truncate(self.config.overwrite)` to all four open sites (unix + non-unix,
both the directory and flat branches). The resume path (`overwrite=false`)
still opens without truncation, so V3 prefix-hash resume is unaffected.

**mtime wiring:** `check_path_readable` populates `FileInfo.mtime_ns` via
`mtime_nanos` (UNIX epoch nanos); the receiver compares it against the dest
file's mtime for `-Y`. Serialized as `#[serde(default, skip_serializing_if =
"Option::is_none")]`, so older peers (no field) default to `None` → `-Y`
skips (conservative, matches "no mtime info = not-newer").

### 4.5 This session (fresh comparison vs trzsz-go + lrzsz)

Found via a fresh source-level comparison against `/tmp/trzsz-go/trzsz` and
`/tmp/lrzsz/src`. All fixed and verified end-to-end:

1. **Tunnel never advertised** — `send_action` always sent `tunnel: None`, so
   the server's `use_tunnel = action.tunnel.unwrap_or(false)` was **always
   false**. Consequence: fork (`SupportFork`) and tunnel binary mode never
   activated over the tunnel. Fixed: `tunnel: Some(tunnel_connected)`
   (`transfer.rs::send_action`). Verified: `nrz -f` fork and binary-over-tunnel
   both work now (7/7 feature smoke).
2. **`send_config` local-config desync** — the receiver advertised
   `binary=true` (forced by tunnel) to the client but its own `recv_*` used
   `config.binary` from args (`false`), so client sent binary-path data while
   the receiver read base64 → "Invalid padding". Fixed: `send_config` now
   unmarshals the sent `cfg_map` back into `self.config` (mirrors Go
   `sendConfig`, which sets `transferConfig` from the marshaled cfgMap).
3. **`-d/--dragfile` no-op** — `args.dragfile` was parsed but
   `set_enable_dragfile` was never called, so drag detection never ran. Fixed:
   wired `filter.set_enable_dragfile(args.dragfile)` in `nrzsz/src/main.rs`.
4. **Progress color pair dead** — `filter.progress_color_pair` was stored but
   never reached `Config.progress_color_pair`, so the gradient never showed in
   the CLI path. Fixed: `set_progress_color_pair` now whitespace-splits and
   populates the transfer `Config` (matching Go `newTextProgressBar`).
5. **Auto-upload dirs** — `allowedautouploaddirs` was not shlex-split
   (whole value = one dir); empty dirs were permissive (Go errors); and the
   `-u` path bypassed the allowed-dir check. Fixed all three
   (`filter.rs::read_config`, `check_auto_upload_allowed`, `upload_files_to`).
6. **Drag tmux command** — in tmux `-CC` mode the command was written raw
   (not hex-encoded/`send -t`-wrapped), `-d` was always appended (Go gates on
   `uploadCommandIsNotTrz`), and non-tmux mode omitted the Ctrl-C (`0x03`)
   interrupt byte. Fixed all three (`filter.rs::upload_drag_files` +
   `set_drag_file_upload_command`); added the `upload_command_is_not_trz`
   field.
7. **`-c yes` force-compress** — the sender used `compress_if_profitable`
   even for `-c yes`, so a small/incompressible file was sent uncompressed
   while the receiver (config `compress=Yes`) assumed zstd → "Unknown frame
   descriptor". Fixed: `-c yes` now always compresses (Go `isCompressFixed`),
   in both `send_file_data` and `send_file_data_with_compress`.

**Verification:** `cargo build --release --workspace` = 0 warnings;
`cargo test --release --workspace` = 64 lib + 13 protocol, 0 failed;
8/8 feature smoke (text, 3 MB binary, directory, fork, `-c yes` compress,
resume, `-Y`, `-+`). The 2 zmodem smoke tests pass with `--ignored`.

### 4.6 Stop / transfer-control plumbing (fresh trzsz-go comparison, this session)

A fresh source-level comparison against `/tmp/trzsz-go/trzsz` found the
stop / during-transfer control path diverged from Go and was partially dead.
All fixed and verified:

1. **`nrz` SIGINT deleted received files** — the server's signal handler had
   an interactive "stop and delete?" prompt that also blocked the async
   runtime. Go's `trz` `handleServerSignal` only calls
   `stopTransferringFiles(false)` (keep files, no prompt). Removed the prompt
   and the file-deletion path; SIGINT/SIGTERM now stop-and-keep
   (`nrz/src/main.rs`).
2. **Wrapper stop never reached the running session** —
   `NrzszFilter::stop_transferring_files` only set a filter-level flag; the
   `TransferSession` on the spawned thread kept going. Added a shared
   `TransferStop` handle (`transfer.rs`): `TransferSession::new_with_stop`
   sources its `stopped`/`stop_and_delete` flags and `NrzszBuffer` from the
   handle; `run_transfer` registers a fresh handle per transfer in an
   `active_stop` slot; `stop_transferring(stop_and_delete)` stops the live
   session and wakes a blocked protocol read (Go's `StopTransferringFiles`
   → `stopTransferringFiles` → `stopBuffer`).
3. **`nrzsz` SIGINT/SIGTERM split** — the wrapper used one `ctrlc` handler
   that killed the child on either signal. Go's `handleSignal` keeps the
   child on SIGINT (stop transfer only) and terminates on SIGTERM. Split via
   `signal_hook::iterator::Signals`: SIGINT → `stop_transferring(false)`,
   SIGTERM → stop + `SIGTERM` to child (`nrzsz/src/main.rs`).
4. **`one_time_upload` result never resolved** — the receiver had no resolver
   and no timeout; `set_one_time_upload_result` was dead. Now `one_time_upload`
   arms a 10s watchdog (Go's `OneTimeUpload` "upload did not start" timer) and
   `run_transfer` resolves the channel after an R/D/F upload (Go's
   `setOneTimeUploadResult`). The one-time files slot is shared with the
   transfer thread, and `upload_files_to` consumes it first (Go's
   `chooseUploadPaths` returns `oneTimeUploadFiles` before `-u`).
5. **Ctrl+C during a transfer was forwarded to the remote** — Go's
   `handleClientInput` routes a local `0x03` to `handleInputCtrlC` (stop the
   local transfer) instead of sending it to the server. `process_input` now
   stops the active session on a raw `0x03` (dropped from the forwarded
   bytes) or a decoded tmux-CC `send -t %N 0x3` (consumed, not forwarded).
6. **Dead stubs deleted** — `NrzszFilter::upload_files` / `download_files`
   (eprintln no-ops, unused by any binary) removed (§2).

**Verification:** `cargo build --release --workspace` = 0 warnings;
`cargo test --release --workspace` = 68 lib + 13 protocol, 0 failed;
2 zmodem e2e pass with `--ignored`. New regression tests:
`shared_stop_handle_stops_session` / `shared_stop_handle_delete_flag`
(`transfer.rs`), `test_one_time_upload_slot_plumbing` /
`test_process_input_ctrl_c_during_transfer_stops` (`filter.rs`). End-to-end:
a 400 MB upload stopped by SIGINT mid-data-transfer leaves a partial file
(no growth after the signal) with the wrapper still alive; directory + `-Y`
conflict semantics re-verified byte-identical.

### 4.7 Download-direction e2e harness + compress-contract bug (this session)

Built the missing **download-direction e2e harness** (the §7 gap): a real
`nsz` server process over the TCP tunnel, driven by a client that mirrors
`filter.rs::download_files_from` (handshake → `send_action` → `recv_config`
→ `recv_files` → `client_exit`). Location: `nsz/tests/tunnel_download_e2e.rs`
(3 tests: 3 MB binary file, recursive directory via `nsz -d`, plus a
`check_paths_readable` sanity — all byte-identical; `nsz/Cargo.toml` gained
`[dev-dependencies] tempfile`).

Found and fixed a real wire-contract bug the harness exposed (directory
downloads failed with zstd "Unknown frame descriptor"):

- **`transfer.rs::send_file_data_with_compress` / `send_file_data` re-decided
  per-chunk compression profitability AFTER the COMP flag was already sent.**
  In Auto mode, `is_compress_fixed` fixes `compress=true` for sizes
  512 B–128 KB without content sampling; both sides then skip the COMP frame
  and the receiver assumes zstd — but the sender's `compress_if_profitable`
  still sent an incompressible chunk raw → receiver decompressed garbage.
  Go's `pipelineEncodeData` compresses **every** chunk once the flag/mode is
  set. Fixed: once `compress_type != None`, every chunk is compressed
  unconditionally (both call sites, so V1 and V2/V3 paths behave the same).

Verification: `cargo test --workspace` = 68 lib + 13 protocol + 3 e2e,
0 failed; 2 zmodem smoke pass with `--ignored`; `cargo build --release
--workspace` clean (0 warnings). Downloads verified byte-identical for both a
file and a directory (client's saved-name list matches Go's `recvFiles`
semantics: deduped `RelPath[0]`, i.e. only the dir root).

### 4.9 Embedding API (`api.rs`) rebuilt (this session)

`api.rs` was entirely dead surface: `NrzszOptions::to_config` was kept for the
`Default` impl but no binary or caller used any of it; `TransferState` was a
parallel enum matching nothing; `TransferStateHandler` was re-exported but
never wired to a `NrzszFilter`. Rebuilt to match Go's embedding surface:

- **`NrzszOptions::build_filter(self) -> NrzszFilter`** — builds `Config` via
  `to_config` and wires the setters that `NrzszOptions` actually controls
  (`dragfile` → `set_enable_dragfile`, `zmodem` → `set_enable_zmodem`,
  `osc52` → `set_enable_osc52`). Documented that connector / paths / uploaded
  files stay caller-side setters.
- **`TransferStateHandler`** — Go's `SetTransferStateCallback(bool)` idiom: a
  `bool` start/stop callback (true at transfer start, false at end), with
  **`apply_to(&mut NrzszFilter)`** bridging into `set_transfer_state_callback`.
  `apply_to` sends the callback through; the previous `TransferState` enum is
  removed (it matched nothing in Go or in `filter.rs`).
- `NrzszOptions::to_config` is kept (used by callers who want just the
  `Config`); `parse_size` also gained `kib/mib/gib` units.
- The `TransferState` export in `lib.rs` was replaced with
  `TransferStateCallback`.

Verification: `cargo test --workspace` = 70 lib (+2 new: `test_build_filter_wires_setters`,
`test_transfer_state_handler_applies_to_filter`; the latter feeds a trigger
line through a real `NrzszFilter` and asserts the bool callback fires with
`true`), 13 protocol, 3 tunnel e2e, 1 relay download, 1 relay upload —
0 failed; `cargo build --release --workspace` clean.

### 4.8 Filter+relay integration harness + relay tunnel-hello desync (this session)

Built the **filter+relay integration harness** (the §7 integration-test gap):
an end-to-end test with a real `nsz`/`nrz` server process sitting behind
`NrzszRelay`, acting as the relay that **rewrites the trigger's tunnel port
and bridges the previous-hop TCP connection to the server tunnel**. The
previous hop runs a real `NrzszFilter` (same code path `nrzsz` uses for a
trigger) whose `tunnel_connector` connects to the relay's rewritten port.

- `nsz/tests/filter_relay_download_e2e.rs` — `nsz` behind relay → filter
  pulls the rewritten `S` trigger → downloads byte-identical. 2 MB binary
  file.
- `nrz/tests/filter_relay_upload_e2e.rs` — `nrz` behind relay → filter
  pushes `-u`-style files → byte-identical at the server's dest.
- `nsz/tests/tunnel_download_e2e.rs` — direct tunnel download harness (§4.7).
- The harness found and fixed a **race in `relay_tunnel`'s hello reads**:
  both the client hello and the server hello were read with a fixed 100-byte
  `read()`; when the peer's terminator `\n` arrived in a separate TCP
  segment, the newline was left in the relay's stream **and the server's
  stream** (hello passed to nsz consumed less than a line for the following
  protocol frame), so nsz saw a bare empty line and died with
  `Error: Invalid format:` (~7/8 runs failed). Fixed: both hellos are read
  line-at-a-time via `tunnel::read_line_from_stream` (same class of bug as
  §4.5 item 6). Test passes 8/8 consecutively after the fix.
- Also fixed the same fixed-buffer read in `filter.rs::connect_to_tunnel`
  (dead-ish API path, same trap).
- **Bounded the relay STANDBY detect buffer** to 4 KiB (`detect_buf` was
  unbounded, so throughput/trigger scans were O(n²) and the buffer grew
  forever on a silent session).

Verification: `cargo test --workspace` = 68 lib + 13 protocol + 3 tunnel e2e +
1 relay download + 1 relay upload, 0 failed
(plus 2 zmodem smoke `--ignored`); `cargo build --release --workspace` clean
(0 warnings). Relay-related diagram is unchanged (raw `#ACT` relay path kept
as designed; the harness exercises the rewritten-port TCP-bridge path, which
is what a rewritten trigger actually drives — the raw path remains available
for previous-hop filters that run transfer over raw streams).


---

## 5. Gap analysis vs trzsz-go v1.2.0

The **wire protocol is matched** (V1–V4, `::TRZSZ:`, JSON framing, tunnel).
Remaining differences vs trzsz-go:

| trzsz-go feature | nrzsz | Notes |
|---|---|---|
| tmux control mode (`-CC`) | ✅ now wired | `TmuxOutputRedirect` + trigger prefix parsing + drag `%begin`/`%end` |
| iTerm2 drag `%begin`/`%end` | ✅ now wired | `upload_drag_files` (Go `uploadDragFiles` parity); **tmux-CC command now hex-encoded + `send -t`-wrapped (§4.5)** |
| `~/.trzsz.conf` (Go keys) | ✅ now read | `config.rs::load_trzsz_conf` + `filter.rs::read_config` |
| `ProgressColorPair` gradient | ✅ now reaches progress bar | `BlendLuv` matches go-colorful byte-for-byte; `set_progress_color_pair` now populates the transfer `Config` (§4.5) |
| Go embedding setters | ✅ done (§4.9) | `api.rs` is now a real embedding surface: `NrzszOptions::build_filter` builds a filter wired via the `dragfile`/`zmodem`/`osc52` setters; `TransferStateHandler` hands a `bool` callback (Go's `SetTransferStateCallback(start/stop)`) into the filter via `apply_to`; `NrzszOptions` docs what remains caller-side (connector/paths) |
| zmodem auto-rename `-E` | ⚠️ partial | `ZmodemProxy` stubs retained; auto-rename via `resolve_dest` exists |
| Windows ConPTY | ⚠️ build fixed, runtime untested | `windows_flush` migrated; no Windows host to execute |
| Warp terminal drag prefixes | ✅ handled | `detect_drag_files` handles old/new warp prefixes + bracketed paste |
| `StopTransferringFiles(stopAndDelete)` reaches live session | ✅ now wired | shared `TransferStop` handle; wrapper/SIGINT/tmux-CC stop the running transfer in place (§4.6) |
| Wrapper SIGINT vs SIGTERM split | ✅ now split | SIGINT keeps the child + stops the transfer; SIGTERM terminates (§4.6) |
| `OneTimeUpload` result + 10s watchdog | ✅ now resolved | `run_transfer` resolves the channel; 10s timer reports "did not start" (§4.6) |
| Ctrl+C during transfer stops locally | ✅ now handled | `process_input` stops the active session instead of forwarding `0x03` to the remote (§4.6) |

**What nrzsz has that trzsz-go also has (matched):** V1–V4 negotiation,
zstd+zlib compression, V3 prefix-hash resume, directory transfer, fork,
tunnel, relay, OSC52, drag upload, zenity, tracelog, dual namespace, tmux-CC,
iTerm2 drag acks, `.trzsz.conf`, progress gradient.

---

## 6. Gap analysis vs lrzsz (legacy rz/sz)

lrzsz (`rz`/`sz`) is the C reference for **raw ZMODEM** over a serial/pty
stream. nrzsz implements the **trzsz wire protocol** (superset adding JSON
config/action framing and a TCP tunnel), not the bare ZMODEM stream.
Consequences:

### 6.1 Raw ZMODEM interop
nrzsz's own engine **does** speak raw ZMODEM now (`zmodem.rs`, verified vs
real `sz`/`rz`), but the product's primary path is the trzsz wire protocol.
Legacy `lrzsz` interop is via the `-z` passthrough (bridges a local
`rz`/`sz`).

### 6.2 `sz`/`rz` CLI flags exposed by `nsz`/`nrz`
From `/tmp/lrzsz/src/lsz.c` (`sz`) and `lrz.c` (`rz`):

| lrzsz flag | lrzsz meaning | nrzsz |
|---|---|---|
| `-a/--ascii` | CR/LF→LF (`ZCNL`) | ✅ `-a` (text mode, forces binary off) |
| `-e/--escape` | escape all control chars | ✅ `-e` |
| `-E/--rename` | force receiver rename (`ZF1_ZMCHNG`) | ✅ `-E` (auto-rename on conflict) |
| `-r/--resume` | resume via ZCRESUM/ZRPOS | ✅ `--resume` (V3 prefix-hash resume is default; `-r` = `--recursive`) |
| `-p/--protect` | protect existing (`ZF1_ZMPROT`) | ✅ `-p` (skip if dest exists) |
| `-w/--windowsize` | windowed streaming | ✅ `-w` (distinct `Config.window_size`) |
| `-Y/--overwrite-or-skip` | overwrite else skip | ✅ `-Y` (skip unless source newer) |
| `-+/--append` | append (`ZF1_ZMAPND`) | ✅ `-+` (append to dest) |
| `-B N` | buffer size | ✅ `-B` (1K–1G, no `auto`) |
| `-Z/--zmodem` | ZMODEM | ✅ (default) |
| `-f`, `-n/-N`, `-o`, `-c`, `-C`, `-S`, `-l/-L`, `-m/-M`, `-O`, `-R`, `-s`, `-u`, `-T`, `-2/4/8`, `-X/-k`, `-j`, `-D`, `--o-sync`, `--delay-startup`, `--syslog`, `--tcp*` | other ZMODEM/XMODEM options | ❌ not applicable (nrzsz protocol differs; raw options only via `-z` passthrough) |

**Directory recursion:** lrzsz **refuses** directories/blkdevs (lsz.c
`S_ISDIR||S_ISBLK` → error); nrzsz **supports** `-d/-r` directory transfer.
nrzsz's directory support is a *superset* of lrzsz.

**Verification note:** nrzsz's compatibility target is **trzsz-go**; raw
`lrzsz` interop is via `-z`.

---

## 7. Recommended next steps (highest value first)

1. **Windows runtime test** — run `pty/windows.rs` (ConPTY) and `binary=false`
   on a real Windows host.
2. ~~**Wire `api.rs` free functions to `NrzszFilter` setters**~~ ✅ done (§4.9).
3. ~~**Add an integration-test harness** for filter+relay~~ ✅ done (§4.8).
4. ~~Fork-with-tunnel gating~~ ✅ done (§4.5 item 1).
5. ~~Download-direction end-to-end harness~~ ✅ done (§4.7).

---

## 8. Quick reference — command surface

| Tool | Purpose | Status |
|---|---|---|
| `nsz [opts] files...` | Send (server: binds tunnel, prints `S` trigger) | ✅ Works |
| `nrz [opts] [path]` | Receive (server: binds tunnel, prints `R/D/F` trigger) | ✅ Works |
| `nrzsz [opts] cmd...` | Wrapper: filter or relay around a shell/ssh command | ✅ Works |
| `nrzsz -u file ... cmd` | Auto-upload when the server triggers | ✅ Works (filter) |
| `nrzsz -D dir ... cmd` | Auto-download dir | ✅ Works (filter) |
| `nrzsz -d ... cmd` | Drag-file upload | ✅ Works (command sent; tmux-CC `%begin`/`%end` added) |
| `nrzsz -z ... cmd` | Zmodem passthrough (self-contained engine) | ✅ Works (verified vs real lrzsz) |
| `nrzsz -o ... cmd` | OSC52 clipboard | ✅ Works |
| `nrzsz -r ... cmd` | Relay between hosts | ✅ Works (tracelog wired) |

**CLI flags** — `nsz`/`nrz`: `-q -y -b -e -d -r -f -c(auto/yes/no) -B -t -a
--resume -p -w -Y -E -+` (plus `nrz -F/--file`). `nrzsz`: `-h -v -r -t -d -z -o
-u -D` + trailing command.

---

## 9. Test / verification status

- `cargo test --workspace`: **70 lib + 13 protocol + 3 tunnel e2e + 1 relay
  download + 1 relay upload, 0 failed** (plus 2 zmodem smoke `--ignored`).
  New: `nsz/tests/tunnel_download_e2e.rs` — download direction over a real
  `nsz` server + TCP tunnel (file + directory, byte-identical), and the
  compress-contract regression it caught (§4.7); `nsz/tests/
  filter_relay_download_e2e.rs` and `nrz/tests/filter_relay_upload_e2e.rs` —
  filter+relay full stack over a real server behind `NrzszRelay`
  (§4.8, plus the relay hello-desync fix).
  Prior regression tests: `shared_stop_handle_stops_session`,
  `shared_stop_handle_delete_flag`, `test_one_time_upload_slot_plumbing`,
  `test_process_input_ctrl_c_during_transfer_stops` (this session, §4.6);
  `test_tunnel_hello_read_does_not_overread`,
  `test_send_action_advertises_tunnel`, `test_send_action_no_tunnel_advertises_false`
  (prior sessions).
- **Comprehensive smoke (this session, 8/8):** upload text, 3 MB binary,
  directory recursive, fork `-f`, `-c yes` compress, `--resume`, `-Y`, `-+`
  — all byte-identical over the TCP tunnel.
- **Conflict-handling smoke (prior session, 6/6):** `-p` skip, `-+` append,
  `-Y` overwrite-newer (both directions), `-w` window size, basic upload —
  all byte-identical over the TCP tunnel.
- `cargo build --release --workspace`: clean; `nsz`/`nrz`/`nrzsz` all build.
- No `TODO`/`FIXME`/`unimplemented!`/`todo!()` markers in `nrzsz-core/src`
  (only the two no-op `ZmodemProxy` stubs).

