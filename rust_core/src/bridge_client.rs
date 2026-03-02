//! Bridge Client - ZeroMQ-based client for Python TradingService
//!
//! Topology:
//!   - SUB  (tcp://127.0.0.1:5556) ← Python PUB:  Ticks + Account updates
//!   - REQ  (tcp://127.0.0.1:5557) → Python REP:  Synchronous order requests
//!   - PULL (tcp://127.0.0.1:5558) ← Python PUSH: Async execution results
//!
//! The tokio runtime is spawned in a background thread. The main loop
//! continues to use synchronous mpsc channels — zero changes needed in main.rs.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};
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

/// Async execution result pushed from Python after background order fill
#[derive(Debug, Deserialize)]
pub struct AsyncExecResult {
    pub req_id: String,
    pub status: String,
    pub data: Option<serde_json::Value>,
    pub message: Option<String>,
}

/// Internal request sent from main thread → tokio REQ socket
struct OrderRequest {
    payload: String,
    response_tx: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
}

pub struct BridgeClient {
    /// Send order requests to the tokio thread for REQ/REP dispatch
    order_tx: Sender<OrderRequest>,
    /// Receive async execution results (from PULL socket)
    pub async_result_rx: Receiver<AsyncExecResult>,
    connected: Arc<AtomicBool>,
}

