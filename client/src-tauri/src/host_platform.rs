use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum HostPlatform {
    Desktop,
    Android,
    Ios,
}
#[tauri::command]
pub fn host_platform() -> HostPlatform {
    #[cfg(target_os = "android")]
    return HostPlatform::Android;
    #[cfg(target_os = "ios")]
    return HostPlatform::Ios;
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    HostPlatform::Desktop
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_platform_is_typed_and_serializes_as_the_public_abi() {
        assert_eq!(host_platform(), HostPlatform::Desktop);
        assert_eq!(
            serde_json::to_string(&HostPlatform::Desktop).unwrap(),
            "\"desktop\""
        );
        assert_eq!(
            serde_json::to_string(&HostPlatform::Android).unwrap(),
            "\"android\""
        );
        assert_eq!(
            serde_json::to_string(&HostPlatform::Ios).unwrap(),
            "\"ios\""
        );
    }
}
