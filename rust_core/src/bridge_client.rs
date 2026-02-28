//! Bridge Client - TCP client for Python TradingService
//!
//! Synchronizes:
//! - Account balance/equity
//! - Real-time ticks
//! - Order execution requests

use std::io::Write;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub req_id: String,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct BridgeResponse {
    pub req_id: Option<String>,
    pub status: String,
    pub data: Option<serde_json::Value>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum BridgeMessage {
    #[serde(rename = "tick")]
    Tick { data: serde_json::Value },
    #[serde(rename = "account")]
    Account { data: serde_json::Value },
}

pub struct BridgeClient {
    stream: TcpStream,
    response_rx: Option<Receiver<Result<BridgeResponse, String>>>,
    connected: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl BridgeClient {
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        info!("Connected to Python TradingService at {}", addr);
        Ok(Self { 
            stream,
            response_rx: None,
            connected: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        })
    }

    /// Spawns a background thread to listen for incoming ticks/updates
    /// returns a Receiver for async messages (ticks/account updates)
    pub fn start_listener(&mut self) -> Receiver<BridgeMessage> {
        let (msg_tx, msg_rx) = channel();
        let (resp_tx, resp_rx) = channel();
        
        self.response_rx = Some(resp_rx);
        
        let stream_clone = self.stream.try_clone().expect("Failed to clone bridge stream");
        let connected_flag = self.connected.clone();
        
        thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stream_clone);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        error!("Bridge connection closed by remote");
                        connected_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        
                        if let Ok(msg) = serde_json::from_str::<BridgeMessage>(trimmed) {
                            let _ = msg_tx.send(msg);
                        } else if let Ok(resp) = serde_json::from_str::<BridgeResponse>(trimmed) {
                            if resp.status == "ok" {
                                let _ = resp_tx.send(Ok(resp));
                            } else {
                                let _ = resp_tx.send(Err(resp.message.unwrap_or("unknown error".to_string())));
                            }
                        } else {
                            warn!("Unexpected message format from bridge: {}", trimmed);
                        }
                    }
                    Err(e) => {
                        error!("Bridge read error: {}", e);
                        break;
                    }
                }
            }
            error!("Bridge listener thread exited");
            connected_flag.store(false, std::sync::atomic::Ordering::SeqCst);
        });
        
        msg_rx
    }

    /// Synchronous request-response for account sync or orders
    /// Blocks until the listener thread receives the response, with a timeout
    pub fn request(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        if !self.is_connected() {
            return Err("Bridge is disconnected".to_string());
        }
        
        let rx = self.response_rx.as_ref().ok_or("Listener not started")?;
        
        let req_id = Uuid::new_v4().to_string();
        let req = BridgeInfo {
            req_id: req_id.clone(),
            method: method.to_string(),
            params,
        };
        
        let msg = serde_json::to_string(&req).map_err(|e| e.to_string())? + "\n";
        self.stream.write_all(msg.as_bytes()).map_err(|e| {
            self.connected.store(false, std::sync::atomic::Ordering::SeqCst);
            e.to_string()
        })?;
        
        // Wait for the response from the background listener thread with timeout
        // Since it's a single channel, we drain until we find our req_id or timeout
        let timeout = Duration::from_secs(5);
        let start = std::time::Instant::now();
        
        while start.elapsed() < timeout {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(resp)) => {
                    if resp.req_id.as_ref() == Some(&req_id) {
                        return Ok(resp.data.unwrap_or(serde_json::Value::Null));
                    }
                    // If it's not our req_id, we drop it (or we could queue it if we had a proper router)
                    // For synchronous calls in a single thread, dropping is mostly fine.
                }
                Ok(Err(e)) => return Err(e),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.connected.store(false, std::sync::atomic::Ordering::SeqCst);
                    return Err("Bridge disconnected during request".to_string());
                }
            }
        }
        
        Err(format!("Request timeout for method {}", method))
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }
}
