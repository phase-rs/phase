use serde::{Deserialize, Serialize};

pub const NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL: &str =
    "native engine is not available on mobile";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeEngineKey {
    Release { version: String },
    Preview { fingerprint: String },
}
#[derive(Clone, Debug, Serialize)]
pub struct NativeEngineReady {
    pub port: u16,
}

#[derive(Clone, Debug, Serialize)]
pub struct NativeEngineProgress {
    pub phase: NativeEngineProgressPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
// Mobile keeps the complete serialized desktop ABI so frontend consumers see
// one stable contract, while its compatibility layer can only emit a subset.
#[cfg_attr(mobile, allow(dead_code))]
pub enum NativeEngineProgressPhase {
    Resolving,
    DownloadingBinary,
    Verifying,
    DownloadingData,
    Spawning,
    Ready,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// See `NativeEngineProgressPhase`: these variants are a shared wire contract.
#[cfg_attr(mobile, allow(dead_code))]
pub enum NativeEngineError {
    InvalidKey {
        detail: String,
    },
    #[allow(dead_code)]
    UnsupportedPlatform {
        detail: String,
    },
    Download {
        detail: String,
    },
    Verification {
        detail: String,
    },
    Manifest {
        detail: String,
    },
    Downgrade {
        detail: String,
    },
    Storage {
        detail: String,
    },
    Spawn {
        detail: String,
    },
    Health {
        detail: String,
    },
    Internal {
        detail: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
// See `NativeEngineProgressPhase`: these variants are a shared wire contract.
#[cfg_attr(mobile, allow(dead_code))]
pub enum BridgeEvent {
    Message { text: String },
    Closed { code: u16, reason: String },
    Error { detail: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
// See `NativeEngineProgressPhase`: these variants are a shared wire contract.
#[cfg_attr(mobile, allow(dead_code))]
pub enum NativeEngineBridgeError {
    NotRunning {
        detail: String,
    },
    #[allow(dead_code)]
    UnsupportedPlatform {
        detail: String,
    },
    Connect {
        detail: String,
    },
    UnknownBridge {
        detail: String,
    },
    Send {
        detail: String,
    },
    Internal {
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_engine_key_preserves_every_exact_json_shape() {
        assert_eq!(
            serde_json::to_string(&NativeEngineKey::Release {
                version: "1.2.3".to_owned(),
            })
            .unwrap(),
            r#"{"release":{"version":"1.2.3"}}"#
        );
        assert_eq!(
            serde_json::to_string(&NativeEngineKey::Preview {
                fingerprint: "0123456789abcdef".to_owned(),
            })
            .unwrap(),
            r#"{"preview":{"fingerprint":"0123456789abcdef"}}"#
        );
    }

    #[test]
    fn native_engine_ready_and_progress_preserve_every_exact_json_shape() {
        assert_eq!(
            serde_json::to_string(&NativeEngineReady { port: 31337 }).unwrap(),
            r#"{"port":31337}"#
        );
        let cases = [
            (NativeEngineProgressPhase::Resolving, "resolving"),
            (
                NativeEngineProgressPhase::DownloadingBinary,
                "downloading_binary",
            ),
            (NativeEngineProgressPhase::Verifying, "verifying"),
            (
                NativeEngineProgressPhase::DownloadingData,
                "downloading_data",
            ),
            (NativeEngineProgressPhase::Spawning, "spawning"),
            (NativeEngineProgressPhase::Ready, "ready"),
            (NativeEngineProgressPhase::Failed, "failed"),
        ];
        for (phase, expected) in cases {
            assert_eq!(
                serde_json::to_string(&NativeEngineProgress {
                    phase: phase.clone(),
                    detail: None,
                })
                .unwrap(),
                format!(r#"{{"phase":"{expected}"}}"#)
            );
            assert_eq!(
                serde_json::to_string(&NativeEngineProgress {
                    phase,
                    detail: Some("12/34".to_owned()),
                })
                .unwrap(),
                format!(r#"{{"phase":"{expected}","detail":"12/34"}}"#)
            );
        }
    }

    #[test]
    fn native_engine_error_preserves_every_exact_json_shape() {
        let errors = [
            NativeEngineError::InvalidKey { detail: "d".into() },
            NativeEngineError::UnsupportedPlatform { detail: "d".into() },
            NativeEngineError::Download { detail: "d".into() },
            NativeEngineError::Verification { detail: "d".into() },
            NativeEngineError::Manifest { detail: "d".into() },
            NativeEngineError::Downgrade { detail: "d".into() },
            NativeEngineError::Storage { detail: "d".into() },
            NativeEngineError::Spawn { detail: "d".into() },
            NativeEngineError::Health { detail: "d".into() },
            NativeEngineError::Internal { detail: "d".into() },
        ];
        let kinds = [
            "invalid_key",
            "unsupported_platform",
            "download",
            "verification",
            "manifest",
            "downgrade",
            "storage",
            "spawn",
            "health",
            "internal",
        ];
        for (error, kind) in errors.into_iter().zip(kinds) {
            assert_eq!(
                serde_json::to_string(&error).unwrap(),
                format!(r#"{{"kind":"{kind}","detail":"d"}}"#)
            );
        }
    }

    #[test]
    fn bridge_event_preserves_every_exact_json_shape() {
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Message {
                text: "hello".into(),
            })
            .unwrap(),
            r#"{"type":"message","text":"hello"}"#
        );
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Closed {
                code: 1000,
                reason: "normal".into(),
            })
            .unwrap(),
            r#"{"type":"closed","code":1000,"reason":"normal"}"#
        );
        assert_eq!(
            serde_json::to_string(&BridgeEvent::Error {
                detail: "boom".into(),
            })
            .unwrap(),
            r#"{"type":"error","detail":"boom"}"#
        );
    }

    #[test]
    fn native_engine_bridge_error_preserves_every_exact_json_shape() {
        let errors = [
            NativeEngineBridgeError::NotRunning { detail: "d".into() },
            NativeEngineBridgeError::UnsupportedPlatform { detail: "d".into() },
            NativeEngineBridgeError::Connect { detail: "d".into() },
            NativeEngineBridgeError::UnknownBridge { detail: "d".into() },
            NativeEngineBridgeError::Send { detail: "d".into() },
            NativeEngineBridgeError::Internal { detail: "d".into() },
        ];
        let kinds = [
            "not_running",
            "unsupported_platform",
            "connect",
            "unknown_bridge",
            "send",
            "internal",
        ];
        for (error, kind) in errors.into_iter().zip(kinds) {
            assert_eq!(
                serde_json::to_string(&error).unwrap(),
                format!(r#"{{"kind":"{kind}","detail":"d"}}"#)
            );
        }
    }

    #[test]
    fn unsupported_platform_contract_is_byte_exact_for_engine_and_bridge_errors() {
        assert_eq!(
            serde_json::to_string(&NativeEngineError::UnsupportedPlatform {
                detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
            })
            .unwrap(),
            r#"{"kind":"unsupported_platform","detail":"native engine is not available on mobile"}"#
        );
        assert_eq!(
            serde_json::to_string(&NativeEngineBridgeError::UnsupportedPlatform {
                detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
            })
            .unwrap(),
            r#"{"kind":"unsupported_platform","detail":"native engine is not available on mobile"}"#
        );
    }
}
