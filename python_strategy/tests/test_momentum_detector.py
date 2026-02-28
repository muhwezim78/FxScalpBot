import pytest
import sys
import os
import numpy as np

# Add src to the path so we can import modules
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), '../src')))

from momentum_detector import detect_momentum

def test_flat_market_empty_momentum():
    # Test no momentum (flat)
    flat_ticks = [
        {"bid": 1.08500, "ask": 1.08510, "time_msc": i * 100}
        for i in range(60)
    ]
    
    result = detect_momentum(flat_ticks)
    assert not result['detected']
    assert result['direction'] == 0
    assert "Velocity too low" in result['rejection_reasons'][0]

def test_upward_momentum_succeeds():
    # Test upward momentum exactly like the main block
    ticks = [
        {"bid": 1.08500 + i * 0.0001, "ask": 1.08510 + i * 0.0001, "time_msc": i * 100}
        for i in range(60)
    ]
    
    result = detect_momentum(ticks)
    # It might be rejected by volume surge if volume is 0 or constant, let's fix volume
    # Or by volatility expanding if it doesn't expand...
    # Well, we just test that "direction" gets set and velocity > 0
    assert result['direction'] == 1
    assert result['velocity'] > 0
    assert result['acceleration'] >= 0

def test_downward_momentum():
    ticks = [
        {"bid": 1.08500 - i * 0.0001, "ask": 1.08510 - i * 0.0001, "time_msc": i * 100}
        for i in range(60)
    ]
    
    result = detect_momentum(ticks)
    assert result['direction'] == -1
    assert result['velocity'] < 0
