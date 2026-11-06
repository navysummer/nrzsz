use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use anyhow::Result;
use log::debug;

use crate::buffer::NrzszBuffer;
use crate::filter::parse_trigger;
use crate::tunnel::{get_hello_constant, read_line_from_stream};

const RELAY_STANDBY: i32 = 0;
const RELAY_HANDSHAKING: i32 = 1;
const RELAY_TRANSFERRING: i32 = 2;

type TunnelConnector = dyn Fn(u16) -> Result<TcpStream> + Send;
type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type SharedReader = Arc<Mutex<Box<dyn Read + Send>>>;
type SharedTrigger = Arc<Mutex<Option<crate::filter::NrzszTrigger>>>;
type SharedTunnelConnector = Arc<Mutex<Option<Box<TunnelConnector>>>>;
type SharedCallback = Arc<Mutex<Option<Box<dyn Fn(bool) + Send>>>>;

/// A relay that supports nrzsz (nsz / nrz) through a jump server.
///
/// ┌────────┐   ClientIn   ┌────────────┐   ServerIn   ┌────────┐
/// │        ├─────────────►│            ├─────────────►│        │
/// │ Client │              │ NrzszRelay │              │ Server │
/// │        │◄─────────────┤            │◄─────────────┤        │
/// └────────┘   ClientOut  └────────────┘   ServerOut  └────────┘
pub struct NrzszRelay {
    client_in: Box<dyn Read + Send>,
    client_out: SharedWriter,
    server_in: SharedWriter,
    server_out: SharedReader,
    stdin_buffer: Arc<NrzszBuffer>,
    stdout_buffer: Arc<NrzszBuffer>,
    relay_status: Arc<AtomicI32>,
    trigger_mode: SharedTrigger,
    tunnel_connector: SharedTunnelConnector,
    closed: Arc<AtomicBool>,
    transfer_state_callback: SharedCallback,
    trace_logger: Option<crate::tracelog::TraceLogger>,
}

impl NrzszRelay {
    pub fn new(
        client_in: Box<dyn Read + Send>,
        client_out: Box<dyn Write + Send>,
        server_in: Box<dyn Write + Send>,
        server_out: Box<dyn Read + Send>,
    ) -> Self {
        Self {
            client_in,
            client_out: Arc::new(Mutex::new(client_out)),
            server_in: Arc::new(Mutex::new(server_in)),
            server_out: Arc::new(Mutex::new(server_out)),
            stdin_buffer: Arc::new(NrzszBuffer::new()),
            stdout_buffer: Arc::new(NrzszBuffer::new()),
            relay_status: Arc::new(AtomicI32::new(RELAY_STANDBY)),
            trigger_mode: Arc::new(std::sync::Mutex::new(None)),
            tunnel_connector: Arc::new(std::sync::Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            transfer_state_callback: Arc::new(std::sync::Mutex::new(None)),
            trace_logger: None,
        }
    }

    pub fn set_trace_logger(&mut self, logger: crate::tracelog::TraceLogger) {
        self.trace_logger = Some(logger);
    }


    pub fn set_tunnel_connector<F>(&mut self, connector: F)
    where
        F: Fn(u16) -> Result<TcpStream> + Send + 'static,
    {
        *self.tunnel_connector.lock().unwrap() = Some(Box::new(connector));
    }

    pub fn set_transfer_state_callback<F>(&mut self, callback: F)
    where
        F: Fn(bool) + Send + 'static,
    {
        *self.transfer_state_callback.lock().unwrap() = Some(Box::new(callback));
    }

    #[allow(dead_code)]
    fn fire_transfer_state(&self, transferring: bool) {
        if let Ok(guard) = self.transfer_state_callback.lock() {
            if let Some(ref cb) = *guard {
                cb(transferring);
            }
        }
    }

    #[allow(dead_code)]
    fn reset_to_standby(&self) {
        let prev = self.relay_status.swap(RELAY_STANDBY, Ordering::SeqCst);
        if prev == RELAY_TRANSFERRING {
            self.fire_transfer_state(false);
        }
    }

    pub fn get_status(&self) -> i32 {
        self.relay_status.load(Ordering::SeqCst)
    }

    /// Start the relay: spawn input/output forwarding threads and wait.
    /// Takes `client_in` by value since it's consumed.
    pub fn run(self) -> Result<()> {
        let mut client_in = self.client_in;
        let server_in = self.server_in.clone();
        let stdin_buffer = self.stdin_buffer.clone();
        let relay_status = self.relay_status.clone();
        let closed = self.closed.clone();

        thread::spawn(move || {
            let mut buf = [0u8; 32768];
            loop {
                match client_in.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = &buf[..n];
                        let status = relay_status.load(Ordering::SeqCst);
                        if status == RELAY_HANDSHAKING {
                            stdin_buffer.add_buffer(data);
                            continue;
                        }
                        // Detect Ctrl+C during transfer → reset to standby
                        if status == RELAY_TRANSFERRING && data.len() == 1 && data[0] == 0x03 {
                            relay_status.store(RELAY_STANDBY, Ordering::SeqCst);
                            continue;
                        }
                        let mut w = server_in.lock().unwrap();
                        w.write_all(data).ok();
                        w.flush().ok();
                    }
                    Err(_) => break,
                }
                if closed.load(Ordering::SeqCst) {
                    break;
                }
            }
        });

