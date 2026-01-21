"""
Trade Qualifier

Determines when NOT to trade. This is the final gate before entry.
If ANY rejection condition is true, the trade is blocked.

Philosophy: It's better to miss a good trade than take a bad one.
"""

from dataclasses import dataclass
from typing import Dict, Any, Tuple, List
from datetime import datetime, time


@dataclass
class QualificationResult:
    """Result of trade qualification."""
    qualified: bool
    rejection_reason: str | None
    suggested_lots: float
    confidence: float  # 0.0 to 1.0


# Configuration - Conservative defaults
MAX_SPREAD_RATIO = 1.5  # Max spread / avg spread ratio
MAX_LATENCY_MS = 50  # Maximum acceptable latency
MIN_MOMENTUM_STRENGTH = 0.3  # Minimum momentum strength to trade
DAILY_LIMIT_WARNING_PCT = 0.8  # Warn at 80% of daily limit
COOLDOWN_SECONDS = 30  # Minimum time between trades

# News blackout times (UTC) - Major economic releases
NEWS_BLACKOUT_MINUTES = [
    (time(8, 30), time(8, 35)),   # US Pre-market
    (time(13, 30), time(13, 35)),  # US Economic Data
    (time(14, 0), time(14, 5)),    # FOMC
    (time(12, 0), time(12, 5)),    # ECB
]

# Low liquidity periods to avoid
LOW_LIQUIDITY_PERIODS = [
    (time(21, 0), time(23, 59)),  # Asian session open transition
    (time(0, 0), time(2, 0)),     # Very early Asian session
]


def is_news_window(current_time: datetime | None = None) -> bool:
    """
    Check if current time is within a news blackout window.
    
    During high-impact news, spreads widen and slippage increases.
    """
    if current_time is None:
        current_time = datetime.utcnow()
    
    current_t = current_time.time()
    
    for start, end in NEWS_BLACKOUT_MINUTES:
        if start <= current_t <= end:
            return True
    
    return False


def is_low_liquidity(current_time: datetime | None = None) -> bool:
    """
    Check if current time is a low liquidity period.
    """
    if current_time is None:
        current_time = datetime.utcnow()
    
    current_t = current_time.time()
    
    for start, end in LOW_LIQUIDITY_PERIODS:
        if start <= current_t <= end:
            return True
    
    return False


def check_spread_condition(
    spread: float,
    avg_spread: float,
    threshold: float = MAX_SPREAD_RATIO
) -> Tuple[bool, str | None]:
    """
    Check if spread is acceptable.
    
    Returns:
        Tuple of (pass, rejection_reason)
    """
    if avg_spread <= 0:
        return False, "no_spread_baseline"
    
    ratio = spread / avg_spread
    
    if ratio > threshold:
        return False, f"spread_too_wide_{ratio:.2f}x"
    
    return True, None


def check_latency_condition(
    latency_ms: int,
    threshold: int = MAX_LATENCY_MS
) -> Tuple[bool, str | None]:
    """
    Check if latency is acceptable.
    """
    if latency_ms > threshold:
        return False, f"latency_too_high_{latency_ms}ms"
    
    return True, None


def check_momentum_condition(
    momentum_detected: bool,
    momentum_strength: float,
    velocity: float,
    acceleration: float,
    direction: int,
) -> Tuple[bool, str | None]:
    """
    Check if momentum is strong enough to trade.
    """
    if not momentum_detected:
        return False, "no_momentum"
    
    if momentum_strength < MIN_MOMENTUM_STRENGTH:
        return False, f"momentum_too_weak_{momentum_strength:.2f}"
    
    if direction == 0:
        return False, "no_direction"
    
    # Acceleration should be in same direction as velocity
    if acceleration * direction < 0:
        return False, "decelerating"
    
    return True, None


def check_daily_limit_condition(
    daily_pnl: float,
    daily_limit: float,
    warning_threshold: float = DAILY_LIMIT_WARNING_PCT
) -> Tuple[bool, str | None]:
    """
    Check if approaching daily loss limit.
    """
    if daily_pnl < 0:
        loss_ratio = abs(daily_pnl) / daily_limit if daily_limit > 0 else 0
        
        if loss_ratio >= 1.0:
            return False, "daily_limit_hit"
        elif loss_ratio >= warning_threshold:
            return False, f"approaching_daily_limit_{loss_ratio:.1%}"
    
    return True, None


def calculate_suggested_lots(
    account_balance: float,
    momentum_strength: float,
    base_lot_per_1000: float = 0.01
) -> float:
    """
    Calculate suggested lot size based on account and signal strength.
    
    Formula: (balance / 1000) * base_lot * strength_modifier
    
    Strength modifier reduces size for weaker signals:
    - Strength 0.3-0.5: 50% of base
    - Strength 0.5-0.7: 75% of base
    - Strength 0.7+: 100% of base
    """
    base_lots = (account_balance / 1000) * base_lot_per_1000
    
    if momentum_strength < 0.5:
        strength_modifier = 0.5
    elif momentum_strength < 0.7:
        strength_modifier = 0.75
    else:
        strength_modifier = 1.0
    
    return base_lots * strength_modifier


