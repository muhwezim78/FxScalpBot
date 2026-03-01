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


# Configuration - Ultra Aggressive Scalping Mode
VELOCITY_THRESHOLD_POINTS = 2.0 # Very low: enters on minor movement
STRENGTH_THRESHOLD = 0.2       # Minimal strength required
IMPULSE_BODY_RATIO = 0.30      # Permits large wicks/high noise
IMPULSE_CLOSE_PERCENT = 0.50   # Permits entry even if price pulls back 50%
EMA_SLOPE_THRESHOLD_POINTS = 0.5 # Minimal trend alignment
ACCEPTANCE_TICKS = 1           # Instant entry on first confirmation tick
WINDOW_TICKS = 30              # Analysis window
EMA_PERIOD = 20                
ATR_PERIOD = 20                


# ── Asset Profile System ─────────────────────────────────────────────
# Scalable architecture: define per-asset-class filter behavior.
# volume_required: whether volume surge gate is mandatory (False = auto-pass)
# breakout_tolerance_points: how many points below the breakout level is still accepted
ASSET_PROFILES = {
    "FOREX":  {"volume_required": True,  "breakout_tolerance_points": 0},
    "CRYPTO": {"volume_required": False, "breakout_tolerance_points": 2},
    "METALS": {"volume_required": True,  "breakout_tolerance_points": 1},
}

CRYPTO_KEYWORDS = ["BTC", "ETH", "XRP", "LTC", "SOL", "DOGE", "ADA", "BNB"]
METALS_KEYWORDS = ["XAU", "XAG", "GOLD", "SILVER"]


def get_asset_profile(symbol: str) -> dict:
    """Determine asset class profile from symbol name."""
    sym = symbol.upper()
    if any(k in sym for k in CRYPTO_KEYWORDS):
        return ASSET_PROFILES["CRYPTO"]
    if any(k in sym for k in METALS_KEYWORDS):
        return ASSET_PROFILES["METALS"]
    return ASSET_PROFILES["FOREX"]


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


def detect_momentum(ticks: List[Dict[str, Any]], symbol: str = "") -> Dict[str, Any]:
    """
    Main momentum detection function called from Rust.
    
    Args:
        ticks: List of tick dicts with 'bid', 'ask', 'timestamp' keys
        symbol: Trading symbol (e.g. 'BTCUSDm') for asset-class-aware filtering
        
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
    
    # Resolve asset profile for adaptive filtering
    profile = get_asset_profile(symbol)
    
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
    # Use real high/low data from bid/ask spreads instead of estimating
    closes = mid_prices
    highs = asks
    lows = bids
    atr_now = calculate_atr(highs, lows, closes, ATR_PERIOD)
    atr_prev = calculate_atr(highs[:-5], lows[:-5], closes[:-5], ATR_PERIOD)
    volatility_expanding = atr_now >= atr_prev if atr_prev > 0 else True  # Relaxed: any expansion counts

    # 4. Breakout Acceptance (Rule 4) — with asset-class tolerance
    direction = 1 if velocity > 0 else -1 if velocity < 0 else 0
    breakout_tolerance = profile["breakout_tolerance_points"] * point
    
    # Acceptance: Check last 2 ticks against the tick before them (with tolerance)
    acceptance_valid = False
    if len(mid_prices) > 3:
        recent_mids = mid_prices[-2:]
        level = mid_prices[-3]
        if direction == 1: acceptance_valid = all(p >= level - breakout_tolerance for p in recent_mids)
        elif direction == -1: acceptance_valid = all(p <= level + breakout_tolerance for p in recent_mids)
    
    trend_valid = (ema_slope > ema_slope_threshold and direction == 1) or \
                  (ema_slope < -ema_slope_threshold and direction == -1)

    # 5. Volume Gate — adaptive per asset class
    total_vols = np.array([t.get("bid_volume", t.get("volume", 0)) for t in recent])
    vol_mean_prev = np.mean(total_vols[:-5]) if len(total_vols) > 10 else 0.0
    vol_ratio = np.mean(total_vols[-5:]) / vol_mean_prev if vol_mean_prev > 0 else 1.0
    
    if not profile["volume_required"] or np.sum(total_vols) == 0:
        # Crypto or broker reports zero volume: auto-pass
        volume_valid = True
    else:
        volume_valid = vol_ratio >= 1.0

    # ZERO LAG STATE GATE
    detected = (
        abs(velocity) > velocity_threshold and
        impulse_valid and
        trend_valid and
        volume_valid and
        volatility_expanding and
        acceptance_valid
    )
    
    # ── Professional Debug Logging ────────────────────────────────────
    # Log every gate result so silent blockers are immediately visible.
    import logging
    _log = logging.getLogger(__name__)
    
    asset_class = "CRYPTO" if not profile["volume_required"] else "FOREX/METALS"
    gate_results = {
        "Velocity":    f"{'PASS' if abs(velocity) > velocity_threshold else 'FAIL'} ({abs(velocity):.4f} vs {velocity_threshold:.6f})",
        "Impulse":     f"{'PASS' if impulse_valid else 'FAIL'} (body={quality['body_ratio']:.2f}>={IMPULSE_BODY_RATIO}, close={quality.get('close_pct',0):.2f}<={IMPULSE_CLOSE_PERCENT})",
        "EMA_Slope":   f"{'PASS' if trend_valid else 'FAIL'} (slope={ema_slope:.6f}, threshold={ema_slope_threshold:.6f}, dir={direction})",
        "Volume":      f"{'BYPASS' if not profile['volume_required'] or np.sum(total_vols) == 0 else ('PASS' if volume_valid else 'FAIL')} (ratio={vol_ratio:.2f})",
        "Volatility":  f"{'PASS' if volatility_expanding else 'FAIL'} (atr_now={atr_now:.6f}, atr_prev={atr_prev:.6f})",
        "Breakout":    f"{'PASS' if acceptance_valid else 'FAIL'} (tolerance={profile['breakout_tolerance_points']}pts)",
    }
    
    if detected:
        _log.info(f"[{symbol}] *** MOMENTUM DETECTED *** dir={direction} strength={calculate_strength(velocity, acceleration):.2f} | {asset_class}")
    else:
        # Only log gate details periodically to avoid spam (every ~50th rejection)
        import random
        if random.random() < 0.02:  # ~2% of rejections get full gate dump
            gates_str = " | ".join(f"{k}:{v}" for k, v in gate_results.items())
            _log.info(f"[{symbol}] Gates: {gates_str}")
    
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
        "rejection_reasons": _get_rejection_reasons(
            velocity, velocity_threshold, impulse_valid, trend_valid, 
            volume_valid, volatility_expanding, acceptance_valid
        ),
    }


def _get_rejection_reasons(
    velocity: float, velocity_threshold: float, impulse_valid: bool,
    trend_valid: bool, volume_valid: bool, volatility_expanding: bool,
    acceptance_valid: bool
) -> list:
    """Helper to list why a signal was rejected."""
    reasons = []
    if abs(velocity) <= velocity_threshold:
        reasons.append(f"Velocity too low ({abs(velocity):.2f} <= {velocity_threshold:.4f})")
    if not impulse_valid:
        reasons.append("Impulse quality failed (body/close ratio)")
    if not trend_valid:
        reasons.append("EMA slope not aligned with direction")
    if not volume_valid:
        reasons.append("Volume surge insufficient")
    if not volatility_expanding:
        reasons.append("Volatility not expanding")
    if not acceptance_valid:
        reasons.append("Breakout acceptance failed")
    return reasons


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
