"""
Momentum Detector

Detects genuine momentum bursts vs market noise.
Uses tick velocity, acceleration, and volume surge indicators.

CRITICAL: This module only DETECTS momentum.
All trading decisions and risk management are in Rust.
"""

from dataclasses import dataclass
from typing import List, Dict, Any
import numpy as np


def calculate_atr(highs: np.ndarray, lows: np.ndarray, closes: np.ndarray, period: int) -> float:
    """Calculates current ATR."""
    if len(closes) < period + 1:
        return 0.0
    tr = np.maximum(highs[1:] - lows[1:], 
                    np.maximum(abs(highs[1:] - closes[:-1]), 
                               abs(lows[1:] - closes[:-1])))
    return float(np.mean(tr[-period:]))

def analyze_acceptance(mid_prices: np.ndarray, direction: int) -> bool:
    """
    Implements 'Breakout Acceptance' (Rule 4).
    Checks if the last N ticks traded back below/above the breakout level.
    """
    if len(mid_prices) < 6: # Need at least ACCEPTANCE_TICKS + 1
        return False
    
    # Check last 5 ticks against the tick before them
    recent = mid_prices[-5:]
    level = mid_prices[-6]
    
    if direction == 1: # Long
        return all(p >= level for p in recent)
    elif direction == -1: # Short
        return all(p <= level for p in recent)
    return False


@dataclass
class MomentumSignal:
    """Result of momentum analysis."""
    detected: bool
    direction: int  # 1 = long, -1 = short, 0 = none
    strength: float  # 0.0 to 1.0
    velocity: float  # Pips per second
    acceleration: float  # Velocity change
    volume_surge: bool


# Configuration - High Precision Impulse Thresholds (Balanced for Frequency)
VELOCITY_THRESHOLD_POINTS = 6.0 # Reduced from 8.0 for better sensitivity
STRENGTH_THRESHOLD = 0.4       # Reduced from 0.5
IMPULSE_BODY_RATIO = 0.60      # 60% of candle (from 65%)
IMPULSE_CLOSE_PERCENT = 0.25   # Close must be in top/bottom 25% (from 15%)
EMA_SLOPE_THRESHOLD_POINTS = 3.0 # Reduced from 5.0
ACCEPTANCE_TICKS = 3           # Reduced from 5 for faster sub-second entry
WINDOW_TICKS = 30              # Analysis window
EMA_PERIOD = 20                
ATR_PERIOD = 20                


def calculate_tick_velocity(mid_prices: np.ndarray, timestamps: np.ndarray) -> float:
    """
    Calculate price velocity (pips per second).
    
    Args:
        mid_prices: Array of mid prices
        timestamps: Array of timestamps in milliseconds
        
    Returns:
        Velocity in pips per second
    """
    if len(mid_prices) < 2:
        return 0.0
    
    price_change = (mid_prices[-1] - mid_prices[0]) * 10000  # To pips
    time_diff = (timestamps[-1] - timestamps[0]) / 1000  # To seconds
    
    if time_diff <= 0:
        return 0.0
    
    return price_change / time_diff


def calculate_acceleration(
    mid_prices: np.ndarray, 
    timestamps: np.ndarray
) -> float:
    """
    Calculate price acceleration (change in velocity).
    
    Positive = speeding up in direction of movement
    Negative = slowing down
    """
    if len(mid_prices) < 10:
        return 0.0
    
    # Split into two halves
    mid_idx = len(mid_prices) // 2
    
    first_half = mid_prices[:mid_idx]
    first_times = timestamps[:mid_idx]
    
    second_half = mid_prices[mid_idx:]
    second_times = timestamps[mid_idx:]
    
    v1 = calculate_tick_velocity(first_half, first_times)
    v2 = calculate_tick_velocity(second_half, second_times)
    
    return v2 - v1


def calculate_ema(data: np.ndarray, period: int) -> np.ndarray:
    """Fast EMA calculation using numpy."""
    alpha = 2 / (period + 1)
    ema = np.zeros_like(data)
    ema[0] = data[0]
    for i in range(1, len(data)):
        ema[i] = data[i] * alpha + ema[i-1] * (1 - alpha)
    return ema

def analyze_impulse_quality(mid_prices: np.ndarray) -> Dict[str, float]:
    """
    Analyzes the 'quality' of the current price action impulse.
    Checks body vs range and close location.
    """
    high = np.max(mid_prices)
    low = np.min(mid_prices)
    rng = high - low
    
    if rng == 0:
        return {"body_ratio": 0.0, "close_pct": 0.5}
    
    # Body is defined as dist between start and end of window
    body = abs(mid_prices[-1] - mid_prices[0])
    body_ratio = body / rng
    
    # Close location: 0.0 = at high, 1.0 = at low (for sell), or 0.0 = at low, 1.0 = at high (for buy)
    # We normalize so 0.0 means 'right at the edge of the move'
    if mid_prices[-1] > mid_prices[0]: # Long impulse
        close_pct = (high - mid_prices[-1]) / rng
    else: # Short impulse
        close_pct = (mid_prices[-1] - low) / rng
        
    return {
        "body_ratio": body_ratio,
        "close_pct": close_pct,
        "range": rng
    }


def calculate_strength(velocity: float, acceleration: float) -> float:
    """
    Calculate momentum strength (0.0 to 1.0).
    
    Combines velocity and acceleration into single metric.
    """
    # Normalize velocity (assume 10 pips/sec is very strong)
    vel_component = min(abs(velocity) / 10.0, 1.0)
    
    # Normalize acceleration (positive acceleration adds to strength)
    if velocity != 0:
        # Acceleration should be in same direction as velocity
        same_direction = (acceleration * velocity) > 0
        acc_component = min(abs(acceleration) / 5.0, 0.5) if same_direction else 0
    else:
        acc_component = 0
    
    return min(vel_component + acc_component, 1.0)


