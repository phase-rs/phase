#![cfg_attr(not(mobile), allow(dead_code))]

use tauri::ipc::Channel;

use crate::native_engine_contract::{
    BridgeEvent, NativeEngineBridgeError, NativeEngineError, NativeEngineKey, NativeEngineProgress,
    NativeEngineReady, NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL,
};

#[cfg_attr(mobile, tauri::command)]
pub async fn ensure_native_engine(
    _key: NativeEngineKey,
) -> Result<NativeEngineReady, NativeEngineError> {
    Err(NativeEngineError::UnsupportedPlatform {
        detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
    })
}
#[cfg_attr(mobile, tauri::command)]
pub fn native_engine_progress() -> Option<NativeEngineProgress> {
    None
}

#[cfg_attr(mobile, tauri::command)]
pub async fn stop_native_engine() -> Result<(), NativeEngineError> {
    Ok(())
}

#[cfg_attr(mobile, tauri::command)]
pub async fn connect_native_engine(
    _on_event: Channel<BridgeEvent>,
) -> Result<u64, NativeEngineBridgeError> {
    Err(NativeEngineBridgeError::UnsupportedPlatform {
        detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
    })
}

#[cfg_attr(mobile, tauri::command)]
pub fn native_engine_bridge_send(_id: u64, _text: String) -> Result<(), NativeEngineBridgeError> {
    Err(NativeEngineBridgeError::UnsupportedPlatform {
        detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
    })
}

#[cfg_attr(mobile, tauri::command)]
pub fn native_engine_bridge_close(_id: u64) -> Result<(), NativeEngineBridgeError> {
    Err(NativeEngineBridgeError::UnsupportedPlatform {
        detail: NATIVE_ENGINE_UNSUPPORTED_PLATFORM_DETAIL.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXACT_ENGINE_ERROR: &str =
        r#"{"kind":"unsupported_platform","detail":"native engine is not available on mobile"}"#;
    const EXACT_BRIDGE_ERROR: &str =
        r#"{"kind":"unsupported_platform","detail":"native engine is not available on mobile"}"#;

    fn exact_json<T: serde::Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    #[test]
    fn ensure_native_engine_returns_exact_unsupported_platform_json_for_release_and_preview() {
        let release =
            tauri::async_runtime::block_on(ensure_native_engine(NativeEngineKey::Release {
                version: "0.60.0".to_owned(),
            }))
            .unwrap_err();
        let preview =
            tauri::async_runtime::block_on(ensure_native_engine(NativeEngineKey::Preview {
                fingerprint: "\"\\\n\u{0000}hostile".to_owned(),
            }))
            .unwrap_err();
        assert_eq!(exact_json(&release), EXACT_ENGINE_ERROR);
        assert_eq!(exact_json(&preview), EXACT_ENGINE_ERROR);
    }

    #[test]
    fn native_engine_progress_returns_two_exact_null_serializations() {
        assert_eq!(exact_json(&native_engine_progress()), "null");
        assert_eq!(exact_json(&native_engine_progress()), "null");
    }

    #[test]
    fn stop_native_engine_is_idempotent_with_two_exact_null_serializations() {
        let first = tauri::async_runtime::block_on(stop_native_engine()).unwrap();
        let second = tauri::async_runtime::block_on(stop_native_engine()).unwrap();
        assert_eq!(exact_json(&first), "null");
        assert_eq!(exact_json(&second), "null");
    }

    #[test]
    fn connect_native_engine_returns_exact_unsupported_json_without_callbacks() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let callback_count = Arc::new(AtomicUsize::new(0));
        let accepting_count = Arc::clone(&callback_count);
        let accepting = Channel::new(move |_| {
            accepting_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let hostile_count = Arc::clone(&callback_count);
        let hostile = Channel::new(move |_| {
            hostile_count.fetch_add(1, Ordering::SeqCst);
            panic!("mobile compatibility handler must not emit bridge events")
        });

        let accepting_error =
            tauri::async_runtime::block_on(connect_native_engine(accepting)).unwrap_err();
        let hostile_error =
            tauri::async_runtime::block_on(connect_native_engine(hostile)).unwrap_err();
        assert_eq!(exact_json(&accepting_error), EXACT_BRIDGE_ERROR);
        assert_eq!(exact_json(&hostile_error), EXACT_BRIDGE_ERROR);
        assert_eq!(callback_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn native_engine_bridge_send_returns_exact_unsupported_platform_json_for_hostile_inputs() {
        let empty = native_engine_bridge_send(0, String::new()).unwrap_err();
        let hostile =
            native_engine_bridge_send(u64::MAX, "\"\\\n\u{0000}hostile".to_owned()).unwrap_err();
        assert_eq!(exact_json(&empty), EXACT_BRIDGE_ERROR);
        assert_eq!(exact_json(&hostile), EXACT_BRIDGE_ERROR);
    }

    #[test]
    fn native_engine_bridge_close_returns_exact_unsupported_platform_json_for_edge_ids() {
        let zero = native_engine_bridge_close(0).unwrap_err();
        let max = native_engine_bridge_close(u64::MAX).unwrap_err();
        assert_eq!(exact_json(&zero), EXACT_BRIDGE_ERROR);
        assert_eq!(exact_json(&max), EXACT_BRIDGE_ERROR);
    }
}
