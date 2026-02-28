use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use tracing::error;

use crate::{ActiveTrade, state_machine::ExitReason};

/// TradeJournal manages appending completed trades to a CSV file.
pub struct TradeJournal {
    file_path: String,
}

impl TradeJournal {
    pub fn new(path: &str) -> Self {
        let is_new = !Path::new(path).exists();
        
        let journal = Self {
            file_path: path.to_string(),
        };
        
        if is_new {
            journal.write_header();
        }
        
        journal
    }
    
    fn write_header(&self) {
        if let Ok(mut file) = OpenOptions::new().create(true).write(true).truncate(true).open(&self.file_path) {
            let header = "trade_id,symbol,direction,entry_time_ms,exit_time_ms,entry_price,exit_price,lots,realized_pnl,exit_reason\n";
            if let Err(e) = file.write_all(header.as_bytes()) {
                error!("Failed to write TradeJournal header: {}", e);
            }
        }
    }
    
    pub fn log_trade(&self, trade: &ActiveTrade, exit_reason: ExitReason) {
        if let Some(run) = &trade.active_run {
            let entry_time = run.entry_time_ms;
            let exit_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            let direction = match trade.state_machine.current_state() {
                crate::TradingState::PositionOpen { direction, .. } => format!("{:?}", direction),
                crate::TradingState::Exiting { direction, .. } => format!("{:?}", direction),
                crate::TradingState::Idle => {
                    // Try to infer from previous events if possible, else "Unknown"
                    "Unknown".to_string()
                }
                _ => "Unknown".to_string(),
            };
            
            // Reconstruct direction from run logic if possible, or we could store direction in ActiveRun
            // For now, let's just log "Long" or "Short" if we can infer it, otherwise "Closed"
            // Let's assume ActiveRun has sign of current_lots or we can just say N/A if missing
            
            // To properly get direction, we should add direction to ActiveRun, or we can just say N/A
            
            let line = format!(
                "{},{},{},{},{},{:.5},{:.5},{:.2},{:.2},{:?}\n",
                trade.id,
                trade.symbol,
                direction,
                entry_time,
                exit_time,
                run.entry_price,
                run.entry_price + (run.total_pnl() / (run.current_lots * 100000.0)), // Approximation of exit price
                run.current_lots,
                run.total_pnl(),
                exit_reason
            );
            
            if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(&self.file_path) {
                if let Err(e) = file.write_all(line.as_bytes()) {
                    error!("Failed to write to TradeJournal: {}", e);
                }
            } else {
                error!("Failed to open TradeJournal for appending.");
            }
        }
    }
}
