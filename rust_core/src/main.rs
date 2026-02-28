mod scanner;
mod trade_manager;

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tracing::{info, error};

use fx_scalp_core::{
    AppConfig, AppState, bridge_client::{BridgeClient, BridgeMessage}
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
    
    let mut last_heartbeat = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    
    loop {
        // 0. Heartbeat
        let current_secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        if current_secs - last_heartbeat >= 30 {
            last_heartbeat = current_secs;
            let state = app_state.read().unwrap();
            info!(
                balance = state.account_balance,
                equity = state.account_equity,
                daily_pnl = state.daily_pnl,
                open_positions = state.open_positions_count,
                active_trades = state.active_trades.len(),
                "Heartbeat Status"
            );
        }

        // 1. Check Global Kill Switch
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
        
        if kill_active { break; }
        
        if daily_loss_hit {
            let state_mut = app_state.write().unwrap();
            let daily_limit = state_mut.account_balance * state_mut.config.account.daily_loss_limit_pct;
             
             if state_mut.daily_pnl < -daily_limit {
                state_mut.kill_switch.trigger(fx_scalp_core::KillReason::DailyLossLimit);
                error!(daily_pnl = state_mut.daily_pnl, limit = -daily_limit, "Daily loss limit hit - shutting down");
                break;
             }
        }
        
        // 2. Process Bridge Messages (Ticks & Account)
        while let Ok(msg) = bridge_rx.try_recv() {
            let mut state = app_state.write().unwrap();
            match msg {
                BridgeMessage::Tick { data } => {
                    if let Ok(mut tick_data) = serde_json::from_value::<fx_scalp_core::Tick>(data) {
                        tick_data.received_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        let _ = state.tick_ingestion.process_tick(tick_data);
                    }
                }
                BridgeMessage::Account { data } => {
                    if let Some(balance) = data.get("balance").and_then(|v| v.as_f64()) { state.account_balance = balance; }
                    if let Some(equity) = data.get("equity").and_then(|v| v.as_f64()) { state.account_equity = equity; }
                    if let Some(count) = data.get("positions_count").and_then(|v| v.as_u64()) { state.open_positions_count = count as u32; }
                    let balance = state.account_balance;
                    state.risk_enforcer.update_daily_limit(balance);
                }
            }
        }

        let _now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

        // 3. SCANNING PHASE
        scanner::scan_for_opportunities(&app_state, &mut bridge);

        // 4. MANAGEMENT PHASE
        trade_manager::update_active_trades(&app_state, &mut bridge);

        // 5. CLEANUP PHASE
        trade_manager::cleanup_completed_trades(&app_state);
        
        thread::sleep(Duration::from_millis(20));
    }
    
    info!("FxScalpBot shutdown complete");
    Ok(())
}
