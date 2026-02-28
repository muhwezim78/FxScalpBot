use std::sync::{Arc, atomic::AtomicBool};
use fx_scalp_core::bridge_client::BridgeClient;
use std::time::Duration;

// We will mock the ZMQ socket for a bridge test
// Note: ZMQ requires actual context, so instead of spinning up a full server
// in the test which is flaky, we just test the parsing of JSON responses 
// typical of what the BridgeClient expects inside its runloop, or what the 
// AppState orchestrator parses from the JSON representation. 

use fx_scalp_core::python_bridge::{MomentumSignal, QualificationResult};

#[test]
fn test_momentum_signal_parsing() {
    let json_resp = r#"{
        "detected": true,
        "direction": 1,
        "strength": 0.85,
        "velocity": 2.5,
        "acceleration": 0.5,
        "ema_slope": 1.2,
        "volume_surge": true
    }"#;
    
    let signal: MomentumSignal = serde_json::from_str(json_resp).unwrap();
    
    assert!(signal.detected);
    assert_eq!(signal.direction, 1);
    assert_eq!(signal.strength, 0.85);
    assert!(signal.volume_surge);
}

#[test]
fn test_qualification_result_parsing() {
    let json_resp = r#"{
        "qualified": false,
        "rejection_reason": "spread_too_wide",
        "suggested_lots": 0.0,
        "confidence": 0.2
    }"#;
    
    let qual: QualificationResult = serde_json::from_str(json_resp).unwrap();
    
    assert!(!qual.qualified);
    assert_eq!(qual.rejection_reason, Some("spread_too_wide".to_string()));
    assert_eq!(qual.suggested_lots, 0.0);
}
