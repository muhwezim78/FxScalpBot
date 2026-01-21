//! Kill Switch - Emergency shutdown mechanism
//! 
//! When triggered, immediately:
//! 1. Cancels all pending orders
//! 2. Closes all positions at market
//! 3. Disables further order submission
//! 4. Logs the event

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, warn};

/// Reasons for triggering the kill switch
#[derive(Debug, Clone, PartialEq)]
pub enum KillReason {
    DailyLossLimit,
    DrawdownLimit,
    ManualStop,
    LatencySpike,
    BrokerDisconnect,
    SpreadAnomaly,
    UnknownError(String),
}

impl std::fmt::Display for KillReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KillReason::DailyLossLimit => write!(f, "Daily loss limit exceeded"),
            KillReason::DrawdownLimit => write!(f, "Drawdown limit exceeded"),
            KillReason::ManualStop => write!(f, "Manual stop requested"),
            KillReason::LatencySpike => write!(f, "Network latency spike detected"),
            KillReason::BrokerDisconnect => write!(f, "Broker connection lost"),
            KillReason::SpreadAnomaly => write!(f, "Abnormal spread detected"),
            KillReason::UnknownError(e) => write!(f, "Unknown error: {}", e),
        }
    }
}

/// Kill switch event record
#[derive(Debug, Clone)]
pub struct KillEvent {
    pub reason: KillReason,
    pub timestamp_ms: u64,
    pub account_state: Option<String>,
}

/// Kill Switch - atomic emergency stop
pub struct KillSwitch {
    triggered: AtomicBool,
    reason: Mutex<Option<KillReason>>,
    event_history: Mutex<Vec<KillEvent>>,
}

impl KillSwitch {
    pub fn new() -> Self {
        Self {
            triggered: AtomicBool::new(false),
            reason: Mutex::new(None),
            event_history: Mutex::new(Vec::new()),
        }
    }
    
    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }
    
    /// Trigger the kill switch
    /// This is an atomic operation - once triggered, cannot be undone without restart
    pub fn trigger(&self, reason: KillReason) {
        // Only trigger once
        if self.triggered.compare_exchange(
            false, 
            true, 
            Ordering::SeqCst, 
            Ordering::SeqCst
        ).is_ok() {
            error!(
                reason = %reason,
                "KILL SWITCH TRIGGERED"
            );
            
            *self.reason.lock().unwrap() = Some(reason.clone());
            
            // Record event
            let event = KillEvent {
                reason,
                timestamp_ms: Self::now_ms(),
                account_state: None,
            };
            self.event_history.lock().unwrap().push(event);
        } else {
            warn!("Kill switch already triggered, ignoring additional trigger");
        }
    }
    
    /// Check if kill switch is active
    #[inline]
    pub fn is_active(&self) -> bool {
        self.triggered.load(Ordering::SeqCst)
    }
    
    /// Get the reason for triggering (if triggered)
    pub fn get_reason(&self) -> Option<KillReason> {
        self.reason.lock().unwrap().clone()
    }
}

impl Default for KillSwitch {
    fn default() -> Self {
        Self::new()
    }
}