def detect_momentum(ticks: List[Dict[str, Any]]) -> Dict[str, Any]:
    """
    Main momentum detection function called from Rust.
    
    Args:
        ticks: List of tick dicts with 'bid', 'ask', 'timestamp' keys
        
    Returns:
        Dict with momentum signal details
    """
    if len(ticks) < WINDOW_TICKS:
        return {
            "detected": False,
            "direction": 0,
            "strength": 0.0,
            "velocity": 0.0,
            "acceleration": 0.0,
            "volume_surge": False,
        }
    
    # Convert to numpy arrays
    recent = ticks[-WINDOW_TICKS:]
    bids = np.array([t.get("bid", 0.0) for t in recent])
    asks = np.array([t.get("ask", 0.0) for t in recent])
    # Consistency fix: Rust uses time_msc (MT5 naming), Python was expecting timestamp
    timestamps = np.array([t.get("time_msc", t.get("timestamp", 0)) for t in recent])
    
    # Extract Mid prices and Points
    mid_prices = (bids + asks) / 2
    point = ticks[0].get("point", 0.00001)
    
    # Calculate thresholds based on symbol price scale (points)
    velocity_threshold = VELOCITY_THRESHOLD_POINTS * point
    ema_slope_threshold = EMA_SLOPE_THRESHOLD_POINTS * point
    
    # Calculate metrics
    velocity = calculate_tick_velocity(mid_prices, timestamps)
    acceleration = calculate_acceleration(mid_prices, timestamps)
    
    # 1. Impulse Quality Gate (Body vs Range)
    quality = analyze_impulse_quality(mid_prices)
    impulse_valid = (quality["body_ratio"] >= IMPULSE_BODY_RATIO and 
                     quality["close_pct"] <= IMPULSE_CLOSE_PERCENT)
    
    # 2. Instant EMA Slope (Trend derivative)
    ema = calculate_ema(mid_prices, EMA_PERIOD)
    ema_slope = ema[-1] - ema[-4] if len(ema) >= 4 else 0.0
    
    # 3. ATR Expansion (Rule 9)
    # Simulate high/low/close from ticks for ATR
    closes = mid_prices
    highs = mid_prices + (point * 1.0) # Conservative estimate
    lows = mid_prices - (point * 1.0)
    atr_now = calculate_atr(highs, lows, closes, ATR_PERIOD)
    atr_prev = calculate_atr(highs[:-5], lows[:-5], closes[:-5], ATR_PERIOD)
    volatility_expanding = atr_now > atr_prev * 1.02 if atr_prev > 0 else True

    # 4. Breakout Acceptance (Rule 4)
    direction = 1 if velocity > 0 else -1 if velocity < 0 else 0
    
    # Acceptance: Check last 3 ticks against the tick before them
    acceptance_valid = False
    if len(mid_prices) > 4:
        recent = mid_prices[-3:]
        level = mid_prices[-4]
        if direction == 1: acceptance_valid = all(p >= level for p in recent)
        elif direction == -1: acceptance_valid = all(p <= level for p in recent)
    
    trend_valid = (ema_slope > ema_slope_threshold and direction == 1) or \
                  (ema_slope < -ema_slope_threshold and direction == -1)

    # 5. Volume Gate
    total_vols = np.array([t.get("bid_volume", t.get("volume", 0)) for t in recent])
    vol_ratio = np.mean(total_vols[-5:]) / np.mean(total_vols[:-5]) if len(total_vols) > 10 else 1.0
    volume_valid = vol_ratio >= 1.5

    # ZERO LAG STATE GATE
    # Consolidates all rules into one instant boolean check
    detected = (
        abs(velocity) > velocity_threshold and
        impulse_valid and
        trend_valid and
        volume_valid and
        volatility_expanding and
        acceptance_valid
    )
    
    return {
        "detected": bool(detected),
        "direction": int(direction),
        "strength": float(calculate_strength(velocity, acceleration)),
        "velocity": float(velocity),
        "acceleration": float(acceleration),
        "quality": quality,
        "ema_slope": float(ema_slope),
        "volatility_expanding": bool(volatility_expanding),
        "acceptance_valid": bool(acceptance_valid),
        "volume_surge": bool(volume_valid),
    }


def analyze_momentum_decay(
    velocities: List[float],
    window: int = 5
) -> bool:
    """
    Detect if momentum is decaying.
    
    Returns True if velocity is consistently decreasing.
    """
    if len(velocities) < window:
        return False
    
    recent = velocities[-window:]
    
    # Check if each velocity is smaller (in magnitude) than previous
    for i in range(1, len(recent)):
        if abs(recent[i]) >= abs(recent[i-1]):
            return False
    
    return True


# Tests
if __name__ == "__main__":
    # Test upward momentum
    ticks = [
        {"bid": 1.08500 + i * 0.0001, "ask": 1.08510 + i * 0.0001, "time_msc": i * 100}
        for i in range(60)
    ]
    
    result = detect_momentum(ticks)
    print(f"Upward momentum test:")
    print(f"  Detected: {result['detected']}")
    print(f"  Direction: {result['direction']} (expected: 1)")
    print(f"  Velocity: {result['velocity']:.2f} pips/sec")
    print(f"  Strength: {result['strength']:.2f}")
    
    # Test no momentum (flat)
    flat_ticks = [
        {"bid": 1.08500, "ask": 1.08510, "time_msc": i * 100}
        for i in range(60)
    ]
    
    result = detect_momentum(flat_ticks)
    print(f"\nFlat market test:")
    print(f"  Detected: {result['detected']} (expected: False)")
    print(f"  Direction: {result['direction']} (expected: 0)")
