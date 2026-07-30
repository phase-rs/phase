use std::fmt;
use std::io::Write;
use std::path::{Component, Path};
use std::time::Duration;

use minisign_verify::{PublicKey, Signature};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::warn;
use url::Url;

pub const PINNED_DATA_MANIFEST_PUBLIC_KEY: &str =
    "RWRDZxG2otNoKLblrgD00kM0a8U0CRZUGHpNCr3W+3ik1E84XHcB6hZe";
const RELEASE_MANIFEST_BASE_URL: &str = "https://data.phase-rs.dev/desktop";
const PREVIEW_MANIFEST_URL: &str = "https://data.phase-rs.dev/desktop/preview-server.json";
const REQUIRED_DATA_FILES: [&str; 1] = ["card-data.json"];
const BEST_EFFORT_DATA_FILES: [&str; 1] = ["draft-pools.json"];

#[derive(Debug)]
pub struct BootstrapError(String);

impl BootstrapError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BootstrapError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelIdentity {
    Release,
    Preview { fingerprint: String },
}

impl ChannelIdentity {
    pub fn embedded() -> Result<Option<Self>, BootstrapError> {
        identity_from_markers(
            option_env!("PHASE_CHANNEL"),
            option_env!("PHASE_ENGINE_FINGERPRINT"),
        )
    }
}

fn identity_from_markers(
    channel: Option<&str>,
    fingerprint: Option<&str>,
) -> Result<Option<ChannelIdentity>, BootstrapError> {
    match channel {
        None => Ok(None),
        Some("release") => {
            if fingerprint.is_some() {
                return Err(BootstrapError::new(
                    "PHASE_ENGINE_FINGERPRINT is only valid for PHASE_CHANNEL=preview",
                ));
            }
            Ok(Some(ChannelIdentity::Release))
        }
        Some("preview") => {
            let fingerprint = fingerprint.ok_or_else(|| {
                BootstrapError::new(
                    "PHASE_CHANNEL=preview requires a 16-hex PHASE_ENGINE_FINGERPRINT",
                )
            })?;
            if !is_fingerprint(fingerprint) {
                return Err(BootstrapError::new(format!(
                    "PHASE_ENGINE_FINGERPRINT must be 16 hexadecimal characters, got {fingerprint:?}"
                )));
            }
            Ok(Some(ChannelIdentity::Preview {
                fingerprint: fingerprint.to_string(),
            }))
        }
        Some(channel) => Err(BootstrapError::new(format!(
            "PHASE_CHANNEL must be release or preview, got {channel:?}"
        ))),
    }
}

fn is_fingerprint(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    pub manifest_url_override: Option<Url>,
    pub no_data_download: bool,
}

#[derive(Debug, Clone)]
enum ManifestResolution {
    Override(Url),
    Release(Url),
    Preview(Url),
}

impl ManifestResolution {
    fn url(&self) -> &Url {
        match self {
            Self::Override(url) | Self::Release(url) | Self::Preview(url) => url,
        }
    }
}

