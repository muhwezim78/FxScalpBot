"""
Volatility Filter

Classifies market volatility regime and filters trades accordingly.
Prevents trading during extreme volatility or when spreads are abnormal.

CRITICAL: This is a FILTER, not a signal generator.
If volatility is extreme, DO NOT TRADE regardless of momentum.
"""

from dataclasses import dataclass
from enum import Enum
from typing import List, Tuple
import numpy as np


class VolatilityRegime(Enum):
    """Market volatility classification."""
    LOW = "low"
    NORMAL = "normal"
    HIGH = "high"
    EXTREME = "extreme"


@dataclass
class VolatilityAnalysis:
    """Result of volatility analysis."""
    regime: VolatilityRegime
    atr_pips: float
    atr_percentile: float  # Where current ATR falls in historical distribution
    spread_ratio: float  # Current spread / average spread
    should_trade: bool
    rejection_reason: str | None


# Configuration
ATR_WINDOW = 14
ATR_EXTREME_THRESHOLD = 3.0  # 3x normal = extreme
ATR_HIGH_THRESHOLD = 2.0  # 2x normal = high
SPREAD_REJECT_THRESHOLD = 1.5  # Reject if spread > 1.5x average


def calculate_atr(highs: np.ndarray, lows: np.ndarray, closes: np.ndarray) -> float:
    """
    Calculate Average True Range.
    
    Returns ATR in the same units as price (convert to pips externally).
    """
    if len(highs) < 2:
        return 0.0
    
    # True Range = max(high-low, abs(high-prev_close), abs(low-prev_close))
    tr_values = []
    
    for i in range(1, len(highs)):
        hl = highs[i] - lows[i]
        hc = abs(highs[i] - closes[i-1])
        lc = abs(lows[i] - closes[i-1])
        tr_values.append(max(hl, hc, lc))
    
    if not tr_values:
        return 0.0
    
    # Simple moving average of True Range
    return np.mean(tr_values[-ATR_WINDOW:])


def classify_regime(
    current_atr: float,
    historical_atrs: List[float]
) -> Tuple[VolatilityRegime, float]:
    """
    Classify volatility regime based on ATR.
    
    Returns:
        Tuple of (regime, percentile)
    """
    if not historical_atrs or current_atr == 0:
        return VolatilityRegime.NORMAL, 50.0
    
    # Calculate percentile
    below_count = sum(1 for atr in historical_atrs if atr < current_atr)
    percentile = (below_count / len(historical_atrs)) * 100
    
    # Calculate ratio to median
    median_atr = np.median(historical_atrs)
    if median_atr == 0:
        return VolatilityRegime.NORMAL, percentile
    
    ratio = current_atr / median_atr
    
    if ratio >= ATR_EXTREME_THRESHOLD:
        return VolatilityRegime.EXTREME, percentile
    elif ratio >= ATR_HIGH_THRESHOLD:
        return VolatilityRegime.HIGH, percentile
    elif ratio < 0.5:
        return VolatilityRegime.LOW, percentile
    else:
        return VolatilityRegime.NORMAL, percentile


def get_regime(atr: float, spread: float) -> str:
    """
    Simplified regime getter for Rust bridge.
    
    Args:
        atr: Current ATR in pips
        spread: Current spread in pips
        
    Returns:
        Regime string: "low", "normal", "high", or "extreme"
    """
    # Simple heuristics without historical data
    if atr > 50:  # Very high ATR (pips)
        return "extreme"
    elif atr > 30:
        return "high"
    elif atr < 5:
        return "low"
    else:
        return "normal"


def analyze_volatility(
    current_atr: float,
    historical_atrs: List[float],
    current_spread: float,
    avg_spread: float,
) -> VolatilityAnalysis:
    """
    Comprehensive volatility analysis.
    
    Args:
        current_atr: Current ATR in pips
        historical_atrs: Historical ATR values
        current_spread: Current spread in pips
        avg_spread: Average spread in pips
        
    Returns:
        VolatilityAnalysis with trading recommendation
    """
    # Classify regime
    regime, percentile = classify_regime(current_atr, historical_atrs)
    
    # Calculate spread ratio
    spread_ratio = current_spread / avg_spread if avg_spread > 0 else 1.0
    
    # Determine if we should trade
    should_trade = True
    rejection_reason = None
    
    if regime == VolatilityRegime.EXTREME:
        should_trade = False
        rejection_reason = "volatility_extreme"
    elif spread_ratio > SPREAD_REJECT_THRESHOLD:
        should_trade = False
        rejection_reason = "spread_too_wide"
    elif regime == VolatilityRegime.LOW:
        # Low volatility = low opportunity, but not necessarily dangerous
        # Could still trade with adjusted expectations
        pass
    
    return VolatilityAnalysis(
        regime=regime,
        atr_pips=current_atr,
        atr_percentile=percentile,
        spread_ratio=spread_ratio,
        should_trade=should_trade,
        rejection_reason=rejection_reason,
    )


def is_spread_acceptable(
    current_spread: float,
    avg_spread: float,
    threshold: float = SPREAD_REJECT_THRESHOLD
) -> Tuple[bool, str | None]:
    """
    Check if spread is acceptable for trading.
    
    Returns:
        Tuple of (acceptable, rejection_reason)
    """
    if avg_spread <= 0:
        return False, "no_spread_history"
    
    ratio = current_spread / avg_spread
    
    if ratio > threshold:
        return False, f"spread_ratio_{ratio:.2f}"
    
    return True, None


def detect_spread_spike(
    spreads: List[float],
    window: int = 10,
    spike_threshold: float = 2.0
) -> bool:
    """
    Detect sudden spread spike.
    
    Returns True if recent spread is significantly higher than moving average.
    """
    if len(spreads) < window + 1:
        return False
    
    historical = spreads[-(window+1):-1]
    current = spreads[-1]
    
    avg = np.mean(historical)
    
    return current > avg * spike_threshold


# Tests
if __name__ == "__main__":
    # Test regime classification
    historical = [10.0, 12.0, 11.0, 9.0, 10.5, 11.2, 10.8, 9.5, 10.2, 11.0]
    
    # Normal volatility
    regime, pct = classify_regime(10.5, historical)
    print(f"Normal ATR (10.5): {regime.value}, percentile: {pct:.1f}")
    
    # High volatility
    regime, pct = classify_regime(22.0, historical)
    print(f"High ATR (22.0): {regime.value}, percentile: {pct:.1f}")
    
    # Extreme volatility
    regime, pct = classify_regime(35.0, historical)
    print(f"Extreme ATR (35.0): {regime.value}, percentile: {pct:.1f}")
    
    # Test spread spike detection
    spreads = [1.0, 1.1, 0.9, 1.0, 1.2, 1.0, 0.8, 1.1, 1.0, 0.9, 3.5]  # Spike at end
    print(f"\nSpread spike detected: {detect_spread_spike(spreads)}")
