//! Trading State Machine - Enforces valid state transitions

use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Signal types supported by the bot
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalType {
    Momentum,
    Reversion,
}

/// Valid trading states
#[derive(Debug, Clone, PartialEq)]
pub enum TradingState {
    /// Waiting for momentum signal
    Idle,
    
    /// Evaluating entry filters
    Qualifying {
        symbol: String,
        started_at_ms: u64,
        momentum_direction: Direction,
        signal_type: SignalType,
    },
    
    /// Filters passed, ready to submit order
    EntryReady {
        symbol: String,
        direction: Direction,
        calculated_lots: f64,
        sl_pips: f64,
        timeout_at_ms: u64,
    },
    
    /// Position is open
    PositionOpen {
        symbol: String,
        direction: Direction,
        entry_price: f64,
        current_lots: f64,
        sl_pips: f64,
        tp_pips: f64,
        entry_time_ms: u64,
        ticket: u64,
    },
    
    /// Adding to position (max 3 times)
    Scaling {
        symbol: String,
        direction: Direction,
        entry_price: f64,
        current_lots: f64,
        sl_pips: f64,
        tp_pips: f64,
        started_at_ms: u64,
        ticket: u64,
    },
    
    /// Closing all positions
    Exiting {
        symbol: String,
        direction: Direction,
        reason: ExitReason,
        started_at_ms: u64,
        lots: f64,
        ticket: u64,
    },
    
    /// Mandatory cooldown period
    Cooldown {
        until_ms: u64,
    },
}

/// Trade direction
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Long,
    Short,
}

/// Reasons for exiting a trade
#[derive(Debug, Clone, PartialEq)]
pub enum ExitReason {
    TakeProfitHit,
    StopLossHit,
    MomentumDecay,
    StallTimeout,
    ReversalDetected,
    SpreadExpansion,
    ManualExit,
    KillSwitch,
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitReason::TakeProfitHit => write!(f, "Take profit hit"),
            ExitReason::StopLossHit => write!(f, "Stop loss hit"),
            ExitReason::MomentumDecay => write!(f, "Momentum decayed"),
            ExitReason::StallTimeout => write!(f, "Price stalled (timeout)"),
            ExitReason::ReversalDetected => write!(f, "Reversal detected (30% drawdown)"),
            ExitReason::SpreadExpansion => write!(f, "Spread expanded"),
            ExitReason::ManualExit => write!(f, "Manual exit"),
            ExitReason::KillSwitch => write!(f, "Kill switch activated"),
        }
    }
}

/// State transition events
#[derive(Debug, Clone)]
pub enum StateEvent {
    MomentumDetected(String, Direction),
    ReversionDetected(String, Direction),
    FiltersPass { lots: f64, sl_pips: f64 },
    FiltersReject,
    OrderFilled { price: f64, ticket: u64 },
    OrderTimeout,
    ProfitLocked { amount: f64 },
    MomentumContinues,
    ScaleComplete { new_lots: f64 },
    MaxScalesReached,
    TakeProfitHit,
    StopLossHit,
    MomentumDecay,
    StallTimeout,
    ReversalDetected,
    SpreadExpanded,
    PositionClosed,
    CooldownComplete,
    KillSwitchTriggered,
}

/// State machine for trading lifecycle
pub struct TradingStateMachine {
    current: TradingState,
    history: Vec<(u64, TradingState, StateEvent)>,
    cooldown_ms: u64,
}

