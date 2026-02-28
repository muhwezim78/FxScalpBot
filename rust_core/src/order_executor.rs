//! Order Executor - Atomic order placement and management
//! 
//! Responsibilities:
//! - Order submission with slippage protection
//! - Order cancellation
//! - Fill tracking
//! - Position management

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

/// Order side
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
}

/// Order status
#[derive(Debug, Clone, PartialEq)]
pub enum OrderStatus {
    Pending,
    Submitted,
    PartiallyFilled { filled_lots: f64 },
    Filled { fill_price: f64, slippage: f64 },
    Cancelled,
    Rejected { reason: String },
    Expired,
}

/// An order
#[derive(Debug, Clone)]
pub struct Order {
    pub id: u64,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub lots: f64,
    pub price: Option<f64>,         // For limit/stop orders
    pub slippage_tolerance: f64,    // Maximum acceptable slippage in pips
    pub status: OrderStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Fill information
#[derive(Debug, Clone)]
pub struct Fill {
    pub order_id: u64,
    pub symbol: String,
    pub side: OrderSide,
    pub lots: f64,
    pub price: f64,
    pub requested_price: Option<f64>,
    pub slippage_pips: f64,
    pub timestamp_ms: u64,
}

/// Open position
#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub side: OrderSide,
    pub lots: f64,
    pub entry_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub opened_at_ms: u64,
    pub point: f64,      // Added for accurate P&L
    pub tick_value: f64, // Added for accurate P&L
    pub digits: u32,     // Added for pip calculation
}

impl Position {
    /// Update P&L based on current price
    pub fn update_pnl(&mut self, bid: f64, ask: f64) {
        self.current_price = match self.side {
            OrderSide::Buy => bid,   // Close price for longs
            OrderSide::Sell => ask,  // Close price for shorts
        };
        
        let price_diff = match self.side {
            OrderSide::Buy => self.current_price - self.entry_price,
            OrderSide::Sell => self.entry_price - self.current_price,
        };
        
        // Exact P&L Formula: (PriceDiff / Point) * TickValue * Lots
        if self.point > 0.0 {
            self.unrealized_pnl = (price_diff / self.point) * self.tick_value * self.lots;
        } else {
            // Fallback to pips if point is missing
            let multiplier = match self.digits {
                5 | 4 => 10000.0,
                3 => 100.0,
                _ => 1.0,
            };
            self.unrealized_pnl = price_diff * multiplier * self.lots * 10.0;
        }
    }
}

/// Order executor
pub struct OrderExecutor {
    next_order_id: AtomicU64,
    pending_orders: HashMap<u64, Order>,
    fills: Vec<Fill>,
    positions: HashMap<String, Position>,
    max_slippage_pips: f64,
}

