//! Self-contained ZMODEM (lrzsz rz/sz compatible) implementation.
//!
//! This module implements the ZMODEM wire protocol directly (no external
//! `rz`/`sz` binary), based on the ZMODEM Inter Application File Transfer
//! Protocol (Chuck Forsberg) and the lrzsz `zm.c`/`zmodem.h` reference.
//!
//! Roles:
//! - **Sender** (`sz`): uploads local files to a remote `rz` receiver.
//! - **Receiver** (`rz`): downloads files from a remote `sz` sender.
//!
//! The engine is fed server output bytes (via a shared buffer) and writes
//! protocol responses to `server_in`, so it plugs into the `NrzszFilter`
//! output loop.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

// ---------------------------------------------------------------------------
// ZMODEM protocol constants (zmodem.h)
// ---------------------------------------------------------------------------


// Protocol constants. Some are not all dispatched by the current
// sender/receiver (e.g. ZSINIT, ZCOMMAND) but are part of the wire format.
#[allow(dead_code)]
mod _zmodem_consts {
    pub const ZPAD: u8 = b'*';
    pub const ZDLE: u8 = 0o30; // 0x18, CAN
    pub const ZBIN: u8 = b'A';
    pub const ZHEX: u8 = b'B';
    pub const ZBIN32: u8 = b'C';

    // Frame types
    pub const ZRQINIT: u8 = 0;
    pub const ZRINIT: u8 = 1;
    pub const ZSINIT: u8 = 2;
    pub const ZACK: u8 = 3;
    pub const ZFILE: u8 = 4;
    pub const ZSKIP: u8 = 5;
    pub const ZNAK: u8 = 6;
    pub const ZABORT: u8 = 7;
    pub const ZFIN: u8 = 8;
    pub const ZRPOS: u8 = 9;
    pub const ZDATA: u8 = 10;
    pub const ZEOF: u8 = 11;
    pub const ZFERR: u8 = 12;
    pub const ZCRC: u8 = 13;
    pub const ZCHALLENGE: u8 = 14;
    pub const ZCOMPL: u8 = 15;
    pub const ZCAN: u8 = 16;

    // ZDLE sequences (frameend values)
    pub const ZCRCE: u8 = b'h';
    pub const ZCRCG: u8 = b'i';
    pub const ZCRCQ: u8 = b'j';
    pub const ZCRCW: u8 = b'k';

    // ZRINIT capability flags (ZF0)
    pub const CANFDX: u8 = 0x01;
    pub const CANOVIO: u8 = 0x02;
    pub const CANBRK: u8 = 0x04;
    pub const CANCRY: u8 = 0x08;
    pub const CANLZW: u8 = 0x10;
    pub const CANFC32: u8 = 0x20;
    pub const ESCCTL: u8 = 0x40;
    pub const ESC8: u8 = 0x80;

    // ZFILE conversion options (ZF0)
    pub const ZCBIN: u8 = 1;
    pub const ZCNL: u8 = 2;
    pub const ZCRESUM: u8 = 3;
    // ZFILE management options (ZF1)
    pub const ZF1_ZMCLOB: u8 = 4; // replace existing
    // ZFILE transport options (ZF2)
    pub const ZTLZW: u8 = 1;
    pub const ZTCRYPT: u8 = 2;
    pub const ZTRLE: u8 = 3;

    // Cancel / finish sequences (zm.c canistr)
    pub const CANISTR: &[u8] = &[0x18; 8];
}
use _zmodem_consts::*;

/// Rolling speed estimate, matching Go's `recentSpeed`.
#[derive(Default, Clone)]
struct RecentSpeed {
    speed_cnt: usize,
    speed_idx: usize,
    time_array: [Option<Instant>; 5],
    step_array: [i64; 5],
}

// ---------------------------------------------------------------------------
// CRC-16 / CRC-32
// ---------------------------------------------------------------------------

/// CRC-16-CCITT (poly 0x1021, init 0, MSB-first, xorout 0). Matches `updcrc`.
fn crc16_update(crc: u16, byte: u8) -> u16 {
    let mut crc = crc ^ ((byte as u16) << 8);
    for _ in 0..8 {
        crc = if crc & 0x8000 != 0 {
            (crc << 1) ^ 0x1021
        } else {
            crc << 1
        };
    }
    crc & 0xffff
}


/// CRC-32 (reflected, poly 0xEDB88320, init 0xFFFFFFFF, xorout 0xFFFFFFFF).
/// Matches `UPDC32`; the final register over type+pos+crcrem must be 0xDEBB20E3.
fn crc32_update(crc: u32, byte: u8) -> u32 {
    let mut crc = crc ^ (byte as u32);
    for _ in 0..8 {
        crc = if crc & 1 != 0 {
            (crc >> 1) ^ 0xEDB88320
        } else {
            crc >> 1
        };
    }
    crc
}

// ---------------------------------------------------------------------------
// ZDLE escape encoding (zsendline semantics)
// ---------------------------------------------------------------------------

