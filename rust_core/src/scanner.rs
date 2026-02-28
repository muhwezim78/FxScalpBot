use std::sync::{Arc, RwLock};
use tracing::{info, error};

use fx_scalp_core::{
    AppState, Direction, StateEvent,
    bridge_client::BridgeClient
};

/// Scans for new trading opportunities across all configured symbols.
///
/// This checks global and per-symbol capacity limits first. If capacity exists,
/// it requests momentum and reversion analysis from the Python strategy service.
pub fn scan_for_opportunities(app_state: &Arc<RwLock<AppState>>, bridge: &mut BridgeClient) {
    let (symbols, capacity_data) = {
        if let Ok(state) = app_state.read() {
            let symbols = state.config.market.symbols.clone();
            
            // Count active trades per symbol
            let mut counts = std::collections::HashMap::new();
            for t in &state.active_trades {
                *counts.entry(t.symbol.clone()).or_insert(0) += 1;
            }
            
            let global_count = state.active_trades.len() as u32;
            (symbols, (counts, global_count, state.config.account.max_parallel_trades, state.config.account.max_trades_per_symbol))
        } else {
            error!("Poisoned RwLock on app_state during scan init");
            return;
        }
    };
    
    let (counts, global_count, max_global, max_per_symbol) = capacity_data;

    // Only scan if we have global capacity
    if global_count < max_global {
        for symbol in symbols {
            // Check per-symbol capacity
            let symbol_count = *counts.get(&symbol).unwrap_or(&0);
            if symbol_count >= max_per_symbol {
               continue; 
            }

            // Get ticks for analysis
            let ticks_json = {
                if let Ok(state) = app_state.read() {
                    if let Some(buffer) = state.tick_ingestion.get_buffer(&symbol) {
                        if buffer.len() >= 50 {
                            Some(serde_json::to_value(buffer.latest(50)).unwrap_or(serde_json::Value::Null))
                        } else { None }
                    } else { None }
                } else {
                    error!("Poisoned RwLock on app_state during tick read");
                    None
                }
            };

            if let Some(ticks) = ticks_json {
                 let params = serde_json::json!({ "symbol": symbol, "ticks": ticks });
                 
                 // 3a. Momentum Scan
                 let mut signal_found = false;
                 let mut found_dir = Direction::Long;
                 let mut found_type = fx_scalp_core::state_machine::SignalType::Momentum;
                 
                 if let Ok(resp) = bridge.request("analyze_momentum", Some(params.clone())) {
                    if let Ok(signal) = serde_json::from_value::<fx_scalp_core::MomentumSignal>(resp) {
                        if signal.detected {
                            signal_found = true;
                            found_dir = if signal.direction > 0 { Direction::Long } else { Direction::Short };
                            found_type = fx_scalp_core::state_machine::SignalType::Momentum;
                            info!(symbol = %symbol, "Momentum Signal found! Spawning new trade...");
                        }
                    }
                 }

                 // 3b. Reversion Scan (if no Momentum)
                 if !signal_found {
                    if let Ok(resp) = bridge.request("analyze_reversion", Some(params)) {
                        if let Ok(signal) = serde_json::from_value::<fx_scalp_core::ReversionSignal>(resp) {
                            if signal.detected {
                                signal_found = true;
                                found_dir = if signal.direction > 0 { Direction::Long } else { Direction::Short };
                                found_type = fx_scalp_core::state_machine::SignalType::Reversion;
                                info!(symbol = %symbol, "Reversion Signal found! Spawning new trade...");
                            }
                        }
                    }
                 }

                 // 3c. Spawn ActiveTrade
                 if signal_found {
                    if let Ok(mut state) = app_state.write() {
                        state.trade_id_counter += 1;
                        let new_id = state.trade_id_counter;
                        
                        let mut new_trade = fx_scalp_core::ActiveTrade::new(new_id, symbol.clone());
                        // Initial transition to Qualifying
                        let event = match found_type {
                            fx_scalp_core::state_machine::SignalType::Momentum => StateEvent::MomentumDetected(symbol.clone(), found_dir),
                            fx_scalp_core::state_machine::SignalType::Reversion => StateEvent::ReversionDetected(symbol.clone(), found_dir),
                        };
                        let _ = new_trade.state_machine.process_event(event);
                        
                        state.active_trades.push(new_trade);
                    } else {
                        error!("Poisoned RwLock on app_state during trade spawn");
                    }
                 }
            }
        }
    }
}