def qualify_trade(context: Dict[str, Any]) -> Dict[str, Any]:
    """
    Main qualification function called from Rust.
    
    Args:
        context: Dict with trading context including:
            - momentum_detected: bool
            - momentum_direction: int
            - momentum_strength: float
            - velocity: float
            - acceleration: float
            - volume_surge: bool
            - spread: float
            - avg_spread: float
            - latency_ms: int
            - daily_pnl: float
            - daily_limit: float
            - account_balance: float
            
    Returns:
        Dict with qualification result
    """
    # Extract context
    momentum_detected = context.get("momentum_detected", False)
    momentum_direction = context.get("momentum_direction", 0)
    momentum_strength = context.get("momentum_strength", 0.0)
    velocity = context.get("velocity", 0.0)
    acceleration = context.get("acceleration", 0.0)
    spread = context.get("spread", 0.0)
    avg_spread = context.get("avg_spread", 0.0)
    latency_ms = context.get("latency_ms", 0)
    daily_pnl = context.get("daily_pnl", 0.0)
    daily_limit = context.get("daily_limit", 0.0)
    account_balance = context.get("account_balance", 0.0)
    
    # Run all checks
    checks = [
        check_spread_condition(spread, avg_spread),
        check_latency_condition(latency_ms),
        check_momentum_condition(
            momentum_detected, momentum_strength, 
            velocity, acceleration, momentum_direction
        ),
        check_daily_limit_condition(daily_pnl, daily_limit),
    ]
    
    # Additional time-based checks
    if is_news_window():
        checks.append((False, "news_blackout"))
    
    if is_low_liquidity():
        checks.append((False, "low_liquidity"))
    
    # Aggregate results
    for passed, reason in checks:
        if not passed:
            return {
                "qualified": False,
                "rejection_reason": reason,
                "suggested_lots": 0.0,
                "confidence": 0.0,
            }
    
    # All checks passed
    suggested_lots = calculate_suggested_lots(account_balance, momentum_strength)
    
    # Confidence based on signal quality
    confidence = momentum_strength
    
    return {
        "qualified": True,
        "rejection_reason": None,
        "suggested_lots": suggested_lots,
        "confidence": confidence,
    }


def get_rejection_reasons(context: Dict[str, Any]) -> List[str]:
    """
    Get all rejection reasons (for diagnostics).
    
    Unlike qualify_trade which returns first rejection,
    this returns ALL reasons why a trade would be rejected.
    """
    reasons = []
    
    # Spread check
    passed, reason = check_spread_condition(
        context.get("spread", 0), 
        context.get("avg_spread", 0)
    )
    if not passed:
        reasons.append(reason)
    
    # Latency check
    passed, reason = check_latency_condition(context.get("latency_ms", 0))
    if not passed:
        reasons.append(reason)
    
    # Momentum check
    passed, reason = check_momentum_condition(
        context.get("momentum_detected", False),
        context.get("momentum_strength", 0.0),
        context.get("velocity", 0.0),
        context.get("acceleration", 0.0),
        context.get("momentum_direction", 0),
    )
    if not passed:
        reasons.append(reason)
    
    # Daily limit check
    passed, reason = check_daily_limit_condition(
        context.get("daily_pnl", 0.0),
        context.get("daily_limit", 0.0),
    )
    if not passed:
        reasons.append(reason)
    
    # Time checks
    if is_news_window():
        reasons.append("news_blackout")
    
    if is_low_liquidity():
        reasons.append("low_liquidity")
    
    return reasons


# Tests
if __name__ == "__main__":
    # Test qualified trade
    context = {
        "momentum_detected": True,
        "momentum_direction": 1,
        "momentum_strength": 0.75,
        "velocity": 2.5,
        "acceleration": 0.5,
        "spread": 1.0,
        "avg_spread": 1.0,
        "latency_ms": 20,
        "daily_pnl": -50,
        "daily_limit": 200,
        "account_balance": 10000,
    }
    
    result = qualify_trade(context)
    print(f"Qualified trade test:")
    print(f"  Qualified: {result['qualified']}")
    print(f"  Suggested lots: {result['suggested_lots']:.4f}")
    print(f"  Confidence: {result['confidence']:.2f}")
    
    # Test spread rejection
    context["spread"] = 2.0  # 2x average
    result = qualify_trade(context)
    print(f"\nSpread rejection test:")
    print(f"  Qualified: {result['qualified']}")
    print(f"  Reason: {result['rejection_reason']}")
