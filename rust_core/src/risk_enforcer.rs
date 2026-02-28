//! Risk Enforcer - Absolute risk limits enforced in Rust
//! 
//! This module is the final gate before any order execution.
//! All limits are HARD STOPS with no exceptions.

use crate::AppConfig;
use tracing::{warn, info};

/// Reasons for vetoing a trade
#[derive(Debug, Clone, PartialEq)]
pub enum RiskVeto {
    DailyLimitHit,
    MaxParallelTradesReached,
    MaxTradesPerSymbolReached,
    MaxScalesReached,
    InsufficientLockedProfit,
    LotSizeExceeded,
    SpreadTooWide,
    LatencyTooHigh,
    KillSwitchActive,
    ReversalDetected,
    StallTimeout,
}

impl std::fmt::Display for RiskVeto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RiskVeto::DailyLimitHit => write!(f, "Daily loss limit reached"),
            RiskVeto::MaxParallelTradesReached => write!(f, "Maximum parallel trades reached"),
            RiskVeto::MaxTradesPerSymbolReached => write!(f, "Maximum trades for this symbol reached"),
            RiskVeto::MaxScalesReached => write!(f, "Maximum scale-ins reached (3)"),
            RiskVeto::InsufficientLockedProfit => write!(f, "Not enough locked profit to scale"),
            RiskVeto::LotSizeExceeded => write!(f, "Lot size exceeds maximum"),
            RiskVeto::SpreadTooWide => write!(f, "Spread exceeds 1.5x average"),
            RiskVeto::LatencyTooHigh => write!(f, "Network latency too high"),
            RiskVeto::KillSwitchActive => write!(f, "Kill switch is active"),
            RiskVeto::ReversalDetected => write!(f, "Momentum reversal detected"),
            RiskVeto::StallTimeout => write!(f, "Price stalled - timeout"),
        }
    }
}

/// Tracks a single trading run (entry + optional scale-ins)
#[derive(Debug, Clone)]
pub struct TradingRun {
    pub symbol: String,
    pub entry_price: f64,
    pub current_lots: f64,
    pub scale_count: u8,
    pub locked_profit: f64,      // Realized P&L from partial closes
    pub unrealized_pnl: f64,     // Current floating P&L
    pub peak_profit: f64,        // Highest total P&L achieved
    pub entry_time_ms: u64,
    pub last_price_change_ms: u64,
    pub sl_pips: f64,
    pub tp_pips: f64,
}

impl TradingRun {
    pub fn new(symbol: String, entry_price: f64, initial_lots: f64, entry_time_ms: u64, sl_pips: f64, tp_pips: f64) -> Self {
        Self {
            symbol,
            entry_price,
            current_lots: initial_lots,
            scale_count: 0,
            locked_profit: 0.0,
            unrealized_pnl: 0.0,
            peak_profit: 0.0,
            entry_time_ms,
            last_price_change_ms: entry_time_ms,
            sl_pips,
            tp_pips,
        }
    }
    
    /// Total P&L = locked + unrealized
    pub fn total_pnl(&self) -> f64 {
        self.locked_profit + self.unrealized_pnl
    }
    
    /// Update peak profit tracking
    pub fn update_peak(&mut self) {
        let total = self.total_pnl();
        if total > self.peak_profit {
            self.peak_profit = total;
        }
    }

    /// Update price and P&L
    pub fn update_price(&mut self, _new_price: f64, new_pnl: f64, now_ms: u64) {
        if (new_pnl - self.unrealized_pnl).abs() > 0.00001 {
            self.last_price_change_ms = now_ms;
        }
        self.unrealized_pnl = new_pnl;
        self.update_peak();
    }
}

/// Account state for risk calculations
#[derive(Debug, Clone)]
pub struct Account {
    pub balance: f64,
    pub equity: f64,
    pub daily_pnl: f64,
    pub open_positions: u32,
    pub current_spread: f64,
    pub avg_spread: f64,
    pub current_latency_ms: u64,
}

/// Risk Enforcer - enforces all hard limits
#[derive(Debug, Clone)]
pub struct RiskEnforcer {
    daily_loss_limit: f64,      // Absolute $ limit (calculated from %)
    daily_loss_pct: f64,        // Percentage limit
    max_parallel_trades: u32,
    max_trades_per_symbol: u32,
    max_position_lots: f64,
    lot_per_1000: f64,          // Base sizing
    max_scales: u8,
    reversal_threshold: f64,    // 0.30 = 30%
    stall_timeout_ms: u64,
    max_spread_multiplier: f64,
    max_latency_ms: u64,
    risk_per_trade_pct: f64,
    min_scale_profit: f64,
    profit_scale_ratio: f64,    // Portion of locked profit usable
    max_spread_pips: f64,
    overrides: std::collections::HashMap<String, crate::SymbolOverride>,
}

