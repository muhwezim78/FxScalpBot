# FxScalpBot

A conservative, survival-oriented momentum scalping system using Rust for execution and Python for strategy.

## Design Philosophy

**Survival > Speed. Capital Preservation > Profit Maximization.**

This system explicitly rejects:
- ❌ Martingale (doubling down on losses)
- ❌ Exponential lot growth
- ❌ "Recover losses" mentality
- ❌ Unrealistic return expectations

This system embraces:
- ✅ Fixed, small profit targets
- ✅ Linear, capped scaling using **locked-in profits only**
- ✅ Hard circuit breakers at multiple levels
- ✅ Time-based exits for stalled momentum

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Python Strategy Layer                    │
│  ┌──────────────┐ ┌─────────────────┐ ┌──────────────────┐  │
│  │   Momentum   │ │   Volatility    │ │      Trade       │  │
│  │   Detector   │ │     Filter      │ │    Qualifier     │  │
│  └──────────────┘ └─────────────────┘ └──────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Rust Execution Layer                      │
│  ┌──────────────┐ ┌─────────────────┐ ┌──────────────────┐  │
│  │     Tick     │ │      Risk       │ │      Order       │  │
│  │   Ingestion  │ │    Enforcer     │ │    Executor      │  │
│  └──────────────┘ └─────────────────┘ └──────────────────┘  │
│  ┌──────────────┐ ┌─────────────────┐ ┌──────────────────┐  │
│  │     Kill     │ │      State      │ │     Python       │  │
│  │    Switch    │ │     Machine     │ │      Bridge      │  │
│  └──────────────┘ └─────────────────┘ └──────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## Core Risk Rules

| Rule | Value |
|------|-------|
| Daily Loss Limit | 2% of account |
| Max Concurrent Positions | 1 |
| Max Scale-Ins | 3 |
| Scaling Source | Locked profits ONLY |
| Reversal Exit | 30% of run profits lost |
| Stall Timeout | 15 seconds |
| Max Spread | 1.5× average |
| Max Latency | 50ms |

## Quick Start

### Prerequisites

- Rust 1.70+
- Python 3.10+
- NumPy, Pandas

### Build Rust Core

```bash
cd rust_core
cargo build --release
```

### Install Python Strategy

```bash
cd python_strategy
pip install -e .
```

### Run (Paper Trading)

```bash
cd rust_core
cargo run --release
```

## Project Structure

```
FxScalpBot/
├── rust_core/           # High-performance execution layer
│   ├── src/
│   │   ├── main.rs
│   │   ├── risk_enforcer.rs
│   │   ├── kill_switch.rs
│   │   ├── state_machine.rs
│   │   ├── tick_ingestion.rs
│   │   ├── order_executor.rs
│   │   └── python_bridge.rs
│   └── Cargo.toml
│
├── python_strategy/     # Strategy and analysis layer
│   ├── src/
│   │   ├── __init__.py
│   │   ├── momentum_detector.py
│   │   ├── volatility_filter.py
│   │   ├── trade_qualifier.py
│   │   └── scale_calculator.py
│   └── pyproject.toml
│
└── config/              # Configuration files
    ├── risk_limits.toml
    └── strategy_params.toml
```

## Realistic Expectations

This system is designed for **realistic, sustainable growth**:

| Starting Capital | Monthly Return | Time to 2× |
|------------------|----------------|------------|
| $1,000 | 2-4% | 18-36 months |
| $10,000 | 2-4% | 18-36 months |
| $50,000 | 1-3% | 24-72 months |

### Why This System CANNOT Turn $5 Into $100,000

- Position sizing is proportional to account: $5 = 0.00005 lots
- Daily loss limit: 2% of $5 = $0.10
- Spread cost often exceeds potential profit at this scale
- **Mathematically impossible within any reasonable timeframe**

## License

MIT
