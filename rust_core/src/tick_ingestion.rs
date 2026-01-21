//! Tick Ingestion - High-performance market data processing
//! 
//! Handles:
//! - Sub-millisecond tick processing
//! - Buffering for Python strategy
//! - Latency measurement
//! - Spread calculation

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{warn};

use serde::{Deserialize, Serialize};

/// A single market tick
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tick {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    #[serde(default)]
    pub bid_volume: f64,
    #[serde(default)]
    pub ask_volume: f64,
    #[serde(rename = "time_msc")]
    pub timestamp_ms: u64,    // Market timestamp (ms)
    #[serde(default)]
    pub received_at_ms: u64,  // Local receive timestamp (ms)
    #[serde(default = "default_digits")]
    pub digits: u32,          // Number of decimal places
    #[serde(default = "default_point")]
    pub point: f64,           // Smallest price change
    #[serde(default = "default_tick_value")]
    pub tick_value: f64,      // Value of one tick in account currency
}

fn default_digits() -> u32 { 5 }
fn default_point() -> f64 { 0.00001 }
fn default_tick_value() -> f64 { 1.0 }

impl Tick {
    /// Multiplier to convert price to pips
    /// Generally 1.0 / (point * 10) for 5-digit symbols, or 1.0 / point for others
    pub fn pip_multiplier(&self) -> f64 {
        match self.digits {
            5 => 10000.0,
            4 => 10000.0,
            3 => 100.0,
            2 => 1.0,  // For Gold/Crypto, treat 1.0 move as 1 pip for easier logic
            _ => 1.0,
        }
    }

    /// Calculate spread in pips
    pub fn spread_pips(&self) -> f64 {
        (self.ask - self.bid) * self.pip_multiplier()
    }
    
    /// Calculate mid price
    pub fn mid_price(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
    
    /// Calculate latency from market to system
    pub fn latency_ms(&self) -> i64 {
        if self.received_at_ms >= self.timestamp_ms {
            (self.received_at_ms - self.timestamp_ms) as i64
        } else {
            0 // Clock skew or inaccurate timestamp
        }
    }
}

/// Tick buffer with rolling window
pub struct TickBuffer {
    ticks: VecDeque<Tick>,
    max_size: usize,
    total_received: AtomicU64,
    avg_spread: f64,
    spread_samples: usize,
}

impl TickBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            ticks: VecDeque::with_capacity(max_size),
            max_size,
            total_received: AtomicU64::new(0),
            avg_spread: 0.0,
            spread_samples: 0,
        }
    }
    
    /// Add a tick to the buffer
    pub fn push(&mut self, tick: Tick) {
        // Update spread average (exponential moving average)
        let spread = tick.spread_pips();
        if self.spread_samples == 0 {
            self.avg_spread = spread;
        } else {
            let alpha = 0.01; // Slow decay for stable average
            self.avg_spread = alpha * spread + (1.0 - alpha) * self.avg_spread;
        }
        self.spread_samples += 1;
        
        // Add to buffer
        if self.ticks.len() >= self.max_size {
            self.ticks.pop_front();
        }
        self.ticks.push_back(tick);
        
        self.total_received.fetch_add(1, Ordering::Relaxed);
    }
    
    /// Get the latest N ticks
    pub fn latest(&self, count: usize) -> Vec<&Tick> {
        let start = self.ticks.len().saturating_sub(count);
        self.ticks.iter().skip(start).collect()
    }
    
    /// Get current spread
    pub fn current_spread(&self) -> Option<f64> {
        self.ticks.back().map(|t| t.spread_pips())
    }
    
    /// Get average spread
    pub fn average_spread(&self) -> f64 {
        self.avg_spread
    }
    
    /// Check if spread is abnormal (> 1.5x average)
    pub fn is_spread_abnormal(&self, threshold_multiplier: f64) -> bool {
        if let Some(current) = self.current_spread() {
            current > self.avg_spread * threshold_multiplier
        } else {
            false
        }
    }
    
    /// Get current latency
    pub fn current_latency_ms(&self) -> Option<i64> {
        self.ticks.back().map(|t| t.latency_ms())
    }
    
    /// Calculate price velocity (pips per second)
    pub fn price_velocity(&self, window_ms: u64) -> Option<f64> {
        if self.ticks.len() < 2 {
            return None;
        }
        
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        let cutoff = now.saturating_sub(window_ms);
        
        let window_ticks: Vec<_> = self.ticks.iter()
            .filter(|t| t.received_at_ms >= cutoff)
            .collect();
        
        if window_ticks.len() < 2 {
            return None;
        }
        
        let first = window_ticks.first()?;
        let last = window_ticks.last()?;
        
        let multiplier = last.pip_multiplier();
        let price_change = (last.mid_price() - first.mid_price()) * multiplier; // In pips
        let time_diff = (last.received_at_ms - first.received_at_ms) as f64 / 1000.0;
        
        if time_diff > 0.001 {
            Some(price_change / time_diff)
        } else {
            None
        }
    }
    
    /// Calculate price acceleration
    pub fn price_acceleration(&self, window_ms: u64) -> Option<f64> {
        if self.ticks.len() < 10 {
            return None;
        }
        
        // Get velocity at two points and compare
        let half_window = window_ms / 2;
        
        let recent_velocity = self.price_velocity(half_window)?;
        let full_velocity = self.price_velocity(window_ms)?;
        
        Some(recent_velocity - full_velocity)
    }
    
    /// Get total ticks received
    pub fn total_received(&self) -> u64 {
        self.total_received.load(Ordering::Relaxed)
    }
    
    /// Get buffer size
    pub fn len(&self) -> usize {
        self.ticks.len()
    }
    
    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }
}

