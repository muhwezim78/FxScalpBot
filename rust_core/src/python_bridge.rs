//! Python Bridge - Communication layer between Rust and Python
//! 
//! Uses PyO3 for Python interop. Python handles:
//! - Momentum detection
//! - Volatility filtering
//! - Trade qualification
//!
//! When compiled without the `python` feature, uses fallback implementations.

#[allow(unused_imports)]
use tracing::{info, error, debug};

use serde::{Deserialize, Serialize};

/// Momentum signal from Python
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MomentumSignal {
    pub detected: bool,
    pub direction: i8,          // 1 = long, -1 = short, 0 = none
    pub strength: f64,          // 0.0 to 1.0
    pub velocity: f64,          // Pips per second
    pub acceleration: f64,      // Velocity change
    pub quality: Option<ImpulseQuality>,
    pub ema_slope: f64,
    pub volume_surge: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ImpulseQuality {
    pub body_ratio: f64,
    pub close_pct: f64,
    pub range: f64,
}

/// Mean reversion signal from Python
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ReversionSignal {
    pub detected: bool,
    pub direction: i8,          // 1 = buy (bounce from bottom), -1 = sell (bounce from top)
    pub z_score: f64,
    pub strength: f64,
    pub mean: f64,
    pub std: f64,
}

/// Trade qualification result from Python
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QualificationResult {
    pub qualified: bool,
    pub rejection_reason: Option<String>,
    pub suggested_lots: f64,
    pub confidence: f64,
}

/// Volatility regime from Python
#[derive(Debug, Clone, PartialEq)]
pub enum VolatilityRegime {
    Low,
    Normal,
    High,
    Extreme,
}

/// Fallback implementations for when Python is not available
/// These are used for testing and simulation
pub mod fallback {
    use super::*;
    
    /// Simple momentum detection without Python
    pub fn detect_momentum_simple(
        prices: &[f64],
        window: usize,
    ) -> MomentumSignal {
        if prices.len() < window {
            return MomentumSignal {
                detected: false,
                direction: 0,
                strength: 0.0,
                velocity: 0.0,
                acceleration: 0.0,
                quality: None,
                ema_slope: 0.0,
                volume_surge: false,
            };
        }
        
        let recent = &prices[prices.len() - window..];
        let first = recent[0];
        let last = *recent.last().unwrap();
        
        let change = last - first;
        let velocity = change * 10000.0 / (window as f64 / 10.0); // Rough pips/sec
        
        // Calculate acceleration (change in velocity)
        let mid = window / 2;
        let first_half_change = recent[mid] - recent[0];
        let second_half_change = last - recent[mid];
        let acceleration = (second_half_change - first_half_change) * 10000.0;
        
        let strength = velocity.abs().min(10.0) / 10.0;
        let detected = velocity.abs() > 0.5 && acceleration.signum() == velocity.signum();
        
        MomentumSignal {
            detected,
            direction: if change > 0.0 { 1 } else if change < 0.0 { -1 } else { 0 },
            strength,
            velocity,
            acceleration,
            quality: None,
            ema_slope: 0.0,
            volume_surge: false, // Can't detect without volume data
        }
    }
    
    /// Simple qualification without Python
    pub fn qualify_trade_simple(
        momentum: &MomentumSignal,
        spread: f64,
        avg_spread: f64,
        latency_ms: u64,
    ) -> QualificationResult {
        // Check spread
        if spread > avg_spread * 1.5 {
            return QualificationResult {
                qualified: false,
                rejection_reason: Some("spread_too_wide".to_string()),
                suggested_lots: 0.0,
                confidence: 0.0,
            };
        }
        
        // Check latency
        if latency_ms > 50 {
            return QualificationResult {
                qualified: false,
                rejection_reason: Some("latency_too_high".to_string()),
                suggested_lots: 0.0,
                confidence: 0.0,
            };
        }
        
        // Check momentum strength
        if !momentum.detected || momentum.strength < 0.3 {
            return QualificationResult {
                qualified: false,
                rejection_reason: Some("momentum_too_weak".to_string()),
                suggested_lots: 0.0,
                confidence: 0.0,
            };
        }
        
        QualificationResult {
            qualified: true,
            rejection_reason: None,
            suggested_lots: 0.01,
            confidence: momentum.strength,
        }
    }
    
    /// Simple volatility regime detection
    pub fn get_volatility_regime(atr: f64) -> VolatilityRegime {
        if atr > 50.0 {
            VolatilityRegime::Extreme
        } else if atr > 30.0 {
            VolatilityRegime::High
        } else if atr < 5.0 {
            VolatilityRegime::Low
        } else {
            VolatilityRegime::Normal
        }
    }
    
    /// Calculate scale lots without Python
    pub fn calculate_scale_lots(
        locked_profit: f64,
        current_lots: f64,
        scale_number: u8,
        max_lots: f64,
    ) -> f64 {
        const MAX_SCALES: u8 = 3;
        const MIN_SCALE_PROFIT: f64 = 1.0;
        const LOT_PER_DOLLAR: f64 = 0.001;
        const MAX_SCALE_LOTS: f64 = 0.1;
        
        if scale_number >= MAX_SCALES || locked_profit < MIN_SCALE_PROFIT {
            return 0.0;
        }
        
        let available = locked_profit * 0.5;
        let scale_factor = match scale_number {
            0 => 0.3,
            1 => 0.4,
            2 => 0.5,
            _ => 0.0,
        };
        
        let base_lots = available * LOT_PER_DOLLAR * scale_factor;
        let capped = base_lots.min(MAX_SCALE_LOTS);
        let remaining_capacity = max_lots - current_lots;
        
        capped.min(remaining_capacity).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::fallback::*;
    
    #[test]
    fn test_momentum_detection_up() {
        // Create an ACCELERATING price sequence
        let mut prices = Vec::new();
        for i in 0..50 {
            let p = 1.08500 + (i as f64 * 0.0001) + (i as f64 * i as f64 * 0.000001);
            prices.push(p);
        }
        let signal = detect_momentum_simple(&prices, 50);
        
        assert!(signal.detected);
        assert_eq!(signal.direction, 1); // Long
        assert!(signal.velocity > 0.0);
        assert!(signal.acceleration > 0.0);
    }
    
    #[test]
    fn test_momentum_detection_down() {
        // Create an ACCELERATING price sequence (downwards)
        let mut prices = Vec::new();
        for i in 0..50 {
            let p = 1.08500 - (i as f64 * 0.0001) - (i as f64 * i as f64 * 0.000001);
            prices.push(p);
        }
        let signal = detect_momentum_simple(&prices, 50);
        
        assert!(signal.detected);
        assert_eq!(signal.direction, -1); // Short
        assert!(signal.velocity < 0.0);
        assert!(signal.acceleration < 0.0);
    }
    
    #[test]
    fn test_qualification_spread_reject() {
        let momentum = super::MomentumSignal {
            detected: true,
            direction: 1,
            strength: 0.8,
            velocity: 5.0,
            acceleration: 1.0,
            quality: None,
            ema_slope: 0.0,
            volume_surge: true,
        };
        
        let result = qualify_trade_simple(&momentum, 3.0, 1.0, 10);
        assert!(!result.qualified);
        assert_eq!(result.rejection_reason, Some("spread_too_wide".to_string()));
    }
    
    #[test]
    fn test_scale_calculation() {
        // No locked profit = no scale
        assert_eq!(calculate_scale_lots(0.5, 0.01, 0, 0.1), 0.0);
        
        // First scale with $10 locked
        let lots = calculate_scale_lots(10.0, 0.01, 0, 0.1);
        assert!(lots > 0.0);
        
        // Max scales reached
        assert_eq!(calculate_scale_lots(100.0, 0.05, 3, 0.1), 0.0);
    }
}