impl OrderExecutor {
    pub fn new(max_slippage_pips: f64) -> Self {
        Self {
            next_order_id: AtomicU64::new(1),
            pending_orders: HashMap::new(),
            fills: Vec::new(),
            positions: HashMap::new(),
            max_slippage_pips,
        }
    }
    
    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }
    
    /// Create a new market order
    pub fn create_market_order(
        &mut self,
        symbol: &str,
        side: OrderSide,
        lots: f64,
    ) -> Order {
        let id = self.next_order_id.fetch_add(1, Ordering::SeqCst);
        let now = Self::now_ms();
        
        let order = Order {
            id,
            symbol: symbol.to_string(),
            side,
            order_type: OrderType::Market,
            lots,
            price: None,
            slippage_tolerance: self.max_slippage_pips,
            status: OrderStatus::Pending,
            created_at_ms: now,
            updated_at_ms: now,
        };
        
        info!(
            order_id = id,
            symbol = symbol,
            side = ?side,
            lots = lots,
            "Market order created"
        );
        
        order
    }
    
    /// Submit an order (would send to broker in production)
    pub fn submit_order(&mut self, mut order: Order) -> Result<u64, String> {
        // Validate order
        if order.lots <= 0.0 {
            return Err("Invalid lot size".to_string());
        }
        
        order.status = OrderStatus::Submitted;
        order.updated_at_ms = Self::now_ms();
        
        let id = order.id;
        self.pending_orders.insert(id, order.clone());
        
        info!(order_id = id, symbol = %order.symbol, lots = order.lots, "Order submitted to internal queue");
        
        Ok(id)
    }

    /// Pulls all orders that need to be sent to the bridge
    pub fn pull_pending_submissions(&mut self) -> Vec<Order> {
        // For simplicity, we'll return all "Submitted" orders that haven't been "Processed" by bridge yet.
        // In a real system, we'd use a more robust state tracking.
        // For now, let's just return all orders in pending_orders.
        self.pending_orders.values().cloned().collect()
    }

    /// Updates order status based on bridge response
    pub fn update_from_bridge(&mut self, order_id: u64, status: OrderStatus) {
        if let Some(order) = self.pending_orders.get_mut(&order_id) {
            order.status = status;
            order.updated_at_ms = Self::now_ms();
        }
    }
    
    /// Process a fill from broker
    pub fn process_fill(
        &mut self,
        order_id: u64,
        fill_price: f64,
        requested_price: Option<f64>,
        point: f64,
        tick_value: f64,
        digits: u32,
    ) -> Result<Fill, String> {
        let order = self.pending_orders
            .remove(&order_id)
            .ok_or_else(|| format!("Order {} not found", order_id))?;
        
        // Calculate slippage
        let slippage_pips = if let Some(req_price) = requested_price {
            let multiplier = match digits {
                5 | 4 => 10000.0,
                3 => 100.0,
                _ => 1.0,
            };
            (fill_price - req_price).abs() * multiplier
        } else {
            0.0
        };
        
        // Check slippage tolerance
        if slippage_pips > order.slippage_tolerance {
            warn!(
                order_id = order_id,
                slippage = slippage_pips,
                tolerance = order.slippage_tolerance,
                "Slippage exceeded tolerance"
            );
        }
        
        let now = Self::now_ms();
        let fill = Fill {
            order_id,
            symbol: order.symbol.clone(),
            side: order.side,
            lots: order.lots,
            price: fill_price,
            requested_price,
            slippage_pips,
            timestamp_ms: now,
        };
        
        // Update position
        self.update_position(&fill, point, tick_value, digits);
        
        // Record fill
        self.fills.push(fill.clone());
        
        info!(
            order_id = order_id,
            price = fill_price,
            slippage = slippage_pips,
            "Order filled"
        );
        
        Ok(fill)
    }
    
    /// Update position based on fill
    fn update_position(&mut self, fill: &Fill, point: f64, tick_value: f64, digits: u32) {
        let now = Self::now_ms();
        if let Some(existing) = self.positions.get_mut(&fill.symbol) {
            if existing.side == fill.side {
                // Adding to position
                let total_lots = existing.lots + fill.lots;
                let avg_price = (existing.entry_price * existing.lots 
                    + fill.price * fill.lots) / total_lots;
                existing.lots = total_lots;
                existing.entry_price = avg_price;
                // Update metadata in case it changed
                existing.point = point;
                existing.tick_value = tick_value;
                existing.digits = digits;
            } else {
                // Reducing or closing position
                if fill.lots >= existing.lots {
                    // Position closed (or reversed)
                    let remaining = fill.lots - existing.lots;
                    if remaining > 0.0 {
                        // Reversed
                        *existing = Position {
                            symbol: fill.symbol.clone(),
                            side: fill.side,
                            lots: remaining,
                            entry_price: fill.price,
                            current_price: fill.price,
                            unrealized_pnl: 0.0,
                            opened_at_ms: now,
                            point,
                            tick_value,
                            digits,
                        };
                    } else {
                        // Closed
                        self.positions.remove(&fill.symbol);
                    }
                } else {
                    // Partial close
                    existing.lots -= fill.lots;
                }
            }
        } else {
            // New position
            self.positions.insert(fill.symbol.clone(), Position {
                symbol: fill.symbol.clone(),
                side: fill.side,
                lots: fill.lots,
                entry_price: fill.price,
                current_price: fill.price,
                unrealized_pnl: 0.0,
                opened_at_ms: now,
                point,
                tick_value,
                digits,
            });
        }
    }
    
    // ... [rest of methods largely unchanged]
    
    /// Cancel an order
    pub fn cancel_order(&mut self, order_id: u64) -> Result<(), String> {
        if let Some(order) = self.pending_orders.get_mut(&order_id) {
            order.status = OrderStatus::Cancelled;
            order.updated_at_ms = Self::now_ms();
            info!(order_id = order_id, "Order cancelled");
            Ok(())
        } else {
            Err(format!("Order {} not found", order_id))
        }
    }
    
    /// Cancel all pending orders
    pub fn cancel_all(&mut self) {
        let now = Self::now_ms();
        for order in self.pending_orders.values_mut() {
            order.status = OrderStatus::Cancelled;
            order.updated_at_ms = now;
        }
        warn!(count = self.pending_orders.len(), "All orders cancelled");
    }
    
    /// Close all positions at market
    pub fn close_all_positions(&mut self) -> Vec<Order> {
        let mut close_orders = Vec::new();
        
        // Collect positions first to avoid borrow conflict with create_market_order
        let positions: Vec<Position> = self.positions.values().cloned().collect();
        
        for position in positions {
            let close_side = match position.side {
                OrderSide::Buy => OrderSide::Sell,
                OrderSide::Sell => OrderSide::Buy,
            };
            
            let order = self.create_market_order(
                &position.symbol,
                close_side,
                position.lots,
            );
            close_orders.push(order);
        }
        
        warn!(count = close_orders.len(), "Closing all positions");
        close_orders
    }
    
    /// Get current position for a symbol
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }
    
    /// Get all positions
    pub fn get_all_positions(&self) -> &HashMap<String, Position> {
        &self.positions
    }
    
    /// Check if there are any open positions
    pub fn has_open_positions(&self) -> bool {
        !self.positions.is_empty()
    }
    
    /// Get total unrealized P&L
    pub fn total_unrealized_pnl(&self) -> f64 {
        self.positions.values().map(|p| p.unrealized_pnl).sum()
    }
}
