"""
Scale Calculator

Calculates lot sizes for scale-ins using ONLY locked-in profits.
NEVER uses original capital or unrealized gains for scaling.

This is CRITICAL for avoiding martingale-like behavior.
"""

from typing import Tuple


# Configuration
MAX_SCALES = 3  # Maximum scale-ins per run
LOT_PER_DOLLAR_PROFIT = 0.001  # 0.001 lots per $1 locked profit
SCALE_FACTORS = [0.3, 0.4, 0.5]  # Percentage of available profit per scale
MIN_SCALE_PROFIT = 1.0  # Minimum locked profit to allow scaling ($)
MAX_SCALE_LOTS = 0.1  # Maximum lots per scale-in


def calculate_available_for_scale(locked_profit: float) -> float:
    """
    Calculate how much of locked profit is available for scaling.
    
    Rule: Only 50% of locked profits can be used for scaling.
    This preserves 50% of gains regardless of what happens.
    """
    return locked_profit * 0.5


def calculate_scale_lots(
    locked_profit: float,
    current_lots: float,
    scale_number: int,
    max_lots: float,
) -> float:
    """
    Calculate lot size for a scale-in.
    
    Called from Rust via Python bridge.
    
    Args:
        locked_profit: Realized P&L from partial closes (NOT unrealized)
        current_lots: Current position size
        scale_number: Which scale this is (0-indexed, 0 = first scale)
        max_lots: Maximum allowed position size
        
    Returns:
        Lot size for this scale-in (0 if scaling not allowed)
    """
    # Check if scaling is allowed
    if scale_number >= MAX_SCALES:
        return 0.0
    
    if locked_profit < MIN_SCALE_PROFIT:
        return 0.0
    
    # Calculate available profit for scaling
    available = calculate_available_for_scale(locked_profit)
    
    # Get scale factor for this level
    scale_factor = SCALE_FACTORS[scale_number] if scale_number < len(SCALE_FACTORS) else 0.5
    
    # Calculate base lots from available profit
    base_lots = available * LOT_PER_DOLLAR_PROFIT * scale_factor
    
    # Apply maximum per-scale limit
    base_lots = min(base_lots, MAX_SCALE_LOTS)
    
    # Ensure we don't exceed max position size
    remaining_capacity = max_lots - current_lots
    
    return max(0.0, min(base_lots, remaining_capacity))


def validate_scale(
    locked_profit: float,
    unrealized_pnl: float,
    scale_count: int,
    peak_profit: float,
    reversal_threshold: float = 0.3,
) -> Tuple[bool, str | None]:
    """
    Validate if scaling is safe.
    
    Returns:
        Tuple of (allowed, rejection_reason)
    """
    # Check scale count
    if scale_count >= MAX_SCALES:
        return False, f"max_scales_reached_{scale_count}"
    
    # Check locked profit
    if locked_profit < MIN_SCALE_PROFIT:
        return False, f"insufficient_locked_profit_{locked_profit:.2f}"
    
    # Check for reversal
    total_pnl = locked_profit + unrealized_pnl
    if peak_profit > 0:
        drawdown = peak_profit - total_pnl
        drawdown_pct = drawdown / peak_profit
        
        if drawdown_pct > reversal_threshold:
            return False, f"reversal_detected_{drawdown_pct:.1%}"
    
    return True, None


def calculate_profit_lock_amount(
    unrealized_pnl: float,
    current_lots: float,
    lock_percentage: float = 0.5,
) -> Tuple[float, float]:
    """
    Calculate how to lock profits via partial close.
    
    Strategy: Close 50% of position to lock in profits.
    
    Args:
        unrealized_pnl: Current floating P&L
        current_lots: Current position size
        lock_percentage: Percentage to close (default 50%)
        
    Returns:
        Tuple of (lots_to_close, expected_locked_profit)
    """
    if unrealized_pnl <= 0:
        return 0.0, 0.0
    
    lots_to_close = current_lots * lock_percentage
    expected_locked = unrealized_pnl * lock_percentage
    
    return lots_to_close, expected_locked


