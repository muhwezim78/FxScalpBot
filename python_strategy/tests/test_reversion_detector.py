import pytest
import sys
import os

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), '../src')))

from reversion_detector import detect_reversion, Z_THRESHOLD

def test_flat_market_no_reversion():
    base = 1.1000
    ticks = [{"bid": base, "ask": base + 0.0001} for _ in range(350)]
    
    result = detect_reversion(ticks)
    assert not result["detected"]
    assert result["direction"] == 0
    assert "Standard deviation is zero" in result["rejection_reasons"][0]

def test_oversold_bounce():
    base = 1.1000
    ticks = [{"bid": base, "ask": base + 0.0001} for _ in range(250)]
    
    # Create extreme drop
    for i in range(50):
        p = base - (i * 0.0001)
        ticks.append({"bid": p, "ask": p + 0.0001})
    
    # Needs a bounce to trigger detection reliably
    last_p = ticks[-1]["bid"]
    ticks.append({"bid": last_p + 0.0005, "ask": last_p + 0.0006})
    
    result = detect_reversion(ticks)
    assert result["detected"] is True
    assert result["direction"] == 1
    assert result["z_score"] < -Z_THRESHOLD