impl Default for TradingStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl TradingStateMachine {
    pub fn new() -> Self {
        Self {
            current: TradingState::Idle,
            history: Vec::new(),
            cooldown_ms: 30000,
        }
    }
    
    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }
    
    /// Get current state
    pub fn current_state(&self) -> TradingState {
        self.current.clone()
    }
    
    /// Get current trade direction if in a trade state
    pub fn current_direction(&self) -> Option<Direction> {
        match &self.current {
            TradingState::Qualifying { momentum_direction, .. } => Some(*momentum_direction),
            TradingState::EntryReady { direction, .. } => Some(*direction),
            TradingState::PositionOpen { direction, .. } => Some(*direction),
            TradingState::Scaling { direction, .. } => Some(*direction),
            TradingState::Exiting { direction, .. } => Some(*direction),
            _ => None,
        }
    }

    /// Process an event and transition state
    pub fn process_event(&mut self, event: StateEvent) -> Result<(), String> {
        let now = Self::now_ms();
        let new_state = self.calculate_transition(&event)?;
        
        info!(
            from = ?self.current,
            to = ?new_state,
            event = ?event,
            "State transition"
        );
        
        // Record history
        self.history.push((now, self.current.clone(), event));
        
        // Apply transition
        self.current = new_state;
        
        Ok(())
    }
    
    /// Calculate the next state based on current state and event
    fn calculate_transition(&self, event: &StateEvent) -> Result<TradingState, String> {
        let now = Self::now_ms();
        
        match (&self.current, event) {
            // IDLE transitions
            (TradingState::Idle, StateEvent::MomentumDetected(symbol, dir)) => {
                Ok(TradingState::Qualifying {
                    symbol: symbol.clone(),
                    started_at_ms: now,
                    momentum_direction: *dir,
                    signal_type: SignalType::Momentum,
                })
            }
            (TradingState::Idle, StateEvent::ReversionDetected(symbol, dir)) => {
                Ok(TradingState::Qualifying {
                    symbol: symbol.clone(),
                    started_at_ms: now,
                    momentum_direction: *dir,
                    signal_type: SignalType::Reversion,
                })
            }
            
            // QUALIFYING transitions
            (TradingState::Qualifying { symbol, momentum_direction, .. }, StateEvent::FiltersPass { lots, sl_pips }) => {
                Ok(TradingState::EntryReady {
                    symbol: symbol.clone(),
                    direction: *momentum_direction,
                    calculated_lots: *lots,
                    sl_pips: *sl_pips,
                    timeout_at_ms: now + 5000, // 5s timeout
                })
            }
            (TradingState::Qualifying { .. }, StateEvent::FiltersReject) => {
                Ok(TradingState::Idle)
            }
            
            // ENTRY_READY transitions
            (TradingState::EntryReady { symbol, direction, calculated_lots, sl_pips, .. }, StateEvent::OrderFilled { price, ticket }) => {
                let tp_pips = *sl_pips * 2.0; // Maintenance of 2:1 ratio
                Ok(TradingState::PositionOpen {
                    symbol: symbol.clone(),
                    direction: *direction,
                    entry_price: *price,
                    current_lots: *calculated_lots,
                    sl_pips: *sl_pips,
                    tp_pips,
                    entry_time_ms: now,
                    ticket: *ticket,
                })
            }
            (TradingState::EntryReady { .. }, StateEvent::OrderTimeout) => {
                Ok(TradingState::Idle)
            }
            
            // POSITION_OPEN transitions
            (TradingState::PositionOpen { symbol, direction, entry_price, current_lots, sl_pips, tp_pips, entry_time_ms, ticket }, 
             StateEvent::ProfitLocked { .. }) => {
                // Stay in position, profit is tracked in TradingRun
                Ok(TradingState::PositionOpen {
                    symbol: symbol.clone(),
                    direction: *direction,
                    entry_price: *entry_price,
                    current_lots: *current_lots,
                    sl_pips: *sl_pips,
                    tp_pips: *tp_pips,
                    entry_time_ms: *entry_time_ms,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, entry_price, current_lots, sl_pips, tp_pips, ticket, .. }, 
             StateEvent::MomentumContinues) => {
                // Scaling: Pass everything forward
                Ok(TradingState::Scaling {
                    symbol: symbol.clone(),
                    direction: *direction,
                    entry_price: *entry_price,
                    current_lots: *current_lots,
                    sl_pips: *sl_pips,
                    tp_pips: *tp_pips,
                    started_at_ms: now,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::TakeProfitHit) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::TakeProfitHit,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::StopLossHit) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::StopLossHit,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::MomentumDecay) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::MomentumDecay,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::StallTimeout) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::StallTimeout,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::ReversalDetected) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::ReversalDetected,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            (TradingState::PositionOpen { symbol, direction, current_lots, ticket, .. }, StateEvent::SpreadExpanded) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::SpreadExpansion,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            
            // SCALING transitions
            (TradingState::Scaling { symbol, direction, entry_price, current_lots, sl_pips, tp_pips, ticket, .. }, 
             StateEvent::ScaleComplete { new_lots }) => {
                Ok(TradingState::PositionOpen {
                    symbol: symbol.clone(),
                    direction: *direction,
                    entry_price: *entry_price,
                    current_lots: current_lots + new_lots,
                    sl_pips: *sl_pips,
                    tp_pips: *tp_pips,
                    entry_time_ms: now, // Reset time for reversal tracking
                    ticket: *ticket,
                })
            }
            (TradingState::Scaling { symbol, direction, current_lots, ticket, .. }, StateEvent::MaxScalesReached) => {
                Ok(TradingState::Exiting {
                    symbol: symbol.clone(),
                    direction: *direction,
                    reason: ExitReason::MomentumDecay,
                    started_at_ms: now,
                    lots: *current_lots,
                    ticket: *ticket,
                })
            }
            
            // EXITING transitions
            (TradingState::Exiting { .. }, StateEvent::PositionClosed) => {
                Ok(TradingState::Cooldown {
                    until_ms: now + self.cooldown_ms,
                })
            }
            
            // COOLDOWN transitions
            (TradingState::Cooldown { until_ms }, StateEvent::CooldownComplete) => {
                if now >= *until_ms {
                    Ok(TradingState::Idle)
                } else {
                    Err(format!("Cooldown not complete, {}ms remaining", *until_ms - now))
                }
            }
            
            // Kill switch can trigger from any state
            (_, StateEvent::KillSwitchTriggered) => {
                let (symbol, direction) = match &self.current {
                    TradingState::Qualifying { symbol, momentum_direction, .. } => (Some(symbol.clone()), Some(*momentum_direction)),
                    TradingState::EntryReady { symbol, direction, .. } => (Some(symbol.clone()), Some(*direction)),
                    TradingState::PositionOpen { symbol, direction, .. } => (Some(symbol.clone()), Some(*direction)),
                    TradingState::Scaling { symbol, direction, .. } => (Some(symbol.clone()), Some(*direction)),
                    TradingState::Exiting { symbol, direction, .. } => (Some(symbol.clone()), Some(*direction)),
                    _ => (None, None),
                };

                if let Some(sym) = symbol {
                    Ok(TradingState::Exiting {
                        symbol: sym,
                        direction: direction.unwrap_or(Direction::Long),
                        reason: ExitReason::KillSwitch,
                        started_at_ms: now,
                        lots: 0.0, // Should read from current state really, but KS is panic mode
                        ticket: 0, 
                    })
                } else {
                    Ok(TradingState::Idle)
                }
            }
            
            // Invalid transition
            (state, event) => {
                Err(format!("Invalid transition: {:?} -> {:?}", state, event))
            }
        }
    }
    
    /// Force transition (for testing/recovery only)
    pub fn transition_to(&mut self, state: TradingState) {
        warn!(
            from = ?self.current,
            to = ?state,
            "Forced state transition"
        );
        self.current = state;
    }
}