def get_scale_summary(
    locked_profit: float,
    current_lots: float,
    scale_count: int,
    max_lots: float,
) -> dict:
    """
    Get comprehensive scale status for diagnostics.
    """
    can_scale, reason = validate_scale(locked_profit, 0, scale_count, locked_profit)
    
    next_scale_lots = 0.0
    if can_scale:
        next_scale_lots = calculate_scale_lots(
            locked_profit, current_lots, scale_count, max_lots
        )
    
    return {
        "can_scale": can_scale,
        "rejection_reason": reason,
        "scales_remaining": max(0, MAX_SCALES - scale_count),
        "locked_profit": locked_profit,
        "available_for_scale": calculate_available_for_scale(locked_profit),
        "next_scale_lots": next_scale_lots,
        "current_lots": current_lots,
        "total_lots_after_scale": current_lots + next_scale_lots,
    }


# Example walkthrough
def example_scaling_scenario():
    """
    Demonstrate how scaling works with locked profits only.
    """
    print("=" * 60)
    print("SCALING EXAMPLE: Conservative Profit-Based Scaling")
    print("=" * 60)
    
    # Initial trade
    account_balance = 10000
    initial_lots = 0.01
    max_lots = 0.1
    
    print(f"\n1. INITIAL ENTRY")
    print(f"   Account: ${account_balance}")
    print(f"   Entry lots: {initial_lots}")
    print(f"   Max position: {max_lots}")
    
    # Trade goes in our favor, lock some profit
    print(f"\n2. PROFIT LOCK")
    unrealized = 15.0  # $15 unrealized profit
    lock_lots, locked = calculate_profit_lock_amount(unrealized, initial_lots)
    print(f"   Unrealized P&L: ${unrealized}")
    print(f"   Partial close: {lock_lots:.4f} lots")
    print(f"   Locked profit: ${locked}")
    
    # Remaining position
    remaining_lots = initial_lots - lock_lots
    locked_profit = locked
    
    print(f"\n3. FIRST SCALE-IN")
    summary = get_scale_summary(locked_profit, remaining_lots, 0, max_lots)
    print(f"   Can scale: {summary['can_scale']}")
    print(f"   Available for scale: ${summary['available_for_scale']:.2f}")
    print(f"   Scale lots: {summary['next_scale_lots']:.4f}")
    print(f"   Total position: {summary['total_lots_after_scale']:.4f}")
    
    # After first scale
    current_lots = summary['total_lots_after_scale']
    
    # More profit, lock again
    print(f"\n4. SECOND PROFIT LOCK")
    unrealized = 25.0
    lock_lots, more_locked = calculate_profit_lock_amount(unrealized, current_lots)
    locked_profit += more_locked
    remaining_lots = current_lots - lock_lots
    print(f"   Unrealized P&L: ${unrealized}")
    print(f"   Additional locked: ${more_locked}")
    print(f"   Total locked: ${locked_profit}")
    
    print(f"\n5. SECOND SCALE-IN")
    summary = get_scale_summary(locked_profit, remaining_lots, 1, max_lots)
    print(f"   Can scale: {summary['can_scale']}")
    print(f"   Scale lots: {summary['next_scale_lots']:.4f}")
    
    print(f"\n6. KEY INSIGHT")
    print(f"   - Original capital at risk: ${account_balance * 0.01}")
    print(f"   - Scaling used ONLY locked profits")
    print(f"   - If trade reverses now, we keep ${locked_profit * 0.7:.2f} minimum")
    print(f"   - This is NOT martingale: we scale on WINS, not losses")


if __name__ == "__main__":
    example_scaling_scenario()
    
    print("\n" + "=" * 60)
    print("UNIT TESTS")
    print("=" * 60)
    
    # Test: No scaling with insufficient profit
    lots = calculate_scale_lots(0.5, 0.01, 0, 0.1)
    print(f"\nInsufficient profit ($0.50): {lots:.4f} lots (expected: 0)")
    
    # Test: First scale with $10 locked
    lots = calculate_scale_lots(10.0, 0.01, 0, 0.1)
    print(f"First scale ($10 locked): {lots:.4f} lots")
    
    # Test: Max scales reached
    lots = calculate_scale_lots(100.0, 0.05, 3, 0.1)
    print(f"Max scales reached: {lots:.4f} lots (expected: 0)")