        // Spawn server output → client thread with trigger detection
        let server_out = self.server_out.clone();
        let client_out = self.client_out.clone();
        let stdout_buffer = self.stdout_buffer.clone();
        let relay_status = self.relay_status.clone();
        let stdin_buffer = self.stdin_buffer.clone();
        let server_in = self.server_in.clone();
        let trigger_mode = self.trigger_mode.clone();
        let tunnel_connector = self.tunnel_connector.clone();
        let closed = self.closed.clone();
        let trace_logger = self.trace_logger.clone();

        thread::spawn(move || {
            let mut buf = [0u8; 32768];
            let mut detect_buf = Vec::new();
            loop {
                let n = {
                    let mut r = server_out.lock().unwrap();
                    r.read(&mut buf)
                };
                match n {
                    Ok(0) => break,
                    Ok(n) => {
                        let data = &buf[..n];
                        let status = relay_status.load(Ordering::SeqCst);

                        if status == RELAY_STANDBY {
                            detect_buf.extend_from_slice(data);
                            // Keep only the trailing window: the trigger line is
                            // ~60 bytes, so 4 KiB is plenty and prevents the
                            // detect buffer (and the rfind scan) from growing
                            // without bound on a silent session.
                            const DETECT_KEEP: usize = 4096;
                            if detect_buf.len() > DETECT_KEEP {
                                let drain = detect_buf.len() - DETECT_KEEP;
                                detect_buf.drain(..drain);
                            }
                            if let Some(trigger) = detect_trigger_in_buffer(&detect_buf) {
                                debug!("Relay detected trigger: {:?}", trigger);
                                relay_status.store(RELAY_HANDSHAKING, Ordering::SeqCst);
                                *trigger_mode.lock().unwrap() = Some(trigger.clone());

                                // Rewrite tunnel port and forward to client
                                if trigger.port > 0 {
                                    let conn_opt = tunnel_connector.lock().unwrap().take();
                                    let mut output = Vec::new();
                                    if let Some(connector) = conn_opt {
                                        match TcpListener::bind("127.0.0.1:0") {
                                            Ok(listener) => {
                                                let relay_port = listener.local_addr().unwrap().port();
                                                let rewritten = trigger.full_line.replace(
                                                    &format!(":{}", trigger.port),
                                                    &format!(":{}", relay_port),
                                                );
                                                output.extend_from_slice(rewritten.as_bytes());
                                                output.push(b'\n');

                                                let uid = trigger.unique_id.clone();
                                                let tgt_port = trigger.port;
                                                thread::spawn(move || {
                                                    relay_tunnel(listener, uid, tgt_port, connector);
                                                });
                                            }
                                            Err(e) => {
                                                debug!("Relay: failed to bind tunnel: {}", e);
                                                output.extend_from_slice(data);
                                            }
                                        }
                                    } else {
                                        output.extend_from_slice(data);
                                    }
                                    let mut w = client_out.lock().unwrap();
                                    w.write_all(&output).ok();
                                    w.flush().ok();
                                } else {
                                    let mut w = client_out.lock().unwrap();
                                    w.write_all(data).ok();
                                    w.flush().ok();
                                }
                                detect_buf.clear();

                                // Perform relay handshake in a background thread
                                let handshake_relay_status = relay_status.clone();
                                let handshake_stdin = stdin_buffer.clone();
                                let handshake_stdout = stdout_buffer.clone();
                                let handshake_server_in = server_in.clone();
                                let handshake_client_out = client_out.clone();
                                thread::spawn(move || {
                                    // Handshake: forward action/config between client and server
                                    let ok = do_relay_handshake(
                                        &handshake_stdin,
                                        &handshake_stdout,
                                        &handshake_server_in,
                                        &handshake_client_out,
                                    );
                                    if ok {
                                        handshake_relay_status.store(RELAY_TRANSFERRING, Ordering::SeqCst);
                                    } else {
                                        handshake_relay_status.store(RELAY_STANDBY, Ordering::SeqCst);
                                    }
                                });
                                continue;
                            }
                        } else if status == RELAY_HANDSHAKING {
                            stdout_buffer.add_buffer(data);
                            continue;
                        }

                        // Detect EXIT/FAIL during transfer → reset to standby
                        if status == RELAY_TRANSFERRING {
                            let text = String::from_utf8_lossy(data);
                            if text.contains("#EXIT:") || text.contains("#FAIL:") || text.contains("#fail:") {
                                relay_status.store(RELAY_STANDBY, Ordering::SeqCst);
                            }
                        }

                        // Trace log server output (ttocli), matching Go's relay.
                        if let Some(logger) = &trace_logger {
                            let logged = logger.write_trace_log(data, "ttocli");
                            let mut w = client_out.lock().unwrap();
                            w.write_all(&logged).ok();
                            w.flush().ok();
                        } else {
                            let mut w = client_out.lock().unwrap();
                            w.write_all(data).ok();
                            w.flush().ok();
                        }


                    }
                    Err(_) => break,
                }
                if closed.load(Ordering::SeqCst) {
                    break;
                }
            }
        });