impl RiskEnforcer {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            daily_loss_limit: 0.0, 
            daily_loss_pct: config.account.daily_loss_limit_pct,
            max_parallel_trades: config.account.max_parallel_trades,
            max_trades_per_symbol: config.account.max_trades_per_symbol,
            max_position_lots: config.account.max_position_lots,
            lot_per_1000: config.account.lot_per_1000,
            max_scales: config.scaling.max_scales,
            reversal_threshold: config.exit.reversal_threshold_pct,
            stall_timeout_ms: config.exit.stall_timeout_ms,
            max_spread_multiplier: config.execution.max_spread_multiplier,
            max_latency_ms: config.execution.max_latency_ms,
            risk_per_trade_pct: config.account.risk_per_trade_pct,
            min_scale_profit: config.scaling.min_scale_profit,
            profit_scale_ratio: config.scaling.profit_scale_ratio,
            max_spread_pips: config.execution.max_spread_pips,
            overrides: config.overrides.clone().unwrap_or_default(),
        }
    }
    
    /// Update daily loss limit based on current account balance
    pub fn update_daily_limit(&mut self, account_balance: f64) {
        self.daily_loss_limit = account_balance * self.daily_loss_pct;
        info!(
            balance = account_balance,
            daily_limit = self.daily_loss_limit,
            "Daily loss limit updated"
        );
    }

    /// Retrieve the applicable max_spread_multiplier for a symbol
    pub fn get_max_spread_multiplier(&self, symbol: &str) -> f64 {
        self.overrides.get(symbol)
            .and_then(|o| o.max_spread_multiplier)
            .unwrap_or(self.max_spread_multiplier)
    }

    /// Retrieve the applicable max_spread_pips for a symbol
    pub fn get_max_spread_pips(&self, symbol: &str) -> f64 {
        self.overrides.get(symbol)
            .and_then(|o| o.max_spread_pips)
            .unwrap_or(self.max_spread_pips)
    }

    /// Retrieve the applicable max_latency_ms for a symbol
    pub fn get_max_latency_ms(&self, symbol: &str) -> u64 {
        self.overrides.get(symbol)
            .and_then(|o| o.max_latency_ms)
            .unwrap_or(self.max_latency_ms)
    }

    /// Retrieve the applicable stall_timeout_ms for a symbol
    pub fn get_stall_timeout_ms(&self, symbol: &str) -> u64 {
        self.overrides.get(symbol)
            .and_then(|o| o.stall_timeout_ms)
            .unwrap_or(self.stall_timeout_ms)
    }
    
    /// Check if a new entry is allowed
    pub fn can_enter(&self, symbol: &str, account: &Account, current_symbol_positions: u32) -> Result<(), RiskVeto> {
        // Check daily loss limit
        if account.daily_pnl < -self.daily_loss_limit {
            warn!(
                daily_pnl = account.daily_pnl,
                limit = -self.daily_loss_limit,
                "Entry blocked: daily limit reached (${:.2} < ${:.2})", 
                account.daily_pnl, -self.daily_loss_limit
            );
            return Err(RiskVeto::DailyLimitHit);
        }
        
        // Global parallel trade limit
        if account.open_positions >= self.max_parallel_trades {
            return Err(RiskVeto::MaxParallelTradesReached);
        }

        // Per-symbol parallel trade limit
        if current_symbol_positions >= self.max_trades_per_symbol {
            return Err(RiskVeto::MaxTradesPerSymbolReached);
        }
        
        let max_spread_mult = self.get_max_spread_multiplier(symbol);
        let max_spread_pips = self.get_max_spread_pips(symbol);

        // Check spread multiplier
        if account.current_spread > account.avg_spread * max_spread_mult {
            warn!(
                symbol = symbol,
                current = account.current_spread,
                limit = account.avg_spread * max_spread_mult,
                "Entry blocked: spread too wide ({:.2} > {:.2} limit)",
                account.current_spread, account.avg_spread * max_spread_mult
            );
            return Err(RiskVeto::SpreadTooWide);
        }

        // Check absolute spread cap
        if account.current_spread > max_spread_pips {
            warn!(
                symbol = symbol,
                current = account.current_spread,
                limit = max_spread_pips,
                "Entry blocked: absolute spread cap hit ({:.2} > {} pips)",
                account.current_spread, max_spread_pips
            );
            return Err(RiskVeto::SpreadTooWide);
        }
        
        let max_latency = self.get_max_latency_ms(symbol);
        // Check latency
        if account.current_latency_ms > max_latency {
            warn!(
                symbol = symbol,
                latency = account.current_latency_ms,
                max = max_latency,
                "Entry blocked: high network latency ({}ms > {}ms max)",
                account.current_latency_ms, max_latency
            );
            return Err(RiskVeto::LatencyTooHigh);
        }
        
        Ok(())
    }
    
    /// Formula: 0.1 lots per $1,000 account balance (10:1 Leverage)
    pub fn calculate_initial_lots(&self, account_balance: f64) -> f64 {
        let mut lots = (account_balance / 1000.0) * self.lot_per_1000;
        lots = (lots * 100.0).round() / 100.0; // Round to MT5 standard (0.01)
        lots.min(self.max_position_lots).max(0.01)
    }

    /// Calculate lot size based on fixed risk percentage of equity
    /// Formula: Lots = (Equity * Risk%) / (SL_Pips * PipValue_per_Lot)
    pub fn calculate_risk_based_lots(&self, equity: f64, sl_pips: f64, tick_value: f64, _point: f64, digits: u32) -> f64 {
        if sl_pips <= 0.0 { return self.calculate_initial_lots(equity); }

        let risk_amount = equity * self.risk_per_trade_pct;
        
        // PipValue_per_Lot = TickValue * (Point_per_Pip / TickSize)
        // If 1 pip = 1.0 (digits=2) and point=0.01, then Point_per_Pip = 1.0.
        let point_per_pip = match digits {
            5 | 4 => 0.0001,
            3 => 0.01,
            _ => 1.0,
        };
        
        // MT5 tick_value is profit for 'tick_size' move.
        // Profit per Pip = tick_value * (point_per_pip / tick_size)
        // But since we don't have tick_size here, assume point is tick_size (true for most)
        let pip_value_per_lot = tick_value * (point_per_pip / _point.max(0.000001));
        
        if pip_value_per_lot <= 0.0 { return self.calculate_initial_lots(equity); }
        
        let mut lots = risk_amount / (sl_pips * pip_value_per_lot);
        
        // Sanity checks and rounding
        lots = (lots * 100.0).round() / 100.0; // Round to 0.01
        lots.min(self.max_position_lots).max(0.01)
    }
    
    /// Check if scaling is allowed and calculate lot size
    /// CRITICAL: Uses ONLY locked profits, never original capital
    pub fn can_scale(&self, run: &TradingRun) -> Result<f64, RiskVeto> {
        // Check max scales
        if run.scale_count >= self.max_scales {
            return Err(RiskVeto::MaxScalesReached);
        }
        
        // Check locked profit availability
        // Only use specified ratio of locked profits for scaling
        let available = run.locked_profit * self.profit_scale_ratio;
        if available < self.min_scale_profit {
            return Err(RiskVeto::InsufficientLockedProfit);
        }
        
        // Calculate scale lot size (linear, not exponential)
        // Formula: 0.001 lots per $1 of available profit
        let scale_lots = self.calculate_scale_lots(available, run.scale_count);
        
        Ok(scale_lots.min(self.max_position_lots - run.current_lots))
    }
    
    /// Calculate lot size for scale-in
    /// Linear scaling: each scale adds slightly more based on locked profit
    fn calculate_scale_lots(&self, available_profit: f64, scale_count: u8) -> f64 {
        let lot_per_dollar = 0.001;
        let scale_factor = match scale_count {
            0 => 0.3,  // First scale: 30% of available
            1 => 0.4,  // Second scale: 40% of available
            2 => 0.5,  // Third scale: 50% of available
            _ => 0.0,  // No more scales
        };
        
        available_profit * lot_per_dollar * scale_factor
    }
    
    /// Check if a reversal has occurred (30% of run profit lost)
    /// CRITICAL: Only triggers if peak profit was significant to avoid noise
    pub fn check_reversal(&self, run: &TradingRun, pip_multiplier: f64) -> bool {
        // Only consider reversal if we've reached a significant peak (e.g. 3 pips)
        let min_peak_pips = 3.0; // Minimal "cushion" before reversal tracking starts
        let peak_pips = (run.peak_profit / run.current_lots) * pip_multiplier; // Rough USD to Pip conversion
        
        if run.peak_profit <= 0.0 || peak_pips < min_peak_pips {
            return false;
        }
        
        let current_pnl = run.total_pnl();
        
        // If we are actually in a loss, don't let "reversal" logic handle it.
        // Let Stop Loss or Stall handle losers. Reversal is for profit harvesting.
        if current_pnl <= 0.0 {
            return false;
        }

        let drawdown = run.peak_profit - current_pnl;
        let drawdown_pct = drawdown / run.peak_profit;
        
        if drawdown_pct > self.reversal_threshold {
            warn!(
                peak = run.peak_profit,
                current = current_pnl,
                drawdown_pct = drawdown_pct,
                "Reversal detected: {} of profits lost", 
                format!("{:.1}%", drawdown_pct * 100.0)
            );
            return true;
        }
        
        false
    }
    
    /// Check if price has stalled (no movement for timeout period)
    pub fn check_stall(&self, run: &TradingRun, current_time_ms: u64) -> bool {
        let stall_duration = current_time_ms - run.last_price_change_ms;
        let timeout = self.get_stall_timeout_ms(&run.symbol);
        stall_duration > timeout
    }
    
    /// Validate a lot size is within limits
    pub fn validate_lot_size(&self, lots: f64) -> Result<(), RiskVeto> {
        if lots > self.max_position_lots {
            return Err(RiskVeto::LotSizeExceeded);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn default_config() -> AppConfig {
        AppConfig::default()
    }
    
    #[test]
    fn test_initial_lot_calculation() {
        let mut config = AppConfig::default();
        config.account.lot_per_1000 = 0.01;
        config.account.max_position_lots = 0.1;
        let enforcer = RiskEnforcer::new(&config);
        
        // $1,000 account = 0.01 lots
        assert!((enforcer.calculate_initial_lots(1000.0) - 0.01).abs() < 0.0001);
        
        // $10,000 account = 0.1 lots (capped at max)
        assert!((enforcer.calculate_initial_lots(10000.0) - 0.1).abs() < 0.0001);
        
        // $100,000 account = still 0.1 lots (max)
        assert!((enforcer.calculate_initial_lots(100000.0) - 0.1).abs() < 0.0001);
    }
    
    #[test]
    fn test_reversal_detection() {
        let enforcer = RiskEnforcer::new(&default_config());
        
        let mut run = TradingRun::new("EURUSD".to_string(), 1.0, 0.01, 0, 5.0, 10.0);
        run.locked_profit = 10.0;
        run.peak_profit = 100.0;
        run.unrealized_pnl = 75.0; // Total P&L = 85, drawdown = 15%
        
        assert!(!enforcer.check_reversal(&run, 10000.0));
        
        run.unrealized_pnl = 55.0; // Total P&L = 65, drawdown = 35%
        assert!(enforcer.check_reversal(&run, 10000.0));
    }
    
    #[test]
    fn test_scale_blocked_without_locked_profit() {
        let enforcer = RiskEnforcer::new(&default_config());
        
        let run = TradingRun::new("EURUSD".to_string(), 1.0, 0.01, 0, 5.0, 10.0);
        // No locked profit = cannot scale
        assert_eq!(enforcer.can_scale(&run), Err(RiskVeto::InsufficientLockedProfit));
    }
    
    #[test]
    fn test_max_scales_enforced() {
        let enforcer = RiskEnforcer::new(&default_config());
        
        let mut run = TradingRun::new("EURUSD".to_string(), 1.0, 0.01, 0, 5.0, 10.0);
        run.locked_profit = 100.0;
        run.scale_count = 3; // Already at max
        
        assert_eq!(enforcer.can_scale(&run), Err(RiskVeto::MaxScalesReached));
    }

    #[test]
    fn test_symbol_overrides() {
        let mut config = AppConfig::default();
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("BTCUSDm".to_string(), crate::SymbolOverride {
            max_spread_multiplier: Some(10.0),
            max_spread_pips: Some(80.0),
            max_latency_ms: Some(200),
            max_slippage_pips: Some(50.0),
            stall_timeout_ms: Some(60000),
        });
        config.overrides = Some(overrides);
        
        let enforcer = RiskEnforcer::new(&config);
        
        assert_eq!(enforcer.get_max_latency_ms("BTCUSDm"), 200);
        assert_eq!(enforcer.get_max_latency_ms("EURUSDm"), 50); // Default
        
        assert_eq!(enforcer.get_stall_timeout_ms("BTCUSDm"), 60000);
        assert_eq!(enforcer.get_stall_timeout_ms("EURUSDm"), 15000); // Default
    }
}