impl BridgeClient {
    /// Connect to the Python TradingService via ZeroMQ.
    ///
    /// Spawns a background tokio runtime that manages:
    ///   - SUB socket for ticks (port 5556)
    ///   - REQ socket for orders (port 5557)
    ///   - PULL socket for async results (port 5558)
    pub fn connect(addr: &str) -> std::io::Result<(Self, Receiver<BridgeMessage>)> {
        let connected = Arc::new(AtomicBool::new(true));
        let connected_clone = connected.clone();

        // Channels: main loop ↔ tokio thread
        let (msg_tx, msg_rx) = channel::<BridgeMessage>();           // ticks/account
        let (order_tx, order_rx) = channel::<OrderRequest>();         // order requests
        let (async_tx, async_rx) = channel::<AsyncExecResult>();      // async results

        // Parse base address (e.g. "127.0.0.1" from "127.0.0.1:5555")
        let base_host = addr.split(':').next().unwrap_or("127.0.0.1").to_string();

        // Spawn the tokio runtime in a dedicated OS thread
        thread::Builder::new()
            .name("zmq-bridge".to_string())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("Failed to build tokio runtime for ZMQ bridge");

                rt.block_on(async move {
                    use zeromq::prelude::*;
                    use zeromq::{SubSocket, ReqSocket, PullSocket};

                    // --- SUB socket (ticks + account) ---
                    let mut sub_socket = SubSocket::new();
                    let sub_addr = format!("tcp://{}:5556", base_host);
                    if let Err(e) = sub_socket.connect(&sub_addr).await {
                        error!("ZMQ SUB connect failed ({}): {}", sub_addr, e);
                        connected_clone.store(false, Ordering::SeqCst);
                        return;
                    }
                    // Subscribe to all messages
                    if let Err(e) = sub_socket.subscribe("").await {
                        error!("ZMQ SUB subscribe failed: {}", e);
                    }
                    info!("ZMQ SUB connected to {} (ticks/account)", sub_addr);

                    // --- PULL socket (async execution results) ---
                    let mut pull_socket = PullSocket::new();
                    let pull_addr = format!("tcp://{}:5558", base_host);
                    if let Err(e) = pull_socket.connect(&pull_addr).await {
                        error!("ZMQ PULL connect failed ({}): {}", pull_addr, e);
                    }
                    info!("ZMQ PULL connected to {} (async results)", pull_addr);

                    // --- REQ socket (order requests) ---
                    let mut req_socket = ReqSocket::new();
                    let req_addr = format!("tcp://{}:5557", base_host);
                    if let Err(e) = req_socket.connect(&req_addr).await {
                        error!("ZMQ REQ connect failed ({}): {}", req_addr, e);
                        connected_clone.store(false, Ordering::SeqCst);
                        return;
                    }
                    info!("ZMQ REQ connected to {} (orders)", req_addr);

                    // --- Event loop ---
                    loop {
                        tokio::select! {
                            // 1. Incoming ticks/account from SUB
                            msg = sub_socket.recv() => {
                                match msg {
                                    Ok(zmq_msg) => {
                                        let bytes = zmq_msg.get(0).map(|f| f.to_vec()).unwrap_or_default();
                                        let text = String::from_utf8_lossy(&bytes);
                                        if let Ok(bridge_msg) = serde_json::from_str::<BridgeMessage>(&text) {
                                            let _ = msg_tx.send(bridge_msg);
                                        }
                                    }
                                    Err(e) => {
                                        error!("ZMQ SUB recv error: {}", e);
                                    }
                                }
                            }

                            // 2. Async execution results from PULL
                            msg = pull_socket.recv() => {
                                match msg {
                                    Ok(zmq_msg) => {
                                        let bytes = zmq_msg.get(0).map(|f| f.to_vec()).unwrap_or_default();
                                        let text = String::from_utf8_lossy(&bytes);
                                        if let Ok(result) = serde_json::from_str::<AsyncExecResult>(&text) {
                                            let _ = async_tx.send(result);
                                        }
                                    }
                                    Err(e) => {
                                        error!("ZMQ PULL recv error: {}", e);
                                    }
                                }
                            }

                            // 3. Outgoing order requests via REQ
                            _ = tokio::task::yield_now() => {
                                // Check for pending order requests (non-blocking)
                                if let Ok(order_req) = order_rx.try_recv() {
                                    use zeromq::ZmqMessage;
                                    let zmq_msg = ZmqMessage::from(order_req.payload.clone());
                                    match req_socket.send(zmq_msg).await {
                                        Ok(_) => {
                                            // Wait for REP
                                            match tokio::time::timeout(
                                                std::time::Duration::from_secs(5),
                                                req_socket.recv()
                                            ).await {
                                                Ok(Ok(reply)) => {
                                                    let bytes = reply.get(0).map(|f| f.to_vec()).unwrap_or_default();
                                                    let text = String::from_utf8_lossy(&bytes);
                                                    if let Ok(resp) = serde_json::from_str::<BridgeResponse>(&text) {
                                                        let result = if resp.status == "ok" || resp.status == "pending" {
                                                            let mut val = resp.data.unwrap_or(serde_json::Value::Null);
                                                            // Inject status into the value for caller inspection
                                                            if let serde_json::Value::Object(ref mut map) = val {
                                                                map.insert("status".to_string(), serde_json::Value::String(resp.status.clone()));
                                                            } else {
                                                                val = serde_json::json!({"status": resp.status});
                                                            }
                                                            Ok(val)
                                                        } else {
                                                            Err(resp.message.unwrap_or("unknown error".to_string()))
                                                        };
                                                        let _ = order_req.response_tx.send(result);
                                                    } else {
                                                        let _ = order_req.response_tx.send(Err(format!("Invalid JSON response: {}", text)));
                                                    }
                                                }
                                                Ok(Err(e)) => {
                                                    let _ = order_req.response_tx.send(Err(format!("ZMQ REQ recv error: {}", e)));
                                                }
                                                Err(_) => {
                                                    let _ = order_req.response_tx.send(Err("ZMQ REQ timeout (5s)".to_string()));
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let _ = order_req.response_tx.send(Err(format!("ZMQ REQ send error: {}", e)));
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            })
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        info!("ZMQ BridgeClient initialized (SUB:5556, REQ:5557, PULL:5558)");

        let client = Self {
            order_tx,
            async_result_rx: async_rx,
            connected,
        };

        Ok((client, msg_rx))
    }

    /// Synchronous request-response for account sync or orders.
    /// Sends via REQ socket (in tokio thread), blocks caller until response arrives.
    pub fn request(&mut self, method: &str, params: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        if !self.is_connected() {
            return Err("Bridge is disconnected".to_string());
        }

        let req_id = Uuid::new_v4().to_string();
        let req = BridgeInfo {
            req_id: req_id.clone(),
            method: method.to_string(),
            params,
        };

        let payload = serde_json::to_string(&req).map_err(|e| e.to_string())?;

        // Create a oneshot-style channel for this specific request
        let (resp_tx, resp_rx) = channel();

        self.order_tx.send(OrderRequest {
            payload,
            response_tx: resp_tx,
        }).map_err(|_| "Failed to send order to ZMQ thread".to_string())?;

        // Block until the tokio thread sends back the response (with timeout)
        match resp_rx.recv_timeout(std::time::Duration::from_secs(6)) {
            Ok(result) => result,
            Err(_) => Err(format!("Request timeout for method {}", method)),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