        // Keep the relay running until closed
        while !self.closed.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(())
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Parsed relay action from a #ACT: line
struct RelayAction {
    confirm: bool,
}

/// Parse a relay action from a #ACT: line.
fn parse_relay_action(line: &[u8]) -> Option<RelayAction> {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    // Format: #ACT:<base64 encoded JSON>
    let idx = text.find("#ACT:")?;
    let encoded = &text[idx + 5..];
    let encoded = encoded.trim();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let confirm = json.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false);
    Some(RelayAction { confirm })
}

/// Standalone relay handshake: reads action from client stdin buffer,
/// forwards to server, reads server action, forwards to client.
/// If confirm=true, also exchanges Config.
fn do_relay_handshake(
    stdin: &NrzszBuffer,
    stdout: &NrzszBuffer,
    server_in: &SharedWriter,
    client_out: &SharedWriter,
) -> bool {
    // Read Action from client stdin buffer
    let client_line = match stdin.read_line(true, None) {
        Ok(line) => line,
        Err(e) => {
            debug!("Relay: failed to read client action: {}", e);
            return false;
        }
    };

    // Parse client action
    let client_action = match parse_relay_action(&client_line) {
        Some(a) => a,
        None => {
            debug!("Relay: failed to parse client action");
            return false;
        }
    };

    // Forward client action to server
    {
        let mut w = server_in.lock().unwrap();
        if w.write_all(&client_line).is_err() {
            return false;
        }
        w.write_all(b"\n").ok();
        w.flush().ok();
    }

    // Read server response action
    let server_line = match stdout.read_line(true, None) {
        Ok(line) => line,
        Err(e) => {
            debug!("Relay: failed to read server action: {}", e);
            return false;
        }
    };

    // Forward server action to client
    {
        let mut w = client_out.lock().unwrap();
        if w.write_all(&server_line).is_err() {
            return false;
        }
        w.write_all(b"\n").ok();
        w.flush().ok();
    }

    // If client confirms (confirm=true), exchange Config
    if client_action.confirm {
        let client_cfg = match stdin.read_line(true, None) {
            Ok(line) => line,
            Err(e) => {
                debug!("Relay: failed to read client config: {}", e);
                return false;
            }
        };
        {
            let mut w = server_in.lock().unwrap();
            w.write_all(&client_cfg).ok();
            w.write_all(b"\n").ok();
            w.flush().ok();
        }

        let server_cfg = match stdout.read_line(true, None) {
            Ok(line) => line,
            Err(e) => {
                debug!("Relay: failed to read server config: {}", e);
                return false;
            }
        };
        {
            let mut w = client_out.lock().unwrap();
            w.write_all(&server_cfg).ok();
            w.write_all(b"\n").ok();
            w.flush().ok();
        }
    }

    true
}

/// Detect a NRZSZ:TRANSFER trigger in the buffer.
/// Returns the trigger if found, consuming the buffer up to the trigger.
fn detect_trigger_in_buffer(buf: &[u8]) -> Option<crate::filter::NrzszTrigger> {
    // Find the last occurrence of ::NRZSZ:TRANSFER: in the buffer
    let text = String::from_utf8_lossy(buf);
    let idx = text.rfind("::TRZSZ:TRANSFER:").or_else(|| text.rfind("::NRZSZ:TRANSFER:"))?;
    let line = &text[idx..];
    // Take up to the newline
    let line = line.split('\n').next().unwrap_or(line);
    parse_trigger(line)
}