/// Tick ingestion service
pub struct TickIngestion {
    buffers: std::collections::HashMap<String, TickBuffer>,
    max_buffer_size: usize,
}

impl TickIngestion {
    pub fn new(max_buffer_size: usize) -> Self {
        Self {
            buffers: std::collections::HashMap::new(),
            max_buffer_size,
        }
    }
    
    /// Process an incoming tick
    pub fn process_tick(&mut self, tick: Tick) -> Result<(), String> {
        // Check latency - permissive warning only, do not reject data
        let latency = tick.latency_ms();
        if latency > 1000 { // Only warn on extreme anomalies (>1s)
            warn!(
                symbol = %tick.symbol,
                latency = latency,
                "Extreme tick latency detected! Check network or bridge bridge."
            );
        }
        
        // Get or create buffer
        let buffer = self.buffers
            .entry(tick.symbol.clone())
            .or_insert_with(|| TickBuffer::new(self.max_buffer_size));
        
        buffer.push(tick);
        
        Ok(())
    }
    
    /// Get buffer for a symbol
    pub fn get_buffer(&self, symbol: &str) -> Option<&TickBuffer> {
        self.buffers.get(symbol)
    }
    
    /// Get mutable buffer for a symbol
    pub fn get_buffer_mut(&mut self, symbol: &str) -> Option<&mut TickBuffer> {
        self.buffers.get_mut(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn create_test_tick(bid: f64, ask: f64) -> Tick {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        Tick {
            symbol: "EURUSD".to_string(),
            bid,
            ask,
            bid_volume: 1000.0,
            ask_volume: 1000.0,
            timestamp_ms: now,
            received_at_ms: now,
        }
    }
    
    #[test]
    fn test_spread_calculation() {
        let tick = create_test_tick(1.08500, 1.08510);
        assert!((tick.spread_pips() - 1.0).abs() < 0.01); // 1 pip spread
    }
    
    #[test]
    fn test_buffer_rolling() {
        let mut buffer = TickBuffer::new(3);
        
        buffer.push(create_test_tick(1.0, 1.1));
        buffer.push(create_test_tick(1.1, 1.2));
        buffer.push(create_test_tick(1.2, 1.3));
        buffer.push(create_test_tick(1.3, 1.4)); // Should push out first
        
        assert_eq!(buffer.len(), 3);
    }
}
