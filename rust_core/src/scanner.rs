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
            
            // Count active trades per symbol (only trades that are actually in play)
            let mut counts = std::collections::HashMap::new();
            let mut symbol_pnl = std::collections::HashMap::<String, f64>::new();
            for t in &state.active_trades {
                let is_active = matches!(
                    t.state_machine.current_state(), 
                    fx_scalp_core::TradingState::PositionOpen { .. } 
                    | fx_scalp_core::TradingState::Exiting { .. } 
                    | fx_scalp_core::TradingState::EntryReady { .. }
                );
                if is_active {
                    *counts.entry(t.symbol.clone()).or_insert(0u32) += 1;
                }
                // Track unrealized P&L for pyramiding check
                if let Some(run) = &t.active_run {
                    *symbol_pnl.entry(t.symbol.clone()).or_insert(0.0) += run.total_pnl();
                }
            }
            
            let global_count = counts.values().sum::<u32>();
            let cooldowns = state.execution_cooldowns.clone();
            (symbols, (counts, symbol_pnl, cooldowns, global_count, state.config.account.max_parallel_trades, state.config.account.max_trades_per_symbol))
        } else {
            error!("Poisoned RwLock on app_state during scan init");
            return;
        }
    };
    
    let (counts, symbol_pnl, cooldowns, global_count, max_global, max_per_symbol) = capacity_data;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Only scan if we have global capacity
    if global_count < max_global {
        for symbol in symbols {
            // Check execution cooldown (e.g. after "No money" rejection)
            if let Some(&cooldown_until) = cooldowns.get(&symbol) {
                if now < cooldown_until {
                    continue; // Symbol is in cooldown, skip scanning
                }
            }
            
            // Check per-symbol: block new entries if existing trades are in LOSS
            let symbol_count = *counts.get(&symbol).unwrap_or(&0);
            let symbol_total_pnl = symbol_pnl.get(&symbol).copied().unwrap_or(0.0);
            
            // Rule: If any existing trades on this symbol and they are losing, do NOT pile on
            if symbol_count > 0 && symbol_total_pnl < 0.0 {
                continue; // Existing trades are underwater — don't add more
            }
            
            // Pyramiding: allow above max if existing trades are in good profit ($5+)
            let is_profitable = symbol_total_pnl > 5.0;
            if symbol_count >= max_per_symbol && !is_profitable {
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