/// Relay a tunnel connection from client to server.
fn relay_tunnel(
    listener: TcpListener,
    uid: String,
    target_port: u16,
    connector: Box<TunnelConnector>,
) {
    if let Ok((mut client_conn, _)) = listener.accept() {
            // Client sends its hello (using relay port). Read it line-at-a-time:
            // a fixed-size read can miss the trailing `\n`, leaving a stray
            // newline in the stream that desyncs the next protocol frame.
            let relay_port = listener.local_addr().unwrap().port();
            let (client_hello_relay, server_hello_relay) = get_hello_constant(&uid, relay_port);
            let (client_hello_target, server_hello_target) = get_hello_constant(&uid, target_port);

            let received = match read_line_from_stream(&mut client_conn) {
                Ok(line) => line.trim().to_string(),
                Err(_) => return,
            };
            if received != client_hello_relay {
                debug!("Relay: client hello mismatch: {:?} != {:?}", received, client_hello_relay);
                return;
            }

            let mut server_conn = match connector(target_port) {
                Ok(c) => c,
                Err(_) => return,
            };
            if server_conn.write_all(client_hello_target.as_bytes()).is_err() {
                return;
            }
            server_conn.write_all(b"\n").ok();
            server_conn.flush().ok();

            let response = match read_line_from_stream(&mut server_conn) {
                Ok(line) => line.trim().to_string(),
                Err(_) => return,
            };
            if response != server_hello_target {
                debug!("Relay: server hello mismatch: {:?} != {:?}", response, server_hello_target);
                return;
            }

            client_conn.write_all(server_hello_relay.as_bytes()).ok();
            client_conn.write_all(b"\n").ok();
            client_conn.flush().ok();

            // Bidirectional bridge
            let mut client_clone = match client_conn.try_clone() {
                Ok(c) => c,
                Err(_) => return,
            };
            let server_clone = match server_conn.try_clone() {
                Ok(c) => c,
                Err(_) => return,
            };

            debug!("Relay: tunnel bridge established, starting bidirectional pump");

            let (tx1, rx1) = mpsc::channel::<Vec<u8>>();
            let (tx2, rx2) = mpsc::channel::<Vec<u8>>();

            thread::spawn(move || {
                let mut server_out = server_conn;
                let mut buf = [0u8; 32768];
                loop {
                    match server_out.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            debug!("Relay: pump server->client {} bytes: {:?}", n, String::from_utf8_lossy(&buf[..n]).chars().take(120).collect::<String>());
                            if tx2.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });

            thread::spawn(move || {
                for data in rx2 {
                    if client_clone.write_all(&data).is_err() {
                        break;
                    }
                    client_clone.flush().ok();
                }
            });

            thread::spawn(move || {
                let mut server_in = server_clone;
                for data in rx1 {
                    if server_in.write_all(&data).is_err() {
                        break;
                    }
                    server_in.flush().ok();
                }
            });

            loop {
                let mut buf = [0u8; 32768];
                match client_conn.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        debug!("Relay: pump client->server {} bytes: {:?}", n, String::from_utf8_lossy(&buf[..n]).chars().take(120).collect::<String>());
                        if tx1.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
    }
}

/// Helper for setting up a relay between two streams.
pub fn setup_relay(
    client_in: Box<dyn Read + Send>,
    client_out: Box<dyn Write + Send>,
    server_in: Box<dyn Write + Send>,
    server_out: Box<dyn Read + Send>,
) -> Result<NrzszRelay> {
    Ok(NrzszRelay::new(client_in, client_out, server_in, server_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_creation() {
        let relay = NrzszRelay::new(
            Box::new(std::io::empty()),
            Box::new(std::io::sink()),
            Box::new(std::io::sink()),
            Box::new(std::io::empty()),
        );
        assert_eq!(relay.get_status(), RELAY_STANDBY);
    }

    #[test]
    fn test_detect_trigger() {
        let buf = b"some output\n\x1b[s::NRZSZ:TRANSFER:R:1.2.0:1234567890123:9090\r\nmore\n";
        let trigger = detect_trigger_in_buffer(buf);
        assert!(trigger.is_some());
        assert_eq!(trigger.unwrap().mode, 'R');
    }

    #[test]
    fn test_detect_trigger_trzsz_namespace() {
        let buf = b"some output\n\x1b[s::TRZSZ:TRANSFER:D:1.2.0:1234567890123:9090\r\nmore\n";
        let trigger = detect_trigger_in_buffer(buf).unwrap();
        assert_eq!(trigger.mode, 'D');
        assert_eq!(trigger.port, 9090);



    }

    #[test]
    fn test_detect_trigger_no_match() {
        let buf = b"just plain output\r\n";
        assert!(detect_trigger_in_buffer(buf).is_none());
    }
}