/// ZDLE-encode a byte sequence (for headers and data subpackets).
///
/// `escctl` mirrors lrzsz's `Zctlesc` (receiver set ESCCTL in ZRINIT): every
/// control character (< 0o100) is escaped, not just the fixed ZMODEM set.
fn zdle_encode(data: &[u8], escctl: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut lastsent = 0u8;
    for &c in data {
        let cat = if c & 0o140 != 0 {
            0
        } else if escctl {
            1
        } else {
            match c {
                ZDLE | 0o20 | 0o21 | 0o23 | 0o220 | 0o221 | 0o223 => 1,
                0o15 | 0o215 => {
                    if (lastsent & 0o177) == b'@' {
                        1
                    } else {
                        2
                    }
                }
                _ => 0,
            }
        };
        match cat {
            0 | 2 => {
                out.push(c);
                lastsent = c;
            }
            _ => {
                let e = c ^ 0o100;
                out.push(ZDLE);
                out.push(e);
                lastsent = e;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Header / data subpacket encoding
// ---------------------------------------------------------------------------

fn hdr_pos(pos: u32) -> [u8; 4] {
    [pos as u8, (pos >> 8) as u8, (pos >> 16) as u8, (pos >> 24) as u8]
}

fn pos_hdr(hdr: &[u8; 4]) -> u32 {
    (hdr[0] as u32) | ((hdr[1] as u32) << 8) | ((hdr[2] as u32) << 16) | ((hdr[3] as u32) << 24)
}

/// Send a binary-32 header (`ZPAD ZDLE ZBIN32 ...`).
fn encode_bin32_header(typ: u8, hdr: &[u8; 4], escctl: bool) -> Vec<u8> {
    let mut out = vec![ZPAD, ZDLE, ZBIN32];
    let mut crc = 0xFFFFFFFFu32;
    crc = crc32_update(crc, typ);
    out.extend_from_slice(&zdle_encode(&[typ], escctl));
    for &b in hdr {
        crc = crc32_update(crc, b);
    }
    out.extend_from_slice(&zdle_encode(hdr, escctl));
    let c = !crc;
    let crc_bytes = [c as u8, (c >> 8) as u8, (c >> 16) as u8, (c >> 24) as u8];
    out.extend_from_slice(&zdle_encode(&crc_bytes, escctl));
    out
}

/// Send a hex header (`ZPAD ZPAD ZDLE ZHEX <hex> CR LF [XON]`).
fn encode_hex_header(typ: u8, hdr: &[u8; 4]) -> Vec<u8> {
    let mut body = vec![typ];
    body.extend_from_slice(hdr);
    let mut crc = 0u16;
    for &b in &body {
        crc = crc16_update(crc, b);
    }
    // lrzsz transmits the raw CRC-16 (no two-zero finish); the receiver
    // recomputes over data+crc and checks for 0.
    let mut out = vec![ZPAD, ZPAD, ZDLE, ZHEX];
    out.extend_from_slice(format!("{:02x}", typ).as_bytes());
    for &b in hdr {
        out.extend_from_slice(format!("{:02x}", b).as_bytes());
    }
    out.extend_from_slice(format!("{:02x}{:02x}", (crc >> 8) & 0xff, crc & 0xff).as_bytes());
    out.push(b'\r');
    out.push(0o212); // 0x8a
    if typ != ZFIN && typ != ZACK {
        out.push(0o21); // XON
    }
    out
}


/// Encode a data subpacket using CRC-32 (4-byte CRC), used after ZBIN32
/// headers when the receiver requested CANFC32. Same ZDLE escaping as the
/// CRC-16 variant; only the trailing CRC width differs.
fn encode_data_subpacket32(data: &[u8], frameend: u8, escctl: bool) -> Vec<u8> {
    let mut crc = 0xFFFFFFFFu32;
    for &b in data {
        crc = crc32_update(crc, b);
    }
    crc = crc32_update(crc, frameend);
    let mut out = zdle_encode(data, escctl);
    out.push(ZDLE);
    out.push(frameend);
    let c = !crc;
    let crc_bytes = [c as u8, (c >> 8) as u8, (c >> 16) as u8, (c >> 24) as u8];
    out.extend_from_slice(&zdle_encode(&crc_bytes, escctl));
    if frameend == ZCRCW {
        out.push(0o21); // XON
    }
    out
}

/// Build a ZFILE data subpacket payload: `name\0 size mtime mode 0 0` (YMODEM batch).
fn build_zfile_payload(name: &str, size: u32, mtime: u32, mode: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    p.extend_from_slice(format!("{}", size).as_bytes());
    p.push(b' ');
    p.extend_from_slice(format!("{}", mtime).as_bytes());
    p.push(b' ');
    p.extend_from_slice(format!("{}", mode).as_bytes());
    p.push(b' ');
    p.push(b'0');
    p.push(b' ');
    p.push(b'0');
    p.push(b' ');
    p.push(0);
    p
}

/// Parse a ZFILE data subpacket payload into (name, size, mtime, mode).
fn parse_zfile_payload(payload: &[u8]) -> (String, u32, u32, u32) {
    let nul = payload.iter().position(|&b| b == 0).unwrap_or(payload.len());
    let name = String::from_utf8_lossy(&payload[..nul]).to_string();
    let meta = String::from_utf8_lossy(&payload[nul + 1..]).to_string();
    let mut fields = meta.split_whitespace();
    let size = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mtime = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let mode = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (name, size, mtime, mode)
}

// ---------------------------------------------------------------------------
// Decoder: incremental frame parser over a byte buffer
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ParseResult {
    Header(u8, [u8; 4]),
    Data(Vec<u8>, u8),
    NeedMore,
    Garbage,
    Cancel,
}

struct Decoder {
    buf: Vec<u8>,
    /// True when the last header was ZBIN32 (CRC-32 data subpackets).
    crc32: bool,
}

impl Decoder {
    fn new() -> Self {
        Self { buf: Vec::new(), crc32: false }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn next(&mut self) -> ParseResult {
        if self.buf.is_empty() {
            return ParseResult::NeedMore;
        }
        // Cancellation: 5 consecutive CAN (0x18).
        if self.buf.len() >= 5 && self.buf[..5].iter().all(|&b| b == ZDLE) {
            self.buf.clear();
            return ParseResult::Cancel;
        }
        // Synchronize on ZPAD (0x2a).
        if self.buf[0] != ZPAD {
            self.buf.remove(0);
            return ParseResult::Garbage;
        }
        if self.buf.len() < 3 {
            return ParseResult::NeedMore;
        }
        if self.buf[0] == ZPAD && self.buf[1] == ZDLE {
            match self.buf[2] {
                ZBIN32 => self.parse_bin32(),
                ZBIN => self.parse_bin16(),
                ZHEX => self.parse_hex(),
                _ => {
                    self.buf.remove(0);
                    ParseResult::Garbage
                }
            }
        } else if self.buf.len() >= 4
            && self.buf[0] == ZPAD
            && self.buf[1] == ZPAD
            && self.buf[2] == ZDLE
            && self.buf[3] == ZHEX
        {
            self.parse_hex()
        } else {
            self.buf.remove(0);
            ParseResult::Garbage
        }
    }

    fn zdlread(&self, i: usize) -> Option<(u8, usize)> {
        if i >= self.buf.len() {
            return None;
        }
        let c = self.buf[i];
        if c == ZDLE {
            if i + 1 >= self.buf.len() {
                return None;
            }
            let d = self.buf[i + 1];
            if d & 0o140 == 0o100 {
                return Some((d ^ 0o100, i + 2));
            }
            return Some((d, i + 2));
        }
        Some((c, i + 1))
    }

    fn parse_bin32(&mut self) -> ParseResult {
        let mut i = 3;
        let (typ, ni) = match self.zdlread(i) {
            Some(v) => v,
            None => return ParseResult::NeedMore,
        };
        i = ni;
        let mut hdr = [0u8; 4];
        let mut crc = 0xFFFFFFFFu32;
        crc = crc32_update(crc, typ);
        for slot in hdr.iter_mut() {
            let (v, ni) = match self.zdlread(i) {
                Some(v) => v,
                None => return ParseResult::NeedMore,
            };
            i = ni;
            *slot = v;
            crc = crc32_update(crc, v);
        }
        for _ in 0..4 {
            let (v, ni) = match self.zdlread(i) {
                Some(v) => v,
                None => return ParseResult::NeedMore,
            };
            i = ni;
            crc = crc32_update(crc, v);
        }
        if crc != 0xDEBB20E3 {
            self.buf.remove(0);
            return ParseResult::Garbage;
        }
        self.crc32 = true;
        self.buf.drain(..i);
        ParseResult::Header(typ, hdr)
    }


    fn parse_bin16(&mut self) -> ParseResult {
        self.crc32 = false;
        let mut i = 3;
        let (typ, ni) = match self.zdlread(i) {
            Some(v) => v,
            None => return ParseResult::NeedMore,
        };
        i = ni;
        let mut hdr = [0u8; 4];
        let mut crc = crc16_update(0, typ);
        for slot in hdr.iter_mut() {
            let (v, ni) = match self.zdlread(i) {
                Some(v) => v,
                None => return ParseResult::NeedMore,
            };
            i = ni;
            *slot = v;
            crc = crc16_update(crc, v);
        }
        for _ in 0..2 {
            let (v, ni) = match self.zdlread(i) {
                Some(v) => v,
                None => return ParseResult::NeedMore,
            };
            i = ni;
            crc = crc16_update(crc, v);
        }
        if crc != 0 {
            self.buf.remove(0);
            return ParseResult::Garbage;
        }
        self.buf.drain(..i);
        ParseResult::Header(typ, hdr)
    }

    fn parse_hex(&mut self) -> ParseResult {
        self.crc32 = false;
        let mut i = 4;
        let mut hex = Vec::new();
        while i < self.buf.len() && matches!(self.buf[i], b'0'..=b'9' | b'a'..=b'f') {
            hex.push(self.buf[i]);
            i += 1;
        }
        if hex.len() < 14 {
            if i < self.buf.len() && !matches!(self.buf[i], b'0'..=b'9' | b'a'..=b'f') {
                self.buf.remove(0);
                return ParseResult::Garbage;
            }
            return ParseResult::NeedMore;
        }
        while i < self.buf.len() && matches!(self.buf[i], b'\r' | b'\n' | 0o212 | 0o21) {
            i += 1;
        }
        let raw = hex_to_bytes(&hex[..14]);
        let typ = raw[0];
        let hdr = [raw[1], raw[2], raw[3], raw[4]];
        let mut crc = crc16_update(0, typ);
        for &b in &hdr {
            crc = crc16_update(crc, b);
        }
        let rcrc = ((raw[5] as u16) << 8) | raw[6] as u16;
        crc = crc16_update(crc, (rcrc >> 8) as u8);
        crc = crc16_update(crc, rcrc as u8);
        if crc != 0 {
            self.buf.remove(0);
            return ParseResult::Garbage;
        }
        self.buf.drain(..i);
        ParseResult::Header(typ, hdr)
    }

    /// Parse a data subpacket from raw (ZDLE-escaped) bytes.
    /// The buffer is assumed to start at the first data byte after a ZDATA header.
    /// Returns the payload and frameend; drains the consumed bytes.
    fn next_data(&mut self) -> ParseResult {
        if self.buf.is_empty() {
            return ParseResult::NeedMore;
        }
        let mut i = 0;
        let mut payload = Vec::new();
        loop {
            if i >= self.buf.len() {
                return ParseResult::NeedMore;
            }
            let c = self.buf[i];
            if c == ZDLE {
                if i + 1 >= self.buf.len() {
                    return ParseResult::NeedMore;
                }
                let d = self.buf[i + 1];
                i += 2;
                match d {
                    ZCRCE | ZCRCG | ZCRCQ | ZCRCW => {
                        // Data subpackets use CRC-16 (2 bytes) normally, or
                        // CRC-32 (4 bytes) after a ZBIN32 header (CANFC32).
                        let n = if self.crc32 { 4 } else { 2 };
                        if i + n > self.buf.len() {
                            return ParseResult::NeedMore;
                        }
                        let mut j = i;
                        for _ in 0..n {
                            match self.zdlread(j) {
                                Some((_, nj)) => j = nj,
                                None => return ParseResult::NeedMore,
                            }
                        }
                        self.buf.drain(..j);
                        // ZCRCW subpackets end with an XON (0x11) from the
                        // sender; consume it so it isn't mistaken for data.
                        if d == ZCRCW && self.buf.first() == Some(&0o21) {
                            self.buf.remove(0);
                        }
                        return ParseResult::Data(payload, d);
                    }
                    0x7f => payload.push(0x7f), // ZRUB0 → rubout
                    0xff => payload.push(0xff), // ZRUB1 → rubout
                    _ => payload.push(d ^ 0o100),
                }
            } else {
                payload.push(c);
                i += 1;
            }
        }
    }

    /// True when the buffer begins with a header (ZPAD-prefixed) rather than data.
    fn at_header(&self) -> bool {
        self.buf.first() == Some(&ZPAD)
    }
}

fn hex_to_bytes(hex: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((hi << 4) | lo);
    }
    out
}

// ---------------------------------------------------------------------------
// Shared feed buffer: filter pushes server output; engine reads it.
// ---------------------------------------------------------------------------

struct Feed {
    queue: Mutex<VecDeque<Vec<u8>>>,
    cond: Condvar,
}

impl Feed {
    fn new() -> Self {
        Self { queue: Mutex::new(VecDeque::new()), cond: Condvar::new() }
    }

    fn push(&self, bytes: Vec<u8>) {
        let mut q = self.queue.lock().unwrap();
        q.push_back(bytes);
        self.cond.notify_one();
    }

    /// Pop the next chunk, waiting up to `timeout`. Returns None on timeout.
    fn pop_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            let (guard, _) = self.cond.wait_timeout(q, timeout).unwrap();
            q = guard;
        }
        q.pop_front()
    }
}

// ---------------------------------------------------------------------------
// Shared state for an active zmodem transfer.
// ---------------------------------------------------------------------------

pub struct ZmodemState {
    pub upload: bool,
    pub auto_rename: bool,
    pub server_in: Arc<Mutex<Box<dyn Write + Send>>>,
    pub client_out: Arc<Mutex<Box<dyn Write + Send>>>,
    pub stopped: Arc<AtomicBool>,
    pub cleaned: Arc<AtomicBool>,
    pub error_occurred: Arc<AtomicBool>,
    transferred_size: Arc<AtomicI64>,
    last_update: Mutex<Option<Instant>>,
    recent_speed: Mutex<RecentSpeed>,
    recv_files: Mutex<Vec<String>>,
    send_files: Mutex<Vec<String>>,
    skip_files: Mutex<Vec<String>>,
    feed: Arc<Feed>,
    engine_done: Arc<AtomicBool>,
}

impl ZmodemState {
    pub fn new(
        upload: bool,
        auto_rename: bool,
        server_in: Arc<Mutex<Box<dyn Write + Send>>>,
        client_out: Arc<Mutex<Box<dyn Write + Send>>>,
    ) -> Self {
        Self {
            upload,
            auto_rename,
            server_in,
            client_out,
            stopped: Arc::new(AtomicBool::new(false)),
            cleaned: Arc::new(AtomicBool::new(false)),
            error_occurred: Arc::new(AtomicBool::new(false)),
            transferred_size: Arc::new(AtomicI64::new(0)),
            last_update: Mutex::new(None),
            recent_speed: Mutex::new(RecentSpeed::default()),
            recv_files: Mutex::new(Vec::new()),
            send_files: Mutex::new(Vec::new()),
            skip_files: Mutex::new(Vec::new()),
            feed: Arc::new(Feed::new()),
            engine_done: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_transferring(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst) || !self.cleaned.load(Ordering::SeqCst)
    }

    pub fn stop_transferring_files(&self) {
        self.handle_zmodem_error("Stopped");
    }

    fn write_message(&self, msg: &str) {
        let mut w = self.client_out.lock().unwrap();
        let line = format!("\r\x1b[2K{}\r\n", msg);
        let _ = w.write_all(line.as_bytes());
        let _ = w.flush();
    }

    fn handle_zmodem_error(&self, msg: &str) {
        if self
            .stopped
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.error_occurred.store(true, Ordering::SeqCst);
        {
            let mut w = self.server_in.lock().unwrap();
            let _ = w.write_all(CANISTR);
            let _ = w.write_all(&[0x08; 10]);
            let _ = w.flush();
        }
        self.write_message(msg);
    }

    fn write_server(&self, bytes: &[u8]) -> Result<()> {
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut w = self.server_in.lock().unwrap();
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    fn get_speed(&self, now: Instant) -> String {
        let mut rs = self.recent_speed.lock().unwrap();
        let size = self.transferred_size.load(Ordering::SeqCst);
        let speed = if rs.speed_cnt == 0 {
            rs.init_first_step(now);
            -1.0
        } else {
            rs.get_speed(size, now)
        };
        if speed < 0.0 {
            return "N/A".to_string();
        }
        format!("{}/s", convert_size_to_string(speed))
    }

    fn update_progress(&self, delta: i64) {
        if delta <= 0 {
            return;
        }
        self.transferred_size.fetch_add(delta, Ordering::SeqCst);
        let now = Instant::now();
        let mut last = self.last_update.lock().unwrap();
        if let Some(prev) = *last {
            if now.duration_since(prev) < Duration::from_millis(200) {
                return;
            }
        }
        *last = Some(now);
        drop(last);
        let size = convert_size_to_string(self.transferred_size.load(Ordering::SeqCst) as f64);
        let speed = self.get_speed(now);
        let line = format!("\r\x1b[2KTransferred {}, Speed {}.", size, speed);
        let mut w = self.client_out.lock().unwrap();
        let _ = w.write_all(line.as_bytes());
        let _ = w.flush();
    }

    /// Feed server output bytes to the engine. Returns false when the engine
    /// has finished and the transfer should be dropped.
    pub fn handle_server_output(&self, buf: &[u8]) -> bool {
        if self.engine_done.load(Ordering::SeqCst) {
            return false;
        }
        self.feed.push(buf.to_vec());
        true
    }

    fn feed_handle(&self) -> Arc<Feed> {
        self.feed.clone()
    }

    fn mark_engine_done(&self) {
        self.engine_done.store(true, Ordering::SeqCst);
    }
}

impl RecentSpeed {
    fn init_first_step(&mut self, now: Instant) {
        self.time_array[0] = Some(now);
        self.step_array[0] = 0;
        self.speed_cnt = 1;
        self.speed_idx = 1;
    }

    fn get_speed(&mut self, step: i64, now: Instant) -> f64 {
        let speed = if self.speed_cnt <= 5 {
            self.speed_cnt += 1;
            let dt = now.duration_since(self.time_array[0].unwrap()).as_secs_f64();
            (step - self.step_array[0]) as f64 / dt
        } else {
            let dt = now
                .duration_since(self.time_array[self.speed_idx].unwrap())
                .as_secs_f64();
            (step - self.step_array[self.speed_idx]) as f64 / dt
        };
        self.time_array[self.speed_idx] = Some(now);
        self.step_array[self.speed_idx] = step;
        self.speed_idx = (self.speed_idx + 1) % 5;
        if speed.is_nan() {
            -1.0
        } else {
            speed
        }
    }
}

// ---------------------------------------------------------------------------
// Engine: sender (upload) and receiver (download)
// ---------------------------------------------------------------------------

pub struct ZmodemTransfer {
    pub state: Arc<ZmodemState>,
}

impl ZmodemTransfer {
    pub fn new(
        upload: bool,
        auto_rename: bool,
        server_in: Arc<Mutex<Box<dyn Write + Send>>>,
        client_out: Arc<Mutex<Box<dyn Write + Send>>>,
    ) -> Self {
        Self {
            state: Arc::new(ZmodemState::new(upload, auto_rename, server_in, client_out)),
        }
    }

    pub fn is_upload(&self) -> bool {
        self.state.upload
    }
}

/// Detect a ZMODEM init sequence in `buf`.
/// `**\x18B01` (ZRINIT) → remote is a receiver → we upload.
/// `**\x18B00` (ZRQINIT) → remote is a sender → we download.
pub fn detect_zmodem(
    buf: &[u8],
    auto_rename: bool,
    server_in: Arc<Mutex<Box<dyn Write + Send>>>,
    client_out: Arc<Mutex<Box<dyn Write + Send>>>,
) -> Option<ZmodemTransfer> {
    let mut i = 0;
    while i + 16 <= buf.len() {
        if buf[i] == ZPAD
            && buf[i + 1] == ZPAD
            && buf[i + 2] == ZDLE
            && buf[i + 3] == ZHEX
            && buf[i + 4] == b'0'
            && (buf[i + 5] == b'0' || buf[i + 5] == b'1')
        {
            let upload = buf[i + 5] == b'1';
            return Some(ZmodemTransfer::new(upload, auto_rename, server_in, client_out));
        }
        i += 1;
    }
    None
}

/// Run the engine: choose upload/download, then drive the protocol.
pub fn handle_zmodem_event(
    state: Arc<ZmodemState>,
    choose_upload_files: impl Fn() -> Result<Vec<String>> + Send + 'static,
    choose_download_path: impl Fn() -> Result<String> + Send + 'static,
) {
    std::thread::sleep(Duration::from_millis(100));
    if state.stopped.load(Ordering::SeqCst) {
        state.mark_engine_done();
        return;
    }
    let result = if state.upload {
        match choose_upload_files() {
            Ok(files) => run_sender(&state, files),
            Err(e) => Err(e),
        }
    } else {
        match choose_download_path() {
            Ok(path) => run_receiver(&state, &path),
            Err(e) => Err(e),
        }
    };
    if let Err(e) = result {
        state.handle_zmodem_error(&e.to_string());
    }
    state.mark_engine_done();
    if !state.error_occurred.load(Ordering::SeqCst) {
        state.write_message("\x1b[1;32mSuccess!!\x1b[0m");
        report_files(&state);
    }
}

fn report_files(state: &Arc<ZmodemState>) {
    let recv = state.recv_files.lock().unwrap();
    let send = state.send_files.lock().unwrap();
    let skip = state.skip_files.lock().unwrap();
    if !recv.is_empty() {
        let msg = join_file_names("Saved files:", &recv);
        let mut w = state.client_out.lock().unwrap();
        let _ = w.write_all(format!("\r\n{}\r\n", msg).as_bytes());
        let _ = w.flush();
    } else if !send.is_empty() {
        let sent: Vec<String> = send.iter().filter(|n| !skip.contains(n)).cloned().collect();
        if !sent.is_empty() {
            let msg = join_file_names("Transferred files:", &sent);
            let mut w = state.client_out.lock().unwrap();
            let _ = w.write_all(format!("\r\n{}\r\n", msg).as_bytes());
            let _ = w.flush();
        }
    }
    if !skip.is_empty() {
        let msg = join_file_names("Skipped files:", &skip);
        let mut w = state.client_out.lock().unwrap();
        let _ = w.write_all(format!("\r\n{}\r\n", msg).as_bytes());
        let _ = w.flush();
    }
}

// ---------------------------------------------------------------------------
// Sender (sz): upload files to a remote rz receiver
// ---------------------------------------------------------------------------

fn run_sender(state: &Arc<ZmodemState>, files: Vec<String>) -> Result<()> {
    if files.is_empty() {
        return Err(anyhow!("No files specified"));
    }
    let feed = state.feed_handle();
    let mut decoder = Decoder::new();

    // Send ZRQINIT to elicit a fresh ZRINIT from the remote receiver.
    state.write_server(&encode_hex_header(ZRQINIT, &[0, 0, 0, 0]))?;
    // Wait for ZRINIT (or ZRPOS from a prior session) and capture the
    // receiver capabilities (ESCCTL tells us to escape control characters).
    let flags = wait_for_rinit(&feed, &mut decoder, state)?;
    let escctl = flags & ESCCTL != 0;
    for file in &files {
        send_one_file(state, &feed, &mut decoder, file, escctl)?;
    }

    // Finish session.
    state.write_server(&encode_hex_header(ZFIN, &[0, 0, 0, 0]))?;
    let _ = wait_for_fin(&feed, &mut decoder, state);
    // Emit "OO\b\b" to confirm clean end (matching Go's zmodemOverAndOut and lrzsz saybibi/ackbibi).
    let _ = state.write_server(b"OO\x08\x08");
    Ok(())
}

fn wait_for_rinit(
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    state: &Arc<ZmodemState>,
) -> Result<u8> {
    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => {
                state.write_server(&encode_hex_header(ZRQINIT, &[0, 0, 0, 0]))?;
                continue;
            }
        }
        loop {
            match decoder.next() {
                ParseResult::Header(typ, hdr) => match typ {
                    // ZRINIT carries the receiver capabilities in ZP3 (hdr[3]).
                    ZRINIT => return Ok(hdr[3]),
                    ZRPOS => return Ok(0),
                    _ => continue,
                },
                ParseResult::Data(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled by server")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
    }
}

fn send_one_file(
    state: &Arc<ZmodemState>,
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    file: &str,
    escctl: bool,
) -> Result<()> {
    let path = Path::new(file);
    let meta = std::fs::metadata(path).map_err(|e| anyhow!("{}: {}", file, e))?;
    if !meta.is_file() {
        return Ok(());
    }
    let size = meta.len() as u32;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0);
    let mode = 0o100644;
    let name = path.file_name().unwrap().to_string_lossy().to_string();

    state.send_files.lock().unwrap().push(name.clone());

    let zfile_flags = [ZCBIN, ZF1_ZMCLOB, 0, 0];
    state.write_server(&encode_bin32_header(ZFILE, &zfile_flags, escctl))?;
    let payload = build_zfile_payload(&name, size, mtime, mode);
    state.write_server(&encode_data_subpacket32(&payload, ZCRCW, escctl))?;

    let start_offset = wait_for_rpos(feed, decoder, state)?;
    if start_offset >= size {
        return Ok(());
    }

    state.write_server(&encode_bin32_header(ZDATA, &hdr_pos(start_offset), escctl))?;
    let f = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(f);
    use std::io::{Read, Seek};
    reader.seek(std::io::SeekFrom::Start(start_offset as u64))?;
    let mut remaining = size - start_offset;
    let mut buf = vec![0u8; 1024];
    while remaining > 0 {
        let want = remaining.min(1024) as usize;
        let n = reader.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        let is_last = n as u32 >= remaining;
        let frameend = if is_last { ZCRCE } else { ZCRCG };
        state.write_server(&encode_data_subpacket32(chunk, frameend, escctl))?;
        remaining -= n as u32;
        state.update_progress(n as i64);
    }

    state.write_server(&encode_bin32_header(ZEOF, &hdr_pos(size), escctl))?;
    wait_for_next(feed, decoder, state)
}

fn wait_for_rpos(
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    state: &Arc<ZmodemState>,
) -> Result<u32> {
    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => continue,
        }
        loop {
            match decoder.next() {
                ParseResult::Header(typ, hdr) => match typ {
                    ZRPOS => return Ok(pos_hdr(&hdr)),
                    ZSKIP => return Err(anyhow!("skipped")),
                    _ => continue,
                },
                ParseResult::Data(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
    }
}

fn wait_for_next(
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    state: &Arc<ZmodemState>,
) -> Result<()> {
    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => continue,
        }
        loop {
            match decoder.next() {
                ParseResult::Header(typ, _) => match typ {
                    ZRINIT | ZRPOS | ZSKIP => return Ok(()),
                    _ => continue,
                },
                ParseResult::Data(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
    }
}

fn wait_for_fin(
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    state: &Arc<ZmodemState>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => continue,
        }
        loop {
            match decoder.next() {
                ParseResult::Header(typ, _) => {
                    if typ == ZFIN {
                        return Ok(());
                    }
                }
                ParseResult::Data(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Receiver (rz): download files from a remote sz sender
// ---------------------------------------------------------------------------

fn run_receiver(state: &Arc<ZmodemState>, dir: &str) -> Result<()> {
    let feed = state.feed_handle();
    let mut decoder = Decoder::new();

    // Send ZRINIT with our capabilities. ZMODEM puts the capability flags in
    // the LAST header byte (ZP3), not the first (lrzsz sends hdr=[0,0,0,0x27]).
    let zrinit_flags = || [0u8, 0, 0, CANFDX | CANOVIO | CANBRK | CANFC32];
    state.write_server(&encode_hex_header(ZRINIT, &zrinit_flags()))?;

    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        loop {
            match decoder.next() {
                ParseResult::Header(typ, _) => match typ {
                    ZRQINIT => {
                        // Remote sender is inviting a receiver; answer with ZRINIT.
                        state.write_server(&encode_hex_header(ZRINIT, &zrinit_flags()))?;
                    }
                    ZSINIT => {
                        // Session init: acknowledge so the sender proceeds.
                        state.write_server(&encode_hex_header(ZACK, &hdr_pos(0)))?;
                    }
                    ZFILE => {
                        let (name, size, _, _) = read_zfile_payload(&feed, &mut decoder, state)?;
                        let path = resolve_dest(dir, &name, state.auto_rename);
                        state.write_server(&encode_hex_header(ZRPOS, &hdr_pos(0)))?;
                        recv_file_data(state, &feed, &mut decoder, &path, size)?;
                    }
                    ZEOF => {
                        state.write_server(&encode_hex_header(ZRINIT, &zrinit_flags()))?;
                    }
                    ZFIN => {
                        state.write_server(&encode_hex_header(ZFIN, &[0, 0, 0, 0]))?;
                        return Ok(());
                    }
                    _ => continue,
                },
                ParseResult::Data(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => {
                state.write_server(&encode_hex_header(ZRINIT, &zrinit_flags()))?;
                continue;
            }
        }
    }
}

fn read_zfile_payload(
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    state: &Arc<ZmodemState>,
) -> Result<(String, u32, u32, u32)> {
    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        loop {
            // The ZFILE metadata arrives as a raw data subpacket. Parse any
            // buffered data first; only wait for more when we need it.
            match decoder.next_data() {
                ParseResult::Data(payload, _) => return Ok(parse_zfile_payload(&payload)),
                ParseResult::Header(_, _) => continue,
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => continue,
        }
    }
}

fn recv_file_data(
    state: &Arc<ZmodemState>,
    feed: &Arc<Feed>,
    decoder: &mut Decoder,
    path: &Path,
    _size: u32,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::File::create(path)?;
    loop {
        if state.stopped.load(Ordering::SeqCst) {
            return Err(anyhow!("stopped"));
        }
        loop {
            // During data reception, bytes are either raw data subpackets
            // (ZDLE-escaped) or a ZEOF/ZACK header (ZPAD-prefixed). Parse any
            // buffered data first; only wait for more when we need it.
            let res = if decoder.at_header() {
                decoder.next()
            } else {
                decoder.next_data()
            };
            match res {
                ParseResult::Data(payload, frameend) => {
                    f.write_all(&payload)?;
                    state.update_progress(payload.len() as i64);
                    if frameend == ZCRCW || frameend == ZCRCQ {
                        let cur = std::fs::metadata(path)?.len() as u32;
                        state.write_server(&encode_hex_header(ZACK, &hdr_pos(cur)))?;
                    }
                }
                ParseResult::Header(typ, _) => match typ {
                    ZEOF => {
                        f.flush()?;
                        state
                            .recv_files
                            .lock()
                            .unwrap()
                            .push(path.file_name().unwrap().to_string_lossy().to_string());
                        return Ok(());
                    }
                    _ => continue,
                },
                ParseResult::Cancel => return Err(anyhow!("cancelled")),
                ParseResult::NeedMore => break,
                ParseResult::Garbage => continue,
            }
        }
        match feed.pop_timeout(Duration::from_millis(500)) {
            Some(bytes) => decoder.feed(&bytes),
            None => continue,
        }
    }
}


fn resolve_dest(dir: &str, name: &str, auto_rename: bool) -> PathBuf {
    let base = PathBuf::from(dir).join(name);
    if !auto_rename {
        return base;
    }
    if !base.exists() {
        return base;
    }
    let mut i = 1;
    loop {
        let candidate = PathBuf::from(dir).join(format!("{}.{}", name, i));
        if !candidate.exists() {
            return candidate;
        }
        i += 1;
    }
}

fn join_file_names(header: &str, names: &[String]) -> String {
    let mut s = header.to_string();
    for n in names {
        if n.is_empty() {
            continue;
        }
        s.push_str("\r\n- ");
        s.push_str(n);
    }
    s
}

fn convert_size_to_string(size: f64) -> String {
    let mut size = size;
    let mut unit = "B";
    if size < 1024.0 {
        // keep B
    } else {
        size /= 1024.0;
        unit = "KB";
        if size >= 1024.0 {
            size /= 1024.0;
            unit = "MB";
            if size >= 1024.0 {
                size /= 1024.0;
                unit = "GB";
                if size >= 1024.0 {
                    size /= 1024.0;
                    unit = "TB";
                }
            }
        }
    }
    if size >= 100.0 {
        format!("{:.0} {}", size, unit)
    } else if size >= 10.0 {
        format!("{:.1} {}", size, unit)
    } else {
        format!("{:.2} {}", size, unit)
    }
}

// ---------------------------------------------------------------------------
// File dialog helpers (zenity)
// ---------------------------------------------------------------------------

pub fn choose_upload_files(
    drag_cmd: String,
    allowed_dirs: Vec<String>,
    default_upload: String,
) -> Result<Vec<String>> {
    if !drag_cmd.is_empty() {
        if let Ok(out) = std::process::Command::new("sh")
            .arg("-c")
            .arg(&drag_cmd)
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let files: Vec<String> = s
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !files.is_empty() {
                    return Ok(files);
                }
            }
        }
    }
    let mut cmd = std::process::Command::new("zenity");
    cmd.args(["--file-selection", "--title=Choose some files to send"]);
    if !default_upload.is_empty() {
        cmd.args(["--filename=", &default_upload]);
    }
    let _ = allowed_dirs;
    let out = cmd.output().map_err(|e| anyhow!("zenity failed: {}", e))?;
    if !out.status.success() {
        return Err(anyhow!("Upload cancelled"));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let files: Vec<String> = s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    if files.is_empty() {
        return Err(anyhow!("No files selected"));
    }
    Ok(files)
}

pub fn choose_download_path() -> Result<String> {
    let out = std::process::Command::new("zenity")
        .args(["--file-selection", "--directory", "--title=Choose a folder to save file(s)"])
        .output()
        .map_err(|e| anyhow!("zenity failed: {}", e))?;
    if !out.status.success() {
        return Err(anyhow!("Download cancelled"));
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return Err(anyhow!("No directory selected"));
    }
    Ok(s)
}

pub fn resolve_home_dir(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// Legacy API shim (ZmodemProxy) retained for compatibility
// ---------------------------------------------------------------------------

pub struct ZmodemProxy {
    is_active: bool,
    _private: (),
}

impl Default for ZmodemProxy {
    fn default() -> Self {
        Self::new()
    }
}

impl ZmodemProxy {
    pub fn new() -> Self {
        Self { is_active: false, _private: () }
    }

    pub fn start_proxy(&mut self) -> Result<()> {
        self.is_active = true;
        Ok(())
    }

    pub fn stop_proxy(&mut self) -> Result<()> {
        self.is_active = false;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.is_active
    }

    pub fn proxy_zmodem_send(&mut self, _file: &Path) -> Result<()> {
        Ok(())
    }

    pub fn proxy_zmodem_receive(&mut self, _output_dir: &Path) -> Result<()> {
        Ok(())
    }

    pub fn detect_zmodem_sequence(&self, data: &[u8]) -> bool {
        detect_zmodem(
            data,
            false,
            Arc::new(Mutex::new(Box::new(std::io::sink()))),
            Arc::new(Mutex::new(Box::new(std::io::sink()))),
        )
        .is_some()
    }

    pub fn send_zmodem_start(&self) -> Vec<u8> {
        encode_hex_header(ZRQINIT, &[0, 0, 0, 0])
    }

    pub fn send_zmodem_cancel(&self) -> Vec<u8> {
        CANISTR.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_writers() -> (Arc<Mutex<Box<dyn Write + Send>>>, Arc<Mutex<Box<dyn Write + Send>>>) {
        (
            Arc::new(Mutex::new(Box::new(std::io::sink()))),
            Arc::new(Mutex::new(Box::new(std::io::sink()))),
        )
    }

    #[test]
    fn test_detect_zmodem_upload() {
        let buf = b"some \x1b[s::TRZSZ:TRANSFER:S:1.2.0:123:456\r\n**\x18B01abcdef123456rest";
        let (si, co) = test_writers();
        let t = detect_zmodem(buf, false, si, co);
        assert!(t.is_some());
        assert!(t.unwrap().is_upload());
    }

    #[test]
    fn test_detect_zmodem_download() {
        let buf = b"**\x18B00abcdef123456";
        let (si, co) = test_writers();
        let t = detect_zmodem(buf, false, si, co);
        assert!(t.is_some());
        assert!(!t.unwrap().is_upload());
    }

    #[test]
    fn test_detect_zmodem_no_match() {
        let buf = b"just plain text";
        let (si, co) = test_writers();
        assert!(detect_zmodem(buf, false, si, co).is_none());
    }

    #[test]
    fn test_convert_size() {
        assert_eq!(convert_size_to_string(0.0), "0.00 B");
        assert_eq!(convert_size_to_string(1536.0), "1.50 KB");
        assert_eq!(convert_size_to_string(10.0 * 1024.0 * 1024.0), "10.0 MB");
    }

    #[test]
    fn test_crc16_vector() {
        // CRC-16-CCITT over 5 zero bytes is 0 (raw register, no finish step).
        let mut crc = 0u16;
        for &b in &[0u8, 0, 0, 0, 0] {
            crc = crc16_update(crc, b);
        }
        assert_eq!(crc, 0x0000);
    }

    #[test]
    fn test_crc32_vector() {
        let mut crc = 0xFFFFFFFFu32;
        for &b in &[4u8, 0, 0, 0, 1, 0x4b, 0x61, 0xa5, 0x44] {
            crc = crc32_update(crc, b);
        }
        assert_eq!(crc, 0xDEBB20E3);
    }

    #[test]
    fn test_hex_header_roundtrip() {
        let hdr = encode_hex_header(ZRQINIT, &[0, 0, 0, 0]);
        assert_eq!(&hdr[..4], &[ZPAD, ZPAD, ZDLE, ZHEX]);
        let mut dec = Decoder::new();
        dec.feed(&hdr);
        match dec.next() {
            ParseResult::Header(typ, pos) => {
                assert_eq!(typ, ZRQINIT);
                assert_eq!(pos, [0, 0, 0, 0]);
            }
            other => panic!("expected header, got {:?}", other),
        }
    }

    #[test]
    fn test_bin32_header_roundtrip() {
        let hdr = encode_bin32_header(ZFILE, &[1, 4, 0, 0], false);
        let mut dec = Decoder::new();
        dec.feed(&hdr);
        match dec.next() {
            ParseResult::Header(typ, flags) => {
                assert_eq!(typ, ZFILE);
                assert_eq!(flags, [1, 4, 0, 0]);
            }
            other => panic!("expected header, got {:?}", other),
        }
    }
    #[test]
    fn test_data_subpacket_roundtrip() {
        // Sender emits ZBIN32 headers, so data subpackets use CRC-32.
        let payload = b"hello world\x00\x00binary\xffdata";
        let frame = encode_data_subpacket32(payload, ZCRCG, false);
        let mut dec = Decoder::new();
        dec.feed(&frame);
        match dec.next_data() {
            ParseResult::Data(data, frameend) => {
                assert_eq!(&data, payload);
                assert_eq!(frameend, ZCRCG);
            }
            other => panic!("expected data, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_zfile_payload() {
        let payload = build_zfile_payload("test.bin", 200, 1527255720, 0o100644);
        let (name, size, mtime, mode) = parse_zfile_payload(&payload);
        assert_eq!(name, "test.bin");
        assert_eq!(size, 200);
        assert_eq!(mtime, 1527255720);
        assert_eq!(mode, 0o100644);
    }

    #[test]
    fn test_join_file_names() {
        let names = vec!["a.txt".to_string(), "b.txt".to_string()];
        assert_eq!(
            join_file_names("Saved files:", &names),
            "Saved files:\r\n- a.txt\r\n- b.txt"
        );
    }

    // ------------------------------------------------------------------
    // End-to-end smoke tests against the real lrzsz `sz`/`rz` binaries.
    // ------------------------------------------------------------------

    /// Spawn a child process attached to a pty slave; returns (pid, master_fd).
    fn spawn_on_pty(cmd: &str, args: &[&str]) -> (u32, i32) {
        spawn_on_pty_cwd(cmd, args, None)
    }

    fn spawn_on_pty_cwd(cmd: &str, args: &[&str], cwd: Option<&str>) -> (u32, i32) {
        let mut master = 0i32;
        let mut slave = 0i32;
        let ok = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                std::ptr::null_mut::<libc::winsize>(),
            )
        };
        assert_eq!(ok, 0, "openpty failed");
        // Put the slave in raw mode from the parent so the child starts with
        // a raw terminal (no echo, no signal translation).
        unsafe {
            let mut tio: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave, &mut tio);
            libc::cfmakeraw(&mut tio);
            libc::tcsetattr(slave, libc::TCSANOW, &tio);
        }
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                // Make the slave our controlling terminal.
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                libc::close(slave);
                libc::close(master);
            }
            if let Some(cwd) = cwd {
                let c = std::ffi::CString::new(cwd).unwrap();
                unsafe {
                    libc::chdir(c.as_ptr());
                }
            }
            let cargs: Vec<std::ffi::CString> = std::iter::once(cmd)
                .chain(args.iter().copied())
                .map(|s| std::ffi::CString::new(s).unwrap())
                .collect();
            let mut ptrs: Vec<*const libc::c_char> = cargs.iter().map(|c| c.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            let _ = unsafe { libc::execvp(cargs[0].as_ptr(), ptrs.as_ptr()) };
            unsafe { libc::_exit(127) };
        }
        unsafe {
            libc::close(slave);
        }
        (pid as u32, master)
    }

    /// Build a state whose server_in writes to the pty master and whose feed
    /// is pumped by a reader thread on the master fd.
    fn state_on_pty(master: i32, upload: bool) -> (Arc<ZmodemState>, Arc<Mutex<Vec<u8>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let server_in: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(TraceWriter::new(master, log.clone()))));
        let client_out: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(std::io::sink())));
        let state = Arc::new(ZmodemState::new(upload, true, server_in, client_out));
        let st_feed = state.feed_handle();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n <= 0 {
                    break;
                }
                st_feed.push(buf[..n as usize].to_vec());
            }
        });
        (state, log)
    }

    /// Writer that records every byte written to the pty master (for tracing).
    struct TraceWriter {
        master: i32,
        log: Arc<Mutex<Vec<u8>>>,
    }
    impl TraceWriter {
        fn new(master: i32, log: Arc<Mutex<Vec<u8>>>) -> Self {
            Self { master, log }
        }
    }
    impl Write for TraceWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = unsafe { libc::write(self.master, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            self.log.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            Ok(n as usize)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn e2e_receive_from_real_sz() {
        let dir = std::env::temp_dir().join(format!("nrzsz_e2e_recv_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hello.txt");
        let content = b"Hello, ZMODEM!\nSecond line with binary \x00\x01\xff.\n";
        std::fs::write(&src, content).unwrap();

        let (pid, master) = spawn_on_pty("/usr/local/bin/sz", &["--binary", "-q", src.to_str().unwrap()]);
        let (state, log) = state_on_pty(master, false);
        let outdir = dir.join("out");
        std::fs::create_dir_all(&outdir).unwrap();
        let dirs = outdir.to_str().unwrap().to_string();
        let r = run_receiver(&state, &dirs);
        if let Err(e) = &r {
            let wl = log.lock().unwrap();
            panic!("run_receiver failed: {:?}; wrote {} bytes: {:?}", e, wl.len(), wl);
        }
        let got = std::fs::read(outdir.join("hello.txt")).unwrap();
        assert_eq!(got, content, "downloaded content mismatch");

        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
            libc::close(master);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn e2e_send_to_real_rz() {
        let dir = std::env::temp_dir().join(format!("nrzsz_e2e_send_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("data.bin");
        let content = b"\x00\x01\x02\x03hello\xff\xfe binary data \x7f\x80 end";
        std::fs::write(&src, content).unwrap();

        let recvdir = dir.join("recv");
        std::fs::create_dir_all(&recvdir).unwrap();
        let (pid, master) = spawn_on_pty_cwd(
            "/usr/local/bin/rz",
            &["--binary", "-q"],
            Some(recvdir.to_str().unwrap()),
        );

        let (state, log) = state_on_pty(master, true);
        let r = run_sender(&state, vec![src.to_str().unwrap().to_string()]);
        if let Err(e) = &r {
            let wl = log.lock().unwrap();
            panic!("run_sender failed: {:?}; wrote {} bytes: {:?}", e, wl.len(), wl);
        }
        let got = std::fs::read(recvdir.join("data.bin")).unwrap();
        assert_eq!(got, content, "uploaded content mismatch");

        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
            libc::close(master);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