fn resolve_manifest(
    manifest_url_override: Option<Url>,
    identity: Option<&ChannelIdentity>,
) -> Result<ManifestResolution, BootstrapError> {
    if let Some(url) = manifest_url_override {
        if url.scheme() != "https" {
            return Err(BootstrapError::new(format!(
                "data manifest URL must use HTTPS, got {url}"
            )));
        }
        return Ok(ManifestResolution::Override(url));
    }

    match identity {
        Some(ChannelIdentity::Release) => {
            let version = env!("CARGO_PKG_VERSION");
            let url = Url::parse(&format!(
                "{RELEASE_MANIFEST_BASE_URL}/release-server-v{version}.json"
            ))
            .expect("release manifest URL is a valid constant");
            Ok(ManifestResolution::Release(url))
        }
        Some(ChannelIdentity::Preview { .. }) => Ok(ManifestResolution::Preview(
            Url::parse(PREVIEW_MANIFEST_URL).expect("preview manifest URL is a valid constant"),
        )),
        None => Err(BootstrapError::new(
            "card-data.json is missing and this binary has no PHASE_CHANNEL identity; pre-provision PHASE_DATA_DIR or pass --data-manifest-url <url>",
        )),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DataFile {
    name: String,
    sha256: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct ManifestEnvelope {
    schema: u32,
    channel: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseManifest {
    schema: u32,
    channel: String,
    version: String,
    data: Vec<DataFile>,
}

#[derive(Debug, Deserialize)]
struct PreviewManifest {
    schema: u32,
    channel: String,
    fingerprints: std::collections::BTreeMap<String, PreviewFingerprint>,
}

#[derive(Debug, Deserialize)]
struct PreviewFingerprint {
    data: Vec<DataFile>,
}

fn parse_manifest_data(
    bytes: &[u8],
    identity: Option<&ChannelIdentity>,
) -> Result<Vec<DataFile>, BootstrapError> {
    let envelope: ManifestEnvelope = serde_json::from_slice(bytes)
        .map_err(|error| BootstrapError::new(format!("invalid data manifest JSON: {error}")))?;
    if envelope.schema != 1 {
        return Err(BootstrapError::new(format!(
            "unsupported data manifest schema {}; expected schema 1",
            envelope.schema
        )));
    }

    let data = match envelope.channel.as_str() {
        "release" => {
            let manifest: ReleaseManifest = serde_json::from_slice(bytes).map_err(|error| {
                BootstrapError::new(format!("invalid release data manifest JSON: {error}"))
            })?;
            if manifest.schema != 1 || manifest.channel != "release" {
                return Err(BootstrapError::new(
                    "release data manifest does not declare schema 1 and channel release",
                ));
            }
            if matches!(identity, Some(ChannelIdentity::Release))
                && manifest.version != env!("CARGO_PKG_VERSION")
            {
                return Err(BootstrapError::new(format!(
                    "release data manifest version {} does not match this server version {}",
                    manifest.version,
                    env!("CARGO_PKG_VERSION")
                )));
            }
            manifest.data
        }
        "preview" => {
            let manifest: PreviewManifest = serde_json::from_slice(bytes).map_err(|error| {
                BootstrapError::new(format!("invalid preview data manifest JSON: {error}"))
            })?;
            if manifest.schema != 1 || manifest.channel != "preview" {
                return Err(BootstrapError::new(
                    "preview data manifest does not declare schema 1 and channel preview",
                ));
            }
            let Some(ChannelIdentity::Preview { fingerprint }) = identity else {
                return Err(BootstrapError::new(
                    "preview data manifest requires a binary built with PHASE_CHANNEL=preview and PHASE_ENGINE_FINGERPRINT",
                ));
            };
            manifest
                .fingerprints
                .get(fingerprint)
                .ok_or_else(|| {
                    BootstrapError::new(format!(
                        "preview data manifest has no entry for this binary fingerprint {fingerprint}"
                    ))
                })?
                .data
                .clone()
        }
        channel => {
            return Err(BootstrapError::new(format!(
                "data manifest has unsupported channel {channel:?}"
            )));
        }
    };

    validate_manifest_data(&data)?;
    Ok(data)
}

fn validate_manifest_data(data: &[DataFile]) -> Result<(), BootstrapError> {
    for file in data {
        if !is_safe_data_file_name(&file.name) {
            return Err(BootstrapError::new(format!(
                "data manifest has unsafe file name {:?}",
                file.name
            )));
        }
        if !is_sha256(&file.sha256) {
            return Err(BootstrapError::new(format!(
                "data manifest has invalid sha256 for {}: {}",
                file.name, file.sha256
            )));
        }
        let url = Url::parse(&file.url).map_err(|error| {
            BootstrapError::new(format!(
                "data manifest has invalid URL for {}: {} ({error})",
                file.name, file.url
            ))
        })?;
        if url.scheme() != "https" {
            return Err(BootstrapError::new(format!(
                "data manifest URL for {} must use HTTPS, got {}",
                file.name, file.url
            )));
        }
    }

    for required in REQUIRED_DATA_FILES {
        if !data.iter().any(|file| file.name == required) {
            return Err(BootstrapError::new(format!(
                "data manifest is missing required file {required}"
            )));
        }
    }
    Ok(())
}

fn is_safe_data_file_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn missing_data_files(data_dir: &Path, names: &[&'static str]) -> Vec<&'static str> {
    names
        .iter()
        .copied()
        .filter(|name| !data_dir.join(name).is_file())
        .collect()
}

pub async fn bootstrap_missing_data(
    data_dir: &Path,
    options: &BootstrapOptions,
    identity: Option<&ChannelIdentity>,
) -> Result<(), BootstrapError> {
    bootstrap_missing_data_with_key(data_dir, options, identity, PINNED_DATA_MANIFEST_PUBLIC_KEY)
        .await
}

async fn bootstrap_missing_data_with_key(
    data_dir: &Path,
    options: &BootstrapOptions,
    identity: Option<&ChannelIdentity>,
    public_key: &str,
) -> Result<(), BootstrapError> {
    let missing_required = missing_data_files(data_dir, &REQUIRED_DATA_FILES);
    let missing_best_effort = missing_data_files(data_dir, &BEST_EFFORT_DATA_FILES);
    if missing_required.is_empty() && missing_best_effort.is_empty() {
        return Ok(());
    }

    if options.no_data_download {
        if missing_required.is_empty() {
            warn!(
                files = ?missing_best_effort,
                "optional data files are missing; server-hosted drafts will remain disabled"
            );
            return Ok(());
        }

        let resolution = resolve_manifest(options.manifest_url_override.clone(), identity)?;
        return Err(BootstrapError::new(format!(
            "missing data files {}; --no-data-download prevents fetching them. Pre-provision PHASE_DATA_DIR or retry without --no-data-download (manifest: {})",
            missing_required.join(", "),
            resolution.url()
        )));
    }

    let resolution = match resolve_manifest(options.manifest_url_override.clone(), identity) {
        Ok(resolution) => resolution,
        Err(error) if missing_required.is_empty() => {
            warn!(
                files = ?missing_best_effort,
                error = %error,
                "optional data files are missing but no manifest is available; server-hosted drafts will remain disabled"
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| BootstrapError::new(format!("failed to build HTTP client: {error}")))?;
    let manifest_url = resolution.url();
    let manifest_bytes = fetch_bytes(&client, manifest_url, "data manifest").await?;
    let signature_url = Url::parse(&format!("{manifest_url}.minisig")).map_err(|error| {
        BootstrapError::new(format!(
            "could not construct minisign URL for manifest {manifest_url}: {error}"
        ))
    })?;
    let signature_bytes = fetch_bytes(&client, &signature_url, "data manifest signature").await?;
    verify_manifest_signature(&manifest_bytes, &signature_bytes, public_key)?;

    let data = parse_manifest_data(&manifest_bytes, identity)?;
    for file in data.iter().filter(|file| {
        missing_required.iter().any(|name| *name == file.name)
            || missing_best_effort.iter().any(|name| *name == file.name)
    }) {
        if BEST_EFFORT_DATA_FILES.contains(&file.name.as_str()) {
            if let Err(error) = download_data_file(&client, data_dir, file).await {
                warn!(
                    file = %file.name,
                    error = %error,
                    "optional data file could not be bootstrapped; server-hosted drafts will remain disabled"
                );
            }
        } else {
            download_data_file(&client, data_dir, file).await?;
        }
    }

    let still_missing = missing_data_files(data_dir, &REQUIRED_DATA_FILES);
    if !still_missing.is_empty() {
        return Err(BootstrapError::new(format!(
            "data bootstrap did not create required files {} from manifest {}",
            still_missing.join(", "),
            manifest_url
        )));
    }
    Ok(())
}

async fn download_data_file(
    client: &Client,
    data_dir: &Path,
    file: &DataFile,
) -> Result<(), BootstrapError> {
    let url = Url::parse(&file.url).map_err(|error| {
        BootstrapError::new(format!(
            "data manifest has invalid URL for {}: {} ({error})",
            file.name, file.url
        ))
    })?;
    let bytes = fetch_bytes(client, &url, &format!("data file {}", file.name)).await?;
    verify_sha256(&bytes, &file.sha256, &file.name, &url)?;
    write_verified_data_file(data_dir, &file.name, &bytes).await
}

async fn fetch_bytes(
    client: &Client,
    url: &Url,
    resource: &str,
) -> Result<Vec<u8>, BootstrapError> {
    let response = client.get(url.clone()).send().await.map_err(|error| {
        BootstrapError::new(format!("failed to fetch {resource} from {url}: {error}"))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(BootstrapError::new(format!(
            "failed to fetch {resource} from {url}: HTTP {status}"
        )));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| {
            BootstrapError::new(format!("failed to read {resource} from {url}: {error}"))
        })
}

fn verify_manifest_signature(
    manifest: &[u8],
    signature: &[u8],
    public_key_base64: &str,
) -> Result<(), BootstrapError> {
    let public_key = PublicKey::from_base64(public_key_base64).map_err(|error| {
        BootstrapError::new(format!("invalid pinned minisign public key: {error}"))
    })?;
    let signature_text = std::str::from_utf8(signature).map_err(|error| {
        BootstrapError::new(format!("manifest signature is not UTF-8: {error}"))
    })?;
    let signature = Signature::decode(signature_text).map_err(|error| {
        BootstrapError::new(format!("invalid manifest minisign signature: {error}"))
    })?;
    public_key
        .verify(manifest, &signature, false)
        .map_err(|error| {
            BootstrapError::new(format!(
                "data manifest signature verification failed: {error}"
            ))
        })
}

fn verify_sha256(
    bytes: &[u8],
    expected: &str,
    name: &str,
    url: &Url,
) -> Result<(), BootstrapError> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(BootstrapError::new(format!(
            "sha256 mismatch for {name} from {url}: expected {expected}, got {actual}"
        )))
    }
}

async fn write_verified_data_file(
    data_dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), BootstrapError> {
    let data_dir = data_dir.to_path_buf();
    let name = name.to_string();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || write_verified_data_file_blocking(&data_dir, &name, &bytes))
        .await
        .map_err(|error| BootstrapError::new(format!("data-file write task failed: {error}")))?
}

fn write_verified_data_file_blocking(
    data_dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), BootstrapError> {
    std::fs::create_dir_all(data_dir).map_err(|error| {
        BootstrapError::new(format!(
            "failed to create data directory {}: {error}",
            data_dir.display()
        ))
    })?;
    let destination = data_dir.join(name);
    let mut temporary = tempfile::Builder::new()
        .prefix(&format!(".{name}."))
        .tempfile_in(data_dir)
        .map_err(|error| {
            BootstrapError::new(format!(
                "failed to create temporary file for {}: {error}",
                destination.display()
            ))
        })?;
    temporary.write_all(bytes).map_err(|error| {
        BootstrapError::new(format!(
            "failed to write temporary data file for {}: {error}",
            destination.display()
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        BootstrapError::new(format!(
            "failed to sync temporary data file for {}: {error}",
            destination.display()
        ))
    })?;
    temporary.persist(&destination).map_err(|error| {
        BootstrapError::new(format!(
            "failed to atomically install data file {}: {}",
            destination.display(),
            error.error
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        bootstrap_missing_data_with_key, identity_from_markers, parse_manifest_data,
        resolve_manifest, verify_manifest_signature, verify_sha256, write_verified_data_file,
        BootstrapOptions, ChannelIdentity,
    };
    use sha2::{Digest, Sha256};
    use url::Url;

    const TEST_PUBLIC_KEY: &str = "RWSRzbuJXEhfwLu1bCNndDifYla7GFbotc6t1tcuytze2q5NjXbWEmG5";
    const SIGNED_TEST_MANIFEST: &[u8] =
        br#"{"schema":1,"channel":"release","version":"test","data":[]}"#;
    const SIGNED_TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUSRzbuJXEhfwAxxkkM8a0M+p0N9xX6VelN8cNVVk9DmUmIUAK7Ga87HN5vjnQn7R4VP1/Lb2DwG8kOI6dj99fNMqYqkpT5DTwM=\ntrusted comment: timestamp:1784645957\tfile:manifest.json\thashed\no9B8aeyirZqbDD1N2/k4voUfPunqsm7iWKqMm6kCOLGrZlx+s5ePOk8m7DejRy/Vg0KsTAsRsg4MNt7ICyICDA==\n";

    #[test]
    fn parses_release_manifest_and_ignores_unknown_fields() {
        let manifest = br#"{
            "schema": 1,
            "channel": "release",
            "version": "test",
            "generated_at": "2026-07-21T00:00:00Z",
            "data": [
                {"name": "card-data.json", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "url": "https://example.test/card-data.json", "future": true},
                {"name": "draft-pools.json", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "url": "https://example.test/draft-pools.json"}
            ],
            "future": "ignored"
        }"#;

        let data = parse_manifest_data(manifest, None).expect("release manifest parses");

        assert_eq!(data.len(), 2);
        assert_eq!(data[0].name, "card-data.json");
    }

    #[test]
    fn parses_preview_manifest_and_selects_embedded_fingerprint() {
        let manifest = br#"{
            "schema": 1,
            "channel": "preview",
            "generated_at": "2026-07-21T00:00:00Z",
            "current": "0123456789abcdef",
            "previous": null,
            "fingerprints": {
                "0123456789abcdef": {
                    "commit": "abc",
                    "binaries": {},
                    "data": [
                        {"name": "card-data.json", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "url": "https://example.test/card-data.json"},
                        {"name": "draft-pools.json", "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "url": "https://example.test/draft-pools.json"}
                    ],
                    "future": "ignored"
                }
            },
            "future": true
        }"#;
        let identity = ChannelIdentity::Preview {
            fingerprint: "0123456789abcdef".to_string(),
        };

        let data = parse_manifest_data(manifest, Some(&identity)).expect("preview manifest parses");

        assert_eq!(data.len(), 2);
        assert_eq!(data[1].name, "draft-pools.json");
    }

    #[test]
    fn rejects_unsupported_manifest_schema() {
        let manifest = br#"{"schema":2,"channel":"release","version":"test","data":[]}"#;

        let error = parse_manifest_data(manifest, None).expect_err("schema 2 must fail");

        assert!(error.to_string().contains("schema 2"));
    }

    #[test]
    fn rejects_non_https_manifest_data_urls() {
        let manifest = br#"{
            "schema": 1,
            "channel": "release",
            "version": "test",
            "data": [
                {"name": "card-data.json", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "url": "http://example.test/card-data.json"}
            ]
        }"#;

        let error = parse_manifest_data(manifest, None).expect_err("HTTP data URL must fail");

        assert!(error.to_string().contains("must use HTTPS"));
    }

    #[test]
    fn release_manifest_allows_missing_optional_draft_pools() {
        let manifest = br#"{
            "schema": 1,
            "channel": "release",
            "version": "test",
            "data": [
                {"name": "card-data.json", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "url": "https://example.test/card-data.json"}
            ]
        }"#;

        parse_manifest_data(manifest, None).expect("draft pools are optional manifest data");
    }

    #[test]
    fn rejects_non_https_manifest_override() {
        let error = resolve_manifest(
            Some(Url::parse("http://example.test/manifest.json").expect("URL")),
            None,
        )
        .expect_err("HTTP manifest override must fail");

        assert!(error.to_string().contains("must use HTTPS"));
    }

    #[test]
    fn signature_verification_accepts_signed_fixture_and_rejects_tampering() {
        verify_manifest_signature(
            SIGNED_TEST_MANIFEST,
            SIGNED_TEST_SIGNATURE.as_bytes(),
            TEST_PUBLIC_KEY,
        )
        .expect("throwaway minisign fixture verifies");

        let error = verify_manifest_signature(
            b"tampered",
            SIGNED_TEST_SIGNATURE.as_bytes(),
            TEST_PUBLIC_KEY,
        )
        .expect_err("tampered manifest must not verify");

        assert!(error.to_string().contains("verification failed"));
    }

    #[tokio::test]
    async fn verifies_sha256_and_writes_with_atomic_replace() {
        let temp = tempfile::tempdir().expect("temp dir");
        let bytes = b"verified data";
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let url = Url::parse("https://example.test/data.json").expect("URL");

        verify_sha256(bytes, &sha256, "card-data.json", &url).expect("hash matches");
        write_verified_data_file(temp.path(), "card-data.json", bytes)
            .await
            .expect("atomic write");

        assert_eq!(
            std::fs::read(temp.path().join("card-data.json")).expect("read written file"),
            bytes
        );
        assert!(verify_sha256(b"wrong", &sha256, "card-data.json", &url).is_err());
    }

    #[tokio::test]
    async fn present_files_skip_manifest_fetch() {
        let temp = tempfile::tempdir().expect("temp dir");
        for name in ["card-data.json", "draft-pools.json"] {
            std::fs::write(temp.path().join(name), "self-hosted data").expect("write data");
        }
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("http://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };

        bootstrap_missing_data_with_key(temp.path(), &options, None, TEST_PUBLIC_KEY)
            .await
            .expect("present files avoid all network access");
    }

    #[tokio::test]
    async fn only_missing_draft_pools_without_identity_is_best_effort() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("card-data.json"), "self-hosted data")
            .expect("write card data");
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: false,
        };

        bootstrap_missing_data_with_key(temp.path(), &options, None, TEST_PUBLIC_KEY)
            .await
            .expect("missing optional draft pools must not prevent startup");
    }

    #[tokio::test]
    async fn only_missing_draft_pools_with_no_data_download_is_best_effort() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("card-data.json"), "self-hosted data")
            .expect("write card data");
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: true,
        };

        bootstrap_missing_data_with_key(temp.path(), &options, None, TEST_PUBLIC_KEY)
            .await
            .expect("--no-data-download must not prevent startup for optional draft pools");
    }

    #[tokio::test]
    async fn no_data_download_fails_before_network_access() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("draft-pools.json"), "self-hosted data")
            .expect("write draft pools");
        let manifest_url = Url::parse("https://127.0.0.1:1/manifest.json").expect("URL");
        let options = BootstrapOptions {
            manifest_url_override: Some(manifest_url.clone()),
            no_data_download: true,
        };

        let error = bootstrap_missing_data_with_key(temp.path(), &options, None, TEST_PUBLIC_KEY)
            .await
            .expect_err("missing data must fail without a download");

        assert!(error.to_string().contains("card-data.json"));
        assert!(!error
            .to_string()
            .contains("card-data.json, draft-pools.json"));
        assert!(error.to_string().contains(manifest_url.as_str()));
    }

    #[tokio::test]
    async fn missing_card_data_without_identity_remains_fatal() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join("draft-pools.json"), "self-hosted data")
            .expect("write draft pools");
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: false,
        };

        let error = bootstrap_missing_data_with_key(temp.path(), &options, None, TEST_PUBLIC_KEY)
            .await
            .expect_err("missing card data without an identity must fail");

        assert!(error.to_string().contains("card-data.json"));
        assert!(error.to_string().contains("no PHASE_CHANNEL identity"));
    }

    #[test]
    fn channel_markers_require_preview_fingerprint() {
        assert_eq!(
            identity_from_markers(Some("release"), None).expect("release identity"),
            Some(ChannelIdentity::Release)
        );
        assert!(identity_from_markers(Some("preview"), None).is_err());
        assert!(identity_from_markers(Some("preview"), Some("too-short")).is_err());
        assert_eq!(
            identity_from_markers(None, None).expect("no identity"),
            None
        );
    }
}
