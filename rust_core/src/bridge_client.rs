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

#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct BridgeResponse {
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
    response_rx: Option<Receiver<Result<serde_json::Value, String>>>,
}

impl BridgeClient {
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        info!("Connected to Python TradingService at {}", addr);
        Ok(Self { 
            stream,
            response_rx: None,
        })
    }

    /// Spawns a background thread to listen for incoming ticks/updates
    /// returns a Receiver for async messages (ticks/account updates)
    pub fn start_listener(&mut self) -> Receiver<BridgeMessage> {
        let (msg_tx, msg_rx) = channel();
        let (resp_tx, resp_rx) = channel();
        
        self.response_rx = Some(resp_rx);
        
        let stream_clone = self.stream.try_clone().expect("Failed to clone bridge stream");
        
        thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stream_clone);
            let mut line = String::new();
            
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        error!("Bridge connection closed by remote");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        
                        if let Ok(msg) = serde_json::from_str::<BridgeMessage>(trimmed) {
                            let _ = msg_tx.send(msg);
                        } else if let Ok(resp) = serde_json::from_str::<BridgeResponse>(trimmed) {
                            if resp.status == "ok" {
                                let _ = resp_tx.send(Ok(resp.data.unwrap_or(serde_json::Value::Null)));
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
        });
        
        msg_rx
    }

    /// Synchronous request-response for account sync or orders
    /// Blocks until the listener thread receives the response
    pub fn request(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        let rx = self.response_rx.as_ref().ok_or("Listener not started")?;
        
        let req = BridgeInfo {
            method: method.to_string(),
            params,
        };
        
        let msg = serde_json::to_string(&req).map_err(|e| e.to_string())? + "\n";
        self.stream.write_all(msg.as_bytes()).map_err(|e| e.to_string())?;
        
        // Wait for the response from the background listener thread
        rx.recv().map_err(|e| e.to_string())?
    }
}
