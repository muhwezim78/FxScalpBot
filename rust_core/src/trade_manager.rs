use std::sync::{Arc, RwLock};
use tracing::{info, warn, error};

use fx_scalp_core::{
    AppState, TradingState, Direction, StateEvent, Account, TradingRun,
    bridge_client::BridgeClient
};

/// Iterates over all active trades and updates their state machine based on market conditions,
/// risk parameters, and bridge actions (order execution, position closure).
pub fn update_active_trades(app_state: &Arc<RwLock<AppState>>, bridge: &mut BridgeClient) {
    let (trade_count, risk_enforcer) = {
        if let Ok(state) = app_state.read() {
            (state.active_trades.len(), state.risk_enforcer.clone())
        } else {
            error!("Poisoned RwLock on app_state during management start");
            (0, app_state.read().unwrap().risk_enforcer.clone())
        }
    };
    
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    
    for i in 0..trade_count {
         // Extract Trade State & Market Data
         let (current_trade_state, tick_info, account_snapshot, trade_symbol) = {
            if let Ok(state) = app_state.read() {
                if i >= state.active_trades.len() { break; } 
            
                // Copy account params first to avoid borrow conflicts
                let balance = state.account_balance;
                let equity = state.account_equity;
                let daily_pnl = state.daily_pnl;
                let open_positions = state.open_positions_count;

                let trade = &state.active_trades[i];
                let symbol = trade.symbol.clone();
                let tm = trade.state_machine.current_state().clone();
                
                let (tick, spread, avg_spread, latency) = if let Some(buf) = state.tick_ingestion.get_buffer(&symbol) {
                     (
                        buf.latest(1).first().map(|t| (*t).clone()), 
                        buf.current_spread().unwrap_or(0.0), 
                        buf.average_spread(), 
                        buf.current_latency_ms().unwrap_or(0)
                    )
                } else { (None, 0.0, 0.0, 0) };
                
                // Build Account snapshot for RiskEnforcer
                let acc = Account {
                    balance,
                    equity,
                    daily_pnl,
                    open_positions,
                    current_spread: spread,
                    avg_spread,
                    current_latency_ms: latency as u64,
                };
                
                (tm, tick, acc, symbol)
            } else {
                error!("Poisoned RwLock on app_state during trade processing");
                break;
            }
         };

         // Process State Logic and accumulate events
         let next_event: Option<StateEvent> = match current_trade_state {
             TradingState::Qualifying { signal_type, .. } => {
                 // Check Risk Enforcer
                 if tick_info.is_some() {
                    let state = app_state.read().unwrap();
                    let sym_count = state.active_trades.iter().filter(|t| t.symbol == trade_symbol).count() as u32;
                    
                    match risk_enforcer.can_enter(&trade_symbol, &account_snapshot, sym_count) {
                        Ok(_) => {
                            let avg_spread = account_snapshot.avg_spread;
                            let base_min_sl = if account_snapshot.current_spread > 0.0 { avg_spread * 8.0 } else { 20.0 };
                            let mut sl_pips = match account_snapshot.current_latency_ms {
                                l if l > 100 => base_min_sl * 2.0,
                                _ => base_min_sl,
                            };
                            sl_pips = sl_pips.max(if avg_spread > 5.0 { 100.0 } else { 30.0 });
                            if trade_symbol.contains("XAU") || trade_symbol.contains("GOLD") {
                                sl_pips = sl_pips.max(300.0);
                            }
                            if signal_type == fx_scalp_core::state_machine::SignalType::Reversion {
                                sl_pips *= 1.2;
                            }
                            
                            let lots = risk_enforcer.calculate_initial_lots(account_snapshot.equity);
                            Some(StateEvent::FiltersPass { lots, sl_pips })
                        }
                        Err(veto) => {
                            warn!(symbol = %trade_symbol, reason = ?veto, "Entry vetoed");
                            Some(StateEvent::FiltersReject)
                        }
                    }
                 } else {
                    Some(StateEvent::FiltersReject)
                 }
             },
             TradingState::EntryReady { direction, calculated_lots, sl_pips, .. } => {
                 let side = if direction == Direction::Long { "buy" } else { "sell" };
                 let (multiplier, bid, ask) = if let Some(t) = tick_info {
                     (t.pip_multiplier(), t.bid, t.ask)
                 } else { (1.0, 0.0, 0.0) };
                 
                 let tp_pips = sl_pips * 2.0;
                 let (sl, tp) = if direction == Direction::Long {
                     (ask - (sl_pips / multiplier), ask + (tp_pips / multiplier))
                 } else {
                     (bid + (sl_pips / multiplier), bid - (tp_pips / multiplier))
                 };
                 
                 let params = serde_json::json!({
                     "symbol": trade_symbol, "type": side, "volume": calculated_lots, 
                     "slippage": 10, "sl": sl, "tp": tp
                 });
                 
                 info!(symbol = %trade_symbol, "Executing Order...");
                 match bridge.request("execute_order", Some(params)) {
                      Ok(resp) => {
                          if let Some(price) = resp.get("price").and_then(|p| p.as_f64()) {
                              let ticket = resp.get("ticket").and_then(|t| t.as_u64()).unwrap_or(0);
                              Some(StateEvent::OrderFilled { price, ticket })
                          } else {
                             Some(StateEvent::OrderTimeout)
                         }
                     }
                     Err(_) => Some(StateEvent::OrderTimeout)
                 }
             },
             TradingState::PositionOpen { .. } => None,
             TradingState::Exiting { direction, lots, ticket, .. } => {
                  let side = if direction == Direction::Long { "sell" } else { "buy" };
                  let params = serde_json::json!({ 
                      "symbol": trade_symbol, 
                      "type": side, 
                      "volume": lots, 
                      "ticket": ticket 
                  }); 
                  
                  match bridge.request("close_position", Some(params)) {
                     Ok(_) => Some(StateEvent::PositionClosed),
                     Err(_) => None
                 }
             },
             TradingState::Cooldown { until_ms } => {
                 if now >= until_ms { Some(StateEvent::CooldownComplete) } else { None }
             },
             _ => None
         };

         // APPLY EVENTS TO STATE MACHINE
         if let Some(event) = next_event {
             if let Ok(mut state) = app_state.write() {
                 if let Some(trade) = state.active_trades.get_mut(i) {
                     let _ = trade.state_machine.process_event(event);
                 }
             } else {
                 error!("Poisoned RwLock on app_state during event application");
             }
         }
         
         // Special Handling for PositionOpen Updates (Runs every loop for active positions)
         if let Ok(mut state_guard) = app_state.write() {
             let state = &mut *state_guard;
             let tick_ingestion = &state.tick_ingestion;
             let active_trades = &mut state.active_trades;
             
             if let Some(trade) = active_trades.get_mut(i) {
                 if let TradingState::PositionOpen { direction, entry_price, current_lots, sl_pips, tp_pips, entry_time_ms, .. } = trade.state_machine.current_state() {
                    // Ensure Run Exists
                    if trade.active_run.is_none() {
                        trade.active_run = Some(TradingRun::new(trade.symbol.clone(), entry_price, current_lots, entry_time_ms, sl_pips, tp_pips));
                    }
                    
                    // Update Run with latest price and check exits
                    if let Some(run) = &mut trade.active_run {
                         if let Some(buf) = tick_ingestion.get_buffer(&trade.symbol) {
                            if let Some(tick) = buf.latest(1).first() {
                                let current_price = if direction == Direction::Long { tick.bid } else { tick.ask };
                                let multiplier = tick.pip_multiplier();
                                let pips = if direction == Direction::Long { (current_price - entry_price) * multiplier } else { (entry_price - current_price) * multiplier };
                                let pnl = if tick.point > 0.0 {
                                    ((current_price - entry_price) / tick.point).abs() * tick.tick_value * current_lots * pips.signum()
                                } else { pips * current_lots * 10.0 };
                                
                                run.update_price(current_price, pnl, now);
                                
                                // CHECK EXITS (Reversal or Stall)
                                if risk_enforcer.check_reversal(run, multiplier) {
                                    let _ = trade.state_machine.process_event(StateEvent::ReversalDetected);
                                } else if risk_enforcer.check_stall(run, now) {
                                    let _ = trade.state_machine.process_event(StateEvent::StallTimeout);
                                }
                            }
                         }
                    }
                 }
             }
         }
    }
}

/// Cleans up completed trades (Idle state) and accumulates their realized P&L.
pub fn cleanup_completed_trades(app_state: &Arc<RwLock<AppState>>) {
    if let Ok(mut state) = app_state.write() {
        let mut realized_pnl = 0.0;
        for t in &state.active_trades {
            if let TradingState::Idle = t.state_machine.current_state() {
                if let Some(run) = &t.active_run {
                    let realized = run.total_pnl();
                    realized_pnl += realized;
                    
                    // Log to the trade journal
                    state.trade_journal.log_trade(t, fx_scalp_core::state_machine::ExitReason::TakeProfitHit); // Stub for now or fetch actual

                    info!(id = t.id, symbol = %t.symbol, realized_pnl = realized, "Trade cycle complete. Realized P&L calculated.");
                } else {
                    info!(id = t.id, symbol = %t.symbol, "Trade cycle complete without filling.");
                }
            }
        }
        state.daily_pnl += realized_pnl;
        
        state.active_trades.retain(|t| !matches!(t.state_machine.current_state(), TradingState::Idle));
    } else {
        error!("Poisoned RwLock during cleanup phase");
    }
}
