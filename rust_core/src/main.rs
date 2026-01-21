use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::{info, error, warn};

use fx_scalp_core::{
    AppConfig, AppState, TradingState, Direction, StateEvent,
    Account, TradingRun, bridge_client::{BridgeClient, BridgeMessage}
};

fn load_config() -> AppConfig {
    let config_path = "../config/risk_limits.toml";
    match std::fs::read_to_string(config_path) {
        Ok(content) => {
            match toml::from_str(&content) {
                Ok(config) => {
                    info!("Successfully loaded config from {}", config_path);
                    config
                }
                Err(e) => {
                    error!("Failed to parse config file: {}. Using defaults.", e);
                    AppConfig::default()
                }
            }
        }
        Err(_) => {
            info!("Config file not found at {}. Using defaults.", config_path);
            AppConfig::default()
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    info!("FxScalpBot starting...");
    info!("Design: Conservative momentum scalping");
    info!("Mode: Capital preservation first");
    
    let config = load_config();
    info!(?config, "Configuration loaded");
    
    let initial_balance = 10000.0; 
    let app_state = Arc::new(RwLock::new(AppState::new(config, initial_balance)));
    
    // Connect to Python Bridge
    let bridge_addr = "127.0.0.1:5555";
    let mut bridge = match BridgeClient::connect(bridge_addr) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to connect to TradingService at {}: {}", bridge_addr, e);
            error!("Ensure python_strategy/src/trading_service.py is running.");
            return Ok(());
        }
    };

    // Start Bridge Listener
    let bridge_rx = bridge.start_listener();

    // Initial Account Sync
    if let Ok(data) = bridge.request("get_account", None) {
        let mut state = app_state.write().unwrap();
        if let Some(balance) = data.get("balance").and_then(|v| v.as_f64()) {
            state.account_balance = balance;
        }
        if let Some(equity) = data.get("equity").and_then(|v| v.as_f64()) {
            state.account_equity = equity;
        }
    }

    {
        let state = app_state.read().unwrap();
        info!(
            daily_limit_pct = state.config.account.daily_loss_limit_pct,
            max_scales = state.config.scaling.max_scales,
            symbols = ?state.config.market.symbols,
            "System initialized and listening for ticks"
        );
    }
    
    loop {
        let (kill_active, daily_loss_hit) = {
            let state = app_state.read().unwrap();
            
            if state.kill_switch.is_active() {
                error!(
                    reason = ?state.kill_switch.get_reason(),
                    "Kill switch active - trading halted"
                );
                return Ok(());
            }
            
            let daily_limit = state.account_balance * state.config.account.daily_loss_limit_pct;
            let hit = state.daily_pnl < -daily_limit;
            
            (state.kill_switch.is_active(), hit)
        };
        
        if kill_active {
            break;
        }
        
        if daily_loss_hit {
            let state_mut = app_state.write().unwrap();
            let daily_limit = state_mut.account_balance * state_mut.config.account.daily_loss_limit_pct;
             
             if state_mut.daily_pnl < -daily_limit {
                state_mut.kill_switch.trigger(fx_scalp_core::KillReason::DailyLossLimit);
                error!(
                    daily_pnl = state_mut.daily_pnl,
                    limit = -daily_limit,
                    "Daily loss limit hit - shutting down"
                );
                break;
             }
        }
        
        // Process messages from bridge
        while let Ok(msg) = bridge_rx.try_recv() {
            let mut state = app_state.write().unwrap();
            match msg {
                BridgeMessage::Tick { data } => {
                    match serde_json::from_value::<fx_scalp_core::Tick>(data.clone()) {
                        Ok(mut tick_data) => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis() as u64;
                            tick_data.received_at_ms = now;
                            let _ = state.tick_ingestion.process_tick(tick_data);
                        },
                        Err(e) => {
                            error!(error = %e, data = %data, "Failed to parse tick data from bridge");
                        }
                    }
                }
                BridgeMessage::Account { data } => {
                    if let Some(balance) = data.get("balance").and_then(|v| v.as_f64()) {
                        state.account_balance = balance;
                    }
                    if let Some(equity) = data.get("equity").and_then(|v| v.as_f64()) {
                        state.account_equity = equity;
                    }
                    if let Some(count) = data.get("positions_count").and_then(|v| v.as_u64()) {
                        state.open_positions_count = count as u32;
                    }
                    let balance = state.account_balance;
                    state.risk_enforcer.update_daily_limit(balance);
                }
            }
        }

        // Process outgoing orders
        let orders_to_send = {
            let mut state = app_state.write().unwrap();
            state.order_executor.pull_pending_submissions()
        };

        for order in orders_to_send {
            if let fx_scalp_core::OrderStatus::Submitted = order.status {
                info!(order_id = order.id, "Dispatching order to MT5 bridge...");
                
                let params = serde_json::json!({
                    "symbol": order.symbol,
                    "type": match order.side {
                        fx_scalp_core::OrderSide::Buy => "buy",
                        fx_scalp_core::OrderSide::Sell => "sell",
                    },
                    "volume": order.lots,
                    "slippage": order.slippage_tolerance,
                });

                match bridge.request("execute_order", Some(params)) {
                    Ok(resp_data) => {
                        let mut state = app_state.write().unwrap();
                        if let Some(price) = resp_data.get("price").and_then(|v| v.as_f64()) {
                            let _ = state.order_executor.process_fill(order.id, price, order.price);
                            info!(order_id = order.id, price = price, "Order fill confirmed by MT5");
                        }
                    }
                    Err(e) => {
                        let mut state = app_state.write().unwrap();
                        state.order_executor.update_from_bridge(order.id, fx_scalp_core::OrderStatus::Rejected { reason: e.clone() });
                        error!(order_id = order.id, error = %e, "Order rejected by MT5");
                    }
                }
            }
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let current_state = {
            let state = app_state.read().unwrap();
            state.state_machine.current_state().clone()
        };

        match current_state {
            TradingState::Idle => {
                let symbols = {
                    let state = app_state.read().unwrap();
                    state.config.market.symbols.clone()
                };

                for symbol in symbols {
                    let ticks_json = {
                        let state = app_state.read().unwrap();
                        if let Some(buffer) = state.tick_ingestion.get_buffer(&symbol) {
                            if buffer.len() >= 50 {
                                let latest = buffer.latest(50);
                                Some(serde_json::to_value(latest).unwrap())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };

                    if let Some(ticks) = ticks_json {
                        let params = serde_json::json!({ "symbol": symbol, "ticks": ticks });
                        
                        // Try Momentum first
                        if let Ok(resp) = bridge.request("analyze_momentum", Some(params.clone())) {
                            if let Ok(signal) = serde_json::from_value::<fx_scalp_core::MomentumSignal>(resp) {
                                if signal.detected {
                                    let dir = if signal.direction > 0 { Direction::Long } else { Direction::Short };
                                    let mut state_mut = app_state.write().unwrap();
                                    let _ = state_mut.state_machine.process_event(StateEvent::MomentumDetected(symbol.clone(), dir));
                                    
                                    info!(
                                        symbol = %symbol, 
                                        direction = ?dir, 
                                        body_pct = %format!("{:.0}%", signal.quality.as_ref().map(|q| q.body_ratio).unwrap_or(0.0) * 100.0),
                                        close_pct = %format!("{:.0}%", signal.quality.as_ref().map(|q| q.close_pct).unwrap_or(0.0) * 100.0),
                                        ema_slope = %signal.ema_slope,
                                        "Zero-Lag Impulse detected! Quality metrics high."
                                    );
                                    break; 
                                }
                            }
                        }

                        // Try Reversion if Momentum not detected
                        if let Ok(resp) = bridge.request("analyze_reversion", Some(params)) {
                            if let Ok(signal) = serde_json::from_value::<fx_scalp_core::ReversionSignal>(resp) {
                                if signal.detected {
                                    let dir = if signal.direction > 0 { Direction::Long } else { Direction::Short };
                                    let mut state_mut = app_state.write().unwrap();
                                    let _ = state_mut.state_machine.process_event(StateEvent::ReversionDetected(symbol.clone(), dir));
                                    
                                    info!(symbol = %symbol, direction = ?dir, z_score = %signal.z_score, "Mean reversion detected, qualifying...");
                                    break;
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
            TradingState::Qualifying { symbol, signal_type, .. } => {
                let (tick_info, current_spread, avg_spread, current_latency) = {
                    let state = app_state.read().unwrap();
                    let buffer = state.tick_ingestion.get_buffer(&symbol);
                    let tick = buffer.and_then(|b| b.latest(1).first().map(|&t| t.clone()));
                    let spread = buffer.and_then(|b| b.current_spread()).unwrap_or(0.0);
                    let avg_spread = buffer.map(|b| b.average_spread()).unwrap_or(0.0);
                    let latency = buffer.and_then(|b| b.current_latency_ms()).unwrap_or(0) as u64;
                    (tick, spread, avg_spread, latency)
                };

                match tick_info {
                    Some(_tick) => {
                        let account = Account {
                            balance: { app_state.read().unwrap().account_balance },
                            equity: { app_state.read().unwrap().account_equity },
                            daily_pnl: { app_state.read().unwrap().daily_pnl },
                            open_positions: { app_state.read().unwrap().open_positions_count },
                            current_spread,
                            avg_spread,
                            current_latency_ms: current_latency,
                        };

                        let mut state_mut = app_state.write().unwrap();
                        match state_mut.risk_enforcer.can_enter(&symbol, &account) {
                            Ok(_) => {
                                // Adaptive SL based on ATR or Spread - Widened for Stealth Mode
                                let mut sl_pips = (avg_spread * 5.0).max(20.0); 
                                
                                // Reversion-specific entry logic
                                if signal_type == fx_scalp_core::state_machine::SignalType::Reversion {
                                    // Mean reversion trades might need tighter SL or different scaling
                                    // For now, let's increase the SL distance slightly to allow room for the mean to catch up
                                    sl_pips *= 1.2;
                                    info!(symbol = %symbol, "Applying Mean Reversion specific risk adjustments (SL factor 1.2x)");
                                }

                                let lots = state_mut.risk_enforcer.calculate_initial_lots(account.equity);
                                
                                let _ = state_mut.state_machine.process_event(StateEvent::FiltersPass { lots, sl_pips });
                                info!(symbol = %symbol, lots = %lots, "Risk approval granted, universal scaling lots calculated");
                            }
                            Err(veto) => {
                                warn!(symbol = %symbol, reason = ?veto, "Risk vetoed entry");
                                let _ = state_mut.state_machine.process_event(StateEvent::FiltersReject);
                            }
                        }
                    }
                    None => {
                        let mut state_mut = app_state.write().unwrap();
                        let _ = state_mut.state_machine.process_event(StateEvent::FiltersReject);
                    }
                }
            }
            TradingState::EntryReady { symbol, direction, calculated_lots, sl_pips, .. } => {
                let side = if direction == Direction::Long { "buy" } else { "sell" };
                let (multiplier, tick_prices) = {
                    let state = app_state.read().unwrap();
                    let buffer = state.tick_ingestion.get_buffer(&symbol);
                    let tick = buffer.and_then(|b| b.latest(1).first().map(|&t| t.clone()));
                    match tick {
                        Some(t) => (t.pip_multiplier(), Some((t.bid, t.ask))),
                        None => (10000.0, None)
                    }
                };

                let tp_pips = sl_pips * 2.0; // Dynamic TP: 2:1 RR
                
                let (sl, tp) = if let Some((bid, ask)) = tick_prices {
                    if direction == Direction::Long {
                        (ask - (sl_pips / multiplier), ask + (tp_pips / multiplier))
                    } else {
                        (bid + (sl_pips / multiplier), bid - (tp_pips / multiplier))
                    }
                } else {
                    (0.0, 0.0)
                };

                let params = serde_json::json!({
                    "symbol": symbol,
                    "type": side,
                    "volume": calculated_lots,
                    "slippage": 10,
                    "sl": sl,
                    "tp": tp
                });

                info!(symbol = %symbol, side = %side, lots = %calculated_lots, sl = %sl, tp = %tp, "Submitting live order to bridge with emergency SL/TP...");
                match bridge.request("execute_order", Some(params)) {
                    Ok(resp) => {
                        if let Some(price) = resp.get("price").and_then(|p| p.as_f64()) {
                            let mut state_mut = app_state.write().unwrap();
                            let _ = state_mut.state_machine.process_event(StateEvent::OrderFilled { price });
                            info!(symbol = %symbol, price = %price, "Order filled! Position is now LIVE.");
                        } else {
                            error!("Order failed: missing price in response");
                            let mut state_mut = app_state.write().unwrap();
                            let _ = state_mut.state_machine.process_event(StateEvent::OrderTimeout);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Bridge execution error");
                        let mut state_mut = app_state.write().unwrap();
                        let _ = state_mut.state_machine.process_event(StateEvent::OrderTimeout);
                    }
                }
            }
            TradingState::PositionOpen { symbol, direction, entry_price, current_lots, sl_pips, tp_pips, entry_time_ms } => {
                let mut state_mut = app_state.write().unwrap();
                
                // CRITICAL: Ensure active_run is fresh for this specific position
                // If it's None or from a different symbol/time, reset it.
                let is_fresh = state_mut.active_run.as_ref()
                    .map(|r| r.symbol == symbol && r.entry_time_ms == entry_time_ms)
                    .unwrap_or(false);

                if !is_fresh {
                     info!(symbol = %symbol, "Initializing fresh profit tracking for new position");
                     let run = TradingRun::new(symbol.clone(), entry_price, current_lots, entry_time_ms, sl_pips, tp_pips);
                     state_mut.active_run = Some(run);
                }
                
                if let Some(mut run) = state_mut.active_run.take() {
                    if let Some(buffer) = state_mut.tick_ingestion.get_buffer(&symbol) {
                        if let Some(tick) = buffer.latest(1).first().map(|&t| t.clone()) {
                            let current_price = if direction == Direction::Long { tick.bid } else { tick.ask };
                            let multiplier = tick.pip_multiplier();
                            let pips = if direction == Direction::Long {
                                (current_price - entry_price) * multiplier
                            } else {
                                (entry_price - current_price) * multiplier
                            };
                            
                            let pnl = if tick.point > 0.0 {
                                ((current_price - entry_price) / tick.point).abs() * tick.tick_value * current_lots * pips.signum()
                            } else {
                                pips * current_lots * 10.0
                            };
                            run.update_price(current_price, pnl, now);
                            
                            // Pure Stealth: No fixed Stop Loss or Take Profit checks.
                            // We rely 100% on Stall and Reversal detection below.
                            
                            if state_mut.risk_enforcer.check_reversal(&run) {
                                let _ = state_mut.state_machine.process_event(StateEvent::ReversalDetected);
                            } else if state_mut.risk_enforcer.check_stall(&run, now) {
                                let _ = state_mut.state_machine.process_event(StateEvent::StallTimeout);
                            }
                        }
                    }
                    state_mut.active_run = Some(run);
                }
                drop(state_mut);
                thread::sleep(Duration::from_millis(20)); // High-velocity check
            }
            TradingState::Exiting { symbol, direction, reason, .. } => {
                if now % 1000 < 50 { // Only log once per second-ish to avoid flood
                    info!(symbol = %symbol, reason = ?reason, "Exiting position (retrying if needed)...");
                }
                
                let lots = {
                    let state = app_state.read().unwrap();
                    state.active_run.as_ref().map(|run| run.current_lots).unwrap_or(0.0)
                };

                let dir = direction; // Use the direction from the state itself
                let side = if dir == Direction::Long { "buy" } else { "sell" };
                let params = serde_json::json!({
                    "symbol": symbol,
                    "type": side,
                    "volume": lots,
                });

                match bridge.request("close_position", Some(params)) {
                    Ok(_) => {
                        info!(symbol = %symbol, "Bridge closure successful");
                        let mut state_mut = app_state.write().unwrap();
                        state_mut.active_run = None;
                        let _ = state_mut.state_machine.process_event(StateEvent::PositionClosed);
                    },
                    Err(e) => {
                        error!(symbol = %symbol, error = %e, "Bridge closure failed. Retrying in 500ms...");
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
            TradingState::Cooldown { until_ms } => {
                if now >= until_ms {
                    let mut state_mut = app_state.write().unwrap();
                    let _ = state_mut.state_machine.process_event(StateEvent::CooldownComplete);
                    info!("Cooldown complete, returning to idle");
                }
                thread::sleep(Duration::from_millis(100));
            }
            _ => {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    
    info!("FxScalpBot shutdown complete");
    Ok(())
}
