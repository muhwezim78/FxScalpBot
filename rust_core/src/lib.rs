//! FxScalpBot Core Library
//!
//! High-performance execution layer for conservative momentum scalping.
//!
//! # Modules
//! - `risk_enforcer`: Hard risk limits and position sizing
//! - `kill_switch`: Emergency shutdown mechanism
//! - `state_machine`: Trade lifecycle FSM
//! - `tick_ingestion`: Market data processing
//! - `order_executor`: Order and position management
//! - `python_bridge`: Strategy communication (uses fallback when Python unavailable)

pub mod risk_enforcer;
pub mod kill_switch;
pub mod state_machine;
pub mod tick_ingestion;
pub mod order_executor;
pub mod python_bridge;
pub mod bridge_client;
pub mod trade_journal;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionConfig {
    pub max_spread_multiplier: f64,
    pub max_spread_pips: f64,
    pub max_latency_ms: u64,
    pub max_slippage_pips: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SymbolOverride {
    pub max_spread_multiplier: Option<f64>,
    pub max_spread_pips: Option<f64>,
    pub max_latency_ms: Option<u64>,
    pub max_slippage_pips: Option<f64>,
    pub stall_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MarketConfig {
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountConfig {
    pub daily_loss_limit_pct: f64,
    pub max_parallel_trades: u32,
    pub max_trades_per_symbol: u32,
    pub max_position_lots: f64,
    pub lot_per_1000: f64,
    pub risk_per_trade_pct: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScalingConfig {
    pub max_scales: u8,
    pub min_scale_profit: f64,
    pub max_scale_lots: f64,
    pub profit_scale_ratio: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExitConfig {
    pub reversal_threshold_pct: f64,
    pub stall_timeout_ms: u64,
    pub cooldown_seconds: u64,
}

/// Application configuration loaded from TOML
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub market: MarketConfig,
    pub account: AccountConfig,
    pub scaling: ScalingConfig,
    pub exit: ExitConfig,
    pub execution: ExecutionConfig,
    pub overrides: Option<std::collections::HashMap<String, SymbolOverride>>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            market: MarketConfig {
                symbols: vec!["EURUSD".to_string()],
            },
            account: AccountConfig {
                daily_loss_limit_pct: 0.02,
                max_parallel_trades: 1, // Default to 1
                max_trades_per_symbol: 1,
                max_position_lots: 0.1,
                lot_per_1000: 0.01,
                risk_per_trade_pct: 0.005, // 0.5% default risk
            },
            scaling: ScalingConfig {
                max_scales: 3,
                min_scale_profit: 1.0,
                max_scale_lots: 0.1,
                profit_scale_ratio: 0.5,
            },
            exit: ExitConfig {
                reversal_threshold_pct: 0.30,
                stall_timeout_ms: 15000,
                cooldown_seconds: 30,
            },
            execution: ExecutionConfig {
                max_spread_multiplier: 1.5,
                max_spread_pips: 2.0, // Default 2 pips max
                max_latency_ms: 50,
                max_slippage_pips: 1.0,
            },
            overrides: None,
        }
    }
}

/// Holds state for a SINGLE active trade instance
pub struct ActiveTrade {
    pub id: u64,
    pub symbol: String,
    pub state_machine: TradingStateMachine,
    pub active_run: Option<TradingRun>,
    /// Tracks the ZMQ req_id for a pending async execution (order or close)
    pub pending_req_id: Option<String>,
}

impl ActiveTrade {
    pub fn new(id: u64, symbol: String) -> Self {
        Self {
            id,
            symbol,
            state_machine: TradingStateMachine::new(),
            active_run: None,
            pending_req_id: None,
        }
    }
}

/// Core application state
pub struct AppState {
    pub config: AppConfig,
    pub risk_enforcer: RiskEnforcer,
    pub kill_switch: KillSwitch,
    pub active_trades: Vec<ActiveTrade>,
    pub trade_id_counter: u64,
    pub tick_ingestion: tick_ingestion::TickIngestion,
    pub order_executor: order_executor::OrderExecutor,
    pub account_balance: f64,
    pub account_equity: f64,
    pub daily_pnl: f64,
    pub open_positions_count: u32,
    pub trade_journal: trade_journal::TradeJournal,
    /// Per-symbol cooldown after execution failure (e.g. "No money")
    /// Maps symbol -> timestamp_ms when cooldown expires
    pub execution_cooldowns: std::collections::HashMap<String, u64>,
}

impl AppState {
    pub fn new(config: AppConfig, initial_balance: f64) -> Self {
        let risk_enforcer = RiskEnforcer::new(&config);
        let kill_switch = KillSwitch::new();
        let active_trades = Vec::new();
        
        let tick_ingestion = tick_ingestion::TickIngestion::new(1000); // 1s permissive threshold for ingestion
        let order_executor = order_executor::OrderExecutor::new(config.execution.max_slippage_pips);
        
        Self {
            config,
            risk_enforcer,
            kill_switch,
            active_trades,
            trade_id_counter: 0,
            tick_ingestion,
            order_executor,
            account_balance: initial_balance,
            account_equity: initial_balance,
            daily_pnl: 0.0,
            open_positions_count: 0,
            trade_journal: trade_journal::TradeJournal::new("logs/trade_journal.csv"),
            execution_cooldowns: std::collections::HashMap::new(),
        }
    }
}

// Re-export main types for convenience
pub use risk_enforcer::{RiskEnforcer, RiskVeto, TradingRun, Account};
pub use kill_switch::{KillSwitch, KillReason};
pub use state_machine::{TradingStateMachine, TradingState, Direction, StateEvent, ExitReason};
pub use tick_ingestion::{Tick, TickBuffer, TickIngestion};
pub use order_executor::{OrderExecutor, Order, OrderSide, OrderType, OrderStatus, Position, Fill};
pub use python_bridge::{MomentumSignal, ReversionSignal, QualificationResult, VolatilityRegime};
