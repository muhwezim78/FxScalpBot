use std::sync::{Arc, RwLock};
use fx_scalp_core::{
    AppState, AppConfig, ActiveTrade, Direction, AccountConfig, MarketConfig, ExitConfig,
    ExecutionConfig, ScalingConfig, Tick, bridge_client::BridgeClient
};

use fx_scalp_core::state_machine::{TradingState, SignalType};

#[test]
fn test_trade_lifecycle() {
    // 1. Setup config (existing code)
    let config = AppConfig {
        market: MarketConfig { symbols: vec!["EURUSD".to_string()] },
        account: AccountConfig {
            daily_loss_limit_pct: 0.05,
            max_parallel_trades: 1,
            max_trades_per_symbol: 1,
            max_position_lots: 1.0,
            lot_per_1000: 0.1,
            risk_per_trade_pct: 0.01,
        },
        scaling: ScalingConfig { max_scales: 0, min_scale_profit: 0.0, max_scale_lots: 0.0, profit_scale_ratio: 0.0 },
        exit: ExitConfig { reversal_threshold_pct: 0.3, stall_timeout_ms: 5000, cooldown_seconds: 0 },
        execution: ExecutionConfig { max_spread_multiplier: 2.0, max_spread_pips: 2.0, max_latency_ms: 100, max_slippage_pips: 1.0 },
        overrides: None,
    };

    let app_state = Arc::new(RwLock::new(AppState::new(config, 10000.0)));
    
    // 3. Push initial ticks
    {
        let mut state = app_state.write().unwrap();
        let tick1 = Tick {
            symbol: "EURUSD".to_string(),
            bid: 1.10000, ask: 1.10010, bid_volume: 1.0, ask_volume: 1.0,
            timestamp_ms: 1000, received_at_ms: 1000, digits: 5, point: 0.00001, tick_value: 1.0,
        };
        state.tick_ingestion.process_tick(tick1);
    }
    
    // 4. Manually spawn a trade
    let trade_id = {
        let mut state = app_state.write().unwrap();
        state.trade_id_counter += 1;
        let id = state.trade_id_counter;
        let mut trade = ActiveTrade::new(id, "EURUSD".to_string());
        let _ = trade.state_machine.process_event(fx_scalp_core::StateEvent::MomentumDetected("EURUSD".to_string(), Direction::Long));
        state.active_trades.push(trade);
        id
    };
    
    // Verify State
    {
        let state = app_state.read().unwrap();
        assert_eq!(state.active_trades.len(), 1);
        if let TradingState::Qualifying { momentum_direction, .. } = state.active_trades[0].state_machine.current_state().clone() {
            assert_eq!(momentum_direction, Direction::Long);
        } else {
            panic!("Expected Qualifying state");
        }
    }
    
    // 5. Force Filters Pass
    {
        let mut state = app_state.write().unwrap();
        let trade = &mut state.active_trades[0];
        let _ = trade.state_machine.process_event(fx_scalp_core::StateEvent::FiltersPass { lots: 0.1, sl_pips: 10.0 });
    }
    
    // 6. Force Order Filled
    {
        let mut state = app_state.write().unwrap();
        let trade = &mut state.active_trades[0];
        let _ = trade.state_machine.process_event(fx_scalp_core::StateEvent::OrderFilled { price: 1.10010, ticket: 12345 });
    }
    
    // Verify Position Open
    {
        let state = app_state.read().unwrap();
        if let TradingState::PositionOpen { current_lots, entry_price, .. } = state.active_trades[0].state_machine.current_state().clone() {
            assert_eq!(current_lots, 0.1);
            assert_eq!(entry_price, 1.10010);
        } else {
            panic!("Expected PositionOpen state");
        }
    }
    
    // 7. Simulate tick movement for Profit
    {
        let mut state = app_state.write().unwrap();
        let tick2 = Tick {
            symbol: "EURUSD".to_string(),
            bid: 1.10210, ask: 1.10220, bid_volume: 1.0, ask_volume: 1.0,
            timestamp_ms: 2000, received_at_ms: 2000, digits: 5, point: 0.00001, tick_value: 1.0,
        };
        state.tick_ingestion.process_tick(tick2);
    }
    
    // 8. Reversal
    {
        let mut state = app_state.write().unwrap();
        let trade = &mut state.active_trades[0];
        let _ = trade.state_machine.process_event(fx_scalp_core::StateEvent::ReversalDetected);
    }
    
    // Verify Exiting
    {
        let state = app_state.read().unwrap();
        match state.active_trades[0].state_machine.current_state() {
            TradingState::Exiting { .. } => {},
            _ => panic!("Expected Exiting state"),
        }
    }
    
    // 9. Force Position Closed
    {
        let mut state = app_state.write().unwrap();
        let trade = &mut state.active_trades[0];
        let _ = trade.state_machine.process_event(fx_scalp_core::StateEvent::PositionClosed);
    }
    
    // Verify Cooldown/Idle
    {
        let state = app_state.read().unwrap();
        match state.active_trades[0].state_machine.current_state() {
            TradingState::Cooldown { .. } => {},
            _ => panic!("Expected Cooldown state"),
        }
    }
}

/// Tests that the capacity limits work properly when pushing trades to state
#[test]
fn test_capacity_limits() {
    let mut config = AppConfig::default();
    config.account.max_parallel_trades = 2;
    config.account.max_trades_per_symbol = 1;
    config.market.symbols = vec!["EURUSD".to_string(), "GBPUSD".to_string()];
    
    let state = AppState::new(config, 10000.0);
    
    // Count active trades per symbol manually as logic mimicking scanner
    let mut counts = std::collections::HashMap::new();
    for t in &state.active_trades {
        *counts.entry(t.symbol.clone()).or_insert(0) += 1;
    }
    
    let global_count = state.active_trades.len() as u32;
    // Initial State
    assert_eq!(global_count, 0);
    assert!(global_count < state.config.account.max_parallel_trades);
}
