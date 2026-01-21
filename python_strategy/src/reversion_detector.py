"""
Mean Reversion Detector

Identifies price extremes and potential reversals to the mean.
Uses Z-score of price relative to a moving average.
"""

import numpy as np
from typing import List, Dict, Any
from dataclasses import dataclass

@dataclass
class ReversionSignal:
    detected: bool
    direction: int  # 1 = long (reversion from bottom), -1 = short (reversion from top)
    z_score: float
    distance_from_mean: f64
    strength: float

# Configuration
WINDOW_SIZE = 300  # Longer window for mean reversion
Z_THRESHOLD = 3.0   # Extreme deviation (Conservative)
BOUNCE_CONFIRMATION = 0.5 # Sigma bounce required to confirm turn

def detect_reversion(ticks: List[Dict[str, Any]]) -> Dict[str, Any]:
    if len(ticks) < WINDOW_SIZE:
        return {
            "detected": False,
            "direction": 0,
            "z_score": 0.0,
            "strength": 0.0
        }
    
    # Extract mid prices
    prices = np.array([(t.get("bid", 0.0) + t.get("ask", 0.0)) / 2 for t in ticks[-WINDOW_SIZE:]])
    current_price = prices[-1]
    
    # Calculate Mean and Std
    mean = np.mean(prices)
    std = np.std(prices)
    
    if std <= 0:
        return {"detected": False, "direction": 0, "z_score": 0.0, "strength": 0.0}
    
    z_score = (current_price - mean) / std
    
    direction = 0
    detected = False
    
    # Overbought Reversion (Short)
    if z_score > Z_THRESHOLD:
        # Check for roll-over: current price must be lower than peak
        peak = np.max(prices)
        if current_price < peak - (std * 0.2): # Small confirmation
            direction = -1
            detected = True
            
    # Oversold Reversion (Long)
    elif z_score < -Z_THRESHOLD:
        # Check for bounce: current price must be higher than trough
        trough = np.min(prices)
        if current_price > trough + (std * 0.2): # Small confirmation
            direction = 1
            detected = True
            
    # Strength is normalized distance past the threshold
    strength = min(abs(z_score) / (Z_THRESHOLD * 1.5), 1.0)
    
    return {
        "detected": bool(detected),
        "direction": int(direction),
        "z_score": float(z_score),
        "strength": float(strength),
        "mean": float(mean),
        "std": float(std)
    }

if __name__ == "__main__":
    # Test Oversold Bounce
    base = 1.1000
    ticks = [{"bid": base, "ask": base + 0.0001} for _ in range(250)]
    # Create extreme drop
    for i in range(50):
        p = base - (i * 0.0001)
        ticks.append({"bid": p, "ask": p + 0.0001})
    
    # Add a small bounce at the end
    last_p = ticks[-1]["bid"]
    ticks.append({"bid": last_p + 0.0005, "ask": last_p + 0.0006})
    
    result = detect_reversion(ticks)
    print(f"Oversold Test: Detected={result['detected']}, Direction={result['direction']}, Z={result['z_score']:.2f}")
