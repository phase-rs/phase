use std::fmt;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
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
pub const CARD_DATA_FILE: &str = "card-data.json";
pub const DRAFT_POOLS_FILE: &str = "draft-pools.json";
const REQUIRED_DATA_FILES: [&str; 1] = [CARD_DATA_FILE];
const BEST_EFFORT_DATA_FILES: [&str; 1] = [DRAFT_POOLS_FILE];

/// A copy moved aside for the duration of one replacement attempt. Never
/// overwritten: if one is here, an attempt that did not finish left the only
/// copy of that file in it.
const HELD_SUFFIX: &str = ".replacing";
/// A copy that a usable file superseded, kept for the operator to inspect.
const RETIRED_SUFFIX: &str = ".unusable";

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

/// Loads a bootstrapped data file, replacing it once if the provisioned copy is
/// not usable by this binary. Presence is not usability: a data directory
/// carried across an upgrade can hold a file this binary cannot deserialize, and
/// the only judgement that settles it without a network round trip is the loader
/// itself. `options` of `None` means this directory is not manifest-managed —
/// the file is loaded and never replaced.
///
/// A replacement is attempted only when a manifest resolves, and an attempt that
/// does not end in a usable file puts the held copy back over anything the
/// refill installed — this start's copy, or one left by an earlier start —
/// and it is kept as `<name>.unusable` only once a usable file is in place.
pub async fn load_data_file<T, E: fmt::Display>(
    data_dir: &Path,
    name: &str,
    options: Option<&BootstrapOptions>,
    identity: Option<&ChannelIdentity>,
    load: impl Fn(&Path) -> Result<T, E>,
) -> Result<T, BootstrapError> {
    load_data_file_with_key(
        data_dir,
        name,
        options,
        identity,
        PINNED_DATA_MANIFEST_PUBLIC_KEY,
        load,
    )
    .await
}

async fn load_data_file_with_key<T, E: fmt::Display>(
    data_dir: &Path,
    name: &str,
    options: Option<&BootstrapOptions>,
    identity: Option<&ChannelIdentity>,
    public_key: &str,
    load: impl Fn(&Path) -> Result<T, E>,
) -> Result<T, BootstrapError> {
    let path = data_dir.join(name);
    // Reducing the loader's error to a `String` here keeps a non-`Send`
    // `Box<dyn Error>` from crossing the awaits below.
    let reason = match load(&path) {
        Ok(value) => {
            // A usable file at the live path supersedes any copy held aside for
            // it, so an ordinary healthy start finishes a replacement an earlier
            // start could not.
            if options.is_some() {
                retire_held_copy(data_dir, name);
            }
            return Ok(value);
        }
        Err(error) => error.to_string(),
    };
    // Every error below is built from this, so each names the file it is about
    // and only that file. With an empty detail it is the message this server
    // composed for a load failure before it could replace one.
    let unusable = |detail: &str| {
        BootstrapError::new(format!(
            "failed to load {}: {reason}{detail}",
            path.display()
        ))
    };

    let Some(options) = options else {
        return Err(unusable(""));
    };

    // `resolve_manifest` opens no socket, and the refill resolves the same two
    // values: deciding here means nothing is moved when no replacement could
    // succeed anyway.
    let manifest = resolve_manifest(options.manifest_url_override.clone(), identity);
    if options.no_data_download {
        return Err(unusable(&match (&manifest, &options.manifest_url_override) {
            (Ok(resolution), _) => format!(
                "; --no-data-download prevents replacing it from {}. Re-run without --no-data-download, or put a usable copy in place.",
                resolution.url()
            ),
            // An override that is not HTTPS is a manifest that *is* configured
            // and cannot be used; quoting the reason names the URL to fix.
            (Err(error), Some(_)) => format!(
                "; --no-data-download prevents replacing it, and the configured data manifest cannot be used: {error}. Put a usable copy in place."
            ),
            (Err(_), None) => "; --no-data-download prevents replacing it, and no data manifest is configured to replace it from. Put a usable copy in place.".to_string(),
        }));
    }
    let manifest_url = match &manifest {
        Ok(resolution) => resolution.url().clone(),
        // `resolve_manifest` fails for two reasons: an override that is not
        // HTTPS, which is worth quoting, and no identity with no override, whose
        // wording is about a missing card-data.json and would be false for any
        // other member of this class.
        Err(error) => {
            return Err(unusable(&match &options.manifest_url_override {
                Some(_) => format!("; it cannot be replaced automatically: {error}"),
                None => "; it cannot be replaced automatically: this binary has no PHASE_CHANNEL identity and no --data-manifest-url was given. Put a usable copy in place, or pass --data-manifest-url <url>.".to_string(),
            }));
        }
    };

    let held = hold_unusable_file(data_dir, name, &reason).map_err(|detail| unusable(&detail))?;

    // One attempt: neither the refill nor the loader runs twice on any path
    // through this statement. The second arm is true both when the refill
    // installed an unusable file and when it installed nothing.
    let replaced = match bootstrap_missing_data_with_key(data_dir, options, identity, public_key)
        .await
    {
        Err(error) => Err(format!(
            "; replacing it from {manifest_url} failed: {error}"
        )),
        Ok(()) => load(&path).map_err(|second| {
            format!("; replacing it from {manifest_url} did not produce a usable file: {second}")
        }),
    };

    match replaced {
        Ok(value) => {
            retire_held_copy(data_dir, name);
            Ok(value)
        }
        Err(detail) => {
            // The copy discarded here is the one this process just downloaded
            // from the signed manifest, which the manifest can produce again.
            let restored = match &held {
                None => String::new(),
                Some(held) => restore_held_copy(held, &path),
            };
            Err(unusable(&format!("{detail}{restored}")))
        }
    }
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

/// Moves a data file this binary cannot use out of the way so the bootstrap can
/// install a replacement, and reports the copy the refill must not lose, which an
/// earlier interrupted start may already have moved aside. Returns `None` only
/// when there is no such copy, which is how an absent file reaches the same
/// refill.
/// The error is the detail clause for the caller's message, not a full message.
fn hold_unusable_file(
    data_dir: &Path,
    name: &str,
    reason: &str,
) -> Result<Option<PathBuf>, String> {
    let path = data_dir.join(name);
    let held = data_dir.join(format!("{name}{HELD_SUFFIX}"));
    match held.try_exists() {
        // A held copy with nothing at the live path is a replacement interrupted
        // between the two, and the refill is what finishes it. The held copy is
        // this attempt's rollback, so a failed refill puts it back at the live
        // path rather than leaving the directory half-open.
        Ok(true) if matches!(path.try_exists(), Ok(false)) => return Ok(Some(held)),
        Ok(true) => {
            return Err(format!(
                "; an interrupted replacement left the previous copy at {}, which is not overwritten. Move it back to {} or remove it to allow another automatic replacement.",
                held.display(),
                path.display()
            ))
        }
        // Unknown counts as occupied: the guard protects a copy that may be the
        // only one.
        Err(error) => {
            return Err(format!(
                "; the data directory could not be checked for an interrupted replacement at {}: {error}",
                held.display()
            ))
        }
        Ok(false) => {}
    }
    match std::fs::rename(&path, &held) {
        Ok(()) => {
            warn!(
                file = %path.display(),
                held = %held.display(),
                reason = reason,
                "data file is unusable by this server; moving it aside to replace it from the data manifest"
            );
            Ok(Some(held))
        }
        // The loader failed because the file is absent, and the refill that
        // follows is the remedy.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        // True whether or not a file was there: a read-only data filesystem
        // answers EROFS rather than NotFound, so this arm is reached for an
        // absent file too.
        Err(error) => Err(format!(
            "; nothing could be moved aside to {}: {error}",
            held.display()
        )),
    }
}

/// Sets aside the copy held for `name`, now that a file this binary can use is
/// in place. A no-op when nothing is held, so a start that finds a usable file
/// finishes a replacement an earlier start could not, without knowing one
/// happened. Never fatal: the server has its data either way.
fn retire_held_copy(data_dir: &Path, name: &str) {
    let held = data_dir.join(format!("{name}{HELD_SUFFIX}"));
    // A rename failure says nothing about a copy that is not there: a read-only
    // data filesystem answers EROFS either way. Unknown counts as held, as in
    // hold_unusable_file.
    if matches!(held.try_exists(), Ok(false)) {
        return;
    }
    let retired = data_dir.join(format!("{name}{RETIRED_SUFFIX}"));
    match std::fs::rename(&held, &retired) {
        Ok(()) => warn!(
            file = name,
            retired = %retired.display(),
            "a copy of this data file that this server could not use was set aside for inspection, replacing any previous copy there"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(
            file = name,
            held = %held.display(),
            error = %error,
            "a copy of this data file that this server could not use could not be set aside; it is still here, and no automatic replacement of this file will be attempted until it is moved or removed"
        ),
    }
}

/// Puts a held copy back over whatever the refill installed, and returns the
/// clause describing what happened — reported rather than assumed, so the
/// message stays true when the rename fails.
fn restore_held_copy(held: &Path, path: &Path) -> String {
    match std::fs::rename(held, path) {
        Ok(()) => format!("; {} was put back", path.display()),
        Err(error) => format!(
            "; the previous copy could not be put back and remains at {}: {error}",
            held.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::{
        bootstrap_missing_data_with_key, hold_unusable_file, identity_from_markers,
        load_data_file_with_key, parse_manifest_data, resolve_manifest, restore_held_copy,
        retire_held_copy, verify_manifest_signature, verify_sha256, write_verified_data_file,
        BootstrapOptions, ChannelIdentity, CARD_DATA_FILE, DRAFT_POOLS_FILE,
    };
    use sha2::{Digest, Sha256};
    use url::Url;

    /// Sidecar names spelled literally, so a renamed suffix fails the assertions
    /// rather than moving with them.
    fn held(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.replacing"))
    }

    fn retired(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.unusable"))
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read file")
    }

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

    #[tokio::test]
    async fn a_usable_file_is_loaded_without_touching_the_directory_or_the_network() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "CARDS").expect("write card data");
        std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "POOLS").expect("write draft pools");
        // Not HTTPS: resolving before loading would fail the message assertions too.
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("http://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };
        let calls = Cell::new(0u32);

        load_data_file_with_key(
            temp.path(),
            CARD_DATA_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await
        .expect("a usable file loads");

        assert_eq!(calls.get(), 1);
        assert!(!held(temp.path(), CARD_DATA_FILE).exists());
        assert!(!retired(temp.path(), CARD_DATA_FILE).exists());
        assert_eq!(read(&temp.path().join(CARD_DATA_FILE)), "CARDS");
        assert_eq!(read(&temp.path().join(DRAFT_POOLS_FILE)), "POOLS");
    }

    #[tokio::test]
    async fn a_held_copy_is_set_aside_by_the_next_start_that_can_serve() {
        let seed = || {
            let temp = tempfile::tempdir().expect("temp dir");
            std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "LIVE").expect("write live");
            std::fs::write(held(temp.path(), DRAFT_POOLS_FILE), "ORIGINAL").expect("write held");
            temp
        };
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: false,
        };

        let managed = seed();
        let calls = Cell::new(0u32);
        load_data_file_with_key(
            managed.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await
        .expect("a usable file loads");

        assert_eq!(calls.get(), 1);
        assert!(!held(managed.path(), DRAFT_POOLS_FILE).exists());
        assert_eq!(read(&retired(managed.path(), DRAFT_POOLS_FILE)), "ORIGINAL");
        assert_eq!(read(&managed.path().join(DRAFT_POOLS_FILE)), "LIVE");

        let unmanaged = seed();
        load_data_file_with_key(
            unmanaged.path(),
            DRAFT_POOLS_FILE,
            None,
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Ok(()) },
        )
        .await
        .expect("a usable file loads with no bootstrap options");

        assert_eq!(read(&held(unmanaged.path(), DRAFT_POOLS_FILE)), "ORIGINAL");
        assert!(!retired(unmanaged.path(), DRAFT_POOLS_FILE).exists());
    }

    #[tokio::test]
    async fn no_data_download_reports_the_unusable_file_without_touching_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "STALE").expect("write card data");
        let manifest_url = Url::parse("https://127.0.0.1:1/manifest.json").expect("URL");
        let options = BootstrapOptions {
            manifest_url_override: Some(manifest_url.clone()),
            no_data_download: true,
        };
        let calls = Cell::new(0u32);

        let message = load_data_file_with_key(
            temp.path(),
            CARD_DATA_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Err("unknown variant `Typed`".to_string())
            },
        )
        .await
        .expect_err("an unusable file must fail when it may not be downloaded")
        .to_string();

        assert!(message.contains(CARD_DATA_FILE), "{message}");
        assert!(message.contains("unknown variant `Typed`"), "{message}");
        assert!(message.contains("--no-data-download"), "{message}");
        assert!(message.contains(manifest_url.as_str()), "{message}");
        assert_eq!(read(&temp.path().join(CARD_DATA_FILE)), "STALE");
        assert!(!held(temp.path(), CARD_DATA_FILE).exists());
        assert!(!retired(temp.path(), CARD_DATA_FILE).exists());
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn no_data_download_distinguishes_an_unusable_manifest_from_no_manifest() {
        let seed = || {
            let temp = tempfile::tempdir().expect("temp dir");
            std::fs::write(temp.path().join(CARD_DATA_FILE), "STALE").expect("write card data");
            temp
        };
        let load = |_: &Path| -> Result<(), String> { Err("unknown variant `Typed`".to_string()) };

        let configured = seed();
        let override_url = Url::parse("http://example.test/m.json").expect("URL");
        let options = BootstrapOptions {
            manifest_url_override: Some(override_url.clone()),
            no_data_download: true,
        };
        let message = load_data_file_with_key(
            configured.path(),
            CARD_DATA_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            load,
        )
        .await
        .expect_err("a manifest that cannot be used must fail")
        .to_string();

        assert!(message.contains(override_url.as_str()), "{message}");
        assert!(message.contains("must use HTTPS"), "{message}");
        assert!(message.contains("--no-data-download"), "{message}");
        assert!(message.contains(CARD_DATA_FILE), "{message}");
        assert_eq!(read(&configured.path().join(CARD_DATA_FILE)), "STALE");
        assert!(!held(configured.path(), CARD_DATA_FILE).exists());

        let unconfigured = seed();
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: true,
        };
        let message = load_data_file_with_key(
            unconfigured.path(),
            CARD_DATA_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            load,
        )
        .await
        .expect_err("no configured manifest must fail")
        .to_string();

        assert!(
            message.contains("no data manifest is configured"),
            "{message}"
        );
        assert!(!message.contains("must use HTTPS"), "{message}");
        assert!(message.contains("--no-data-download"), "{message}");
        assert!(!held(unconfigured.path(), CARD_DATA_FILE).exists());
    }

    #[tokio::test]
    async fn an_unusable_file_is_not_touched_when_no_manifest_can_replace_it() {
        let seed = || {
            let temp = tempfile::tempdir().expect("temp dir");
            std::fs::write(temp.path().join(CARD_DATA_FILE), "CARDS").expect("write card data");
            std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "POOLS").expect("write pools");
            temp
        };

        let no_manifest = seed();
        let calls = Cell::new(0u32);
        let options = BootstrapOptions {
            manifest_url_override: None,
            no_data_download: false,
        };
        let message = load_data_file_with_key(
            no_manifest.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Err("missing field `code`".to_string())
            },
        )
        .await
        .expect_err("an unusable file with no manifest must fail")
        .to_string();

        assert!(message.contains(DRAFT_POOLS_FILE), "{message}");
        assert!(message.contains("missing field `code`"), "{message}");
        assert!(
            message.contains("cannot be replaced automatically"),
            "{message}"
        );
        // The refill's own no-identity wording is a sentence about card-data.json,
        // which is present here and was never loaded.
        assert!(!message.contains(CARD_DATA_FILE), "{message}");
        assert!(!held(no_manifest.path(), DRAFT_POOLS_FILE).exists());
        assert_eq!(read(&no_manifest.path().join(DRAFT_POOLS_FILE)), "POOLS");
        assert_eq!(read(&no_manifest.path().join(CARD_DATA_FILE)), "CARDS");
        assert_eq!(calls.get(), 1);

        let with_manifest = seed();
        let manifest_url = Url::parse("https://127.0.0.1:1/manifest.json").expect("URL");
        let options = BootstrapOptions {
            manifest_url_override: Some(manifest_url.clone()),
            no_data_download: false,
        };
        let message = load_data_file_with_key(
            with_manifest.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Err("missing field `code`".to_string()) },
        )
        .await
        .expect_err("an unreachable manifest must fail")
        .to_string();

        assert!(message.contains(manifest_url.as_str()), "{message}");
        assert!(message.contains("replacing it from"), "{message}");
        assert_eq!(read(&with_manifest.path().join(DRAFT_POOLS_FILE)), "POOLS");
    }

    #[tokio::test]
    async fn a_failed_replacement_puts_the_required_file_back_and_is_attempted_once() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "ORIGINAL").expect("write card data");
        std::fs::write(retired(temp.path(), CARD_DATA_FILE), "OLDER").expect("write retired");
        std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "POOLS").expect("write pools");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        let accepts = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        let manifest_url =
            Url::parse(&format!("https://127.0.0.1:{port}/manifest.json")).expect("URL");
        let options = BootstrapOptions {
            manifest_url_override: Some(manifest_url.clone()),
            no_data_download: false,
        };
        let calls = Cell::new(0u32);

        let message = load_data_file_with_key(
            temp.path(),
            CARD_DATA_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Err("unknown variant `Typed`".to_string())
            },
        )
        .await
        .expect_err("a manifest that serves nothing cannot replace the file")
        .to_string();

        assert!(message.contains("unknown variant `Typed`"), "{message}");
        assert!(message.contains(manifest_url.as_str()), "{message}");
        assert!(
            message.contains("failed to fetch data manifest"),
            "{message}"
        );
        assert!(message.contains("was put back"), "{message}");
        assert_eq!(read(&temp.path().join(CARD_DATA_FILE)), "ORIGINAL");
        assert_eq!(read(&retired(temp.path(), CARD_DATA_FILE)), "OLDER");
        assert!(!held(temp.path(), CARD_DATA_FILE).exists());
        assert_eq!(calls.get(), 1);
        // A second refill raises this; nothing but a real refill raises it at all.
        assert_eq!(accepts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn without_bootstrap_options_the_file_is_loaded_but_never_replaced() {
        let seed = || {
            let temp = tempfile::tempdir().expect("temp dir");
            std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "POOLS").expect("write pools");
            temp
        };

        let unmanaged = seed();
        let calls = Cell::new(0u32);
        let message = load_data_file_with_key(
            unmanaged.path(),
            DRAFT_POOLS_FILE,
            None,
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Err("stale pool shape".to_string())
            },
        )
        .await
        .expect_err("an unusable file still fails")
        .to_string();

        assert_eq!(
            message,
            format!(
                "failed to load {}: stale pool shape",
                unmanaged.path().join(DRAFT_POOLS_FILE).display()
            )
        );
        assert_eq!(calls.get(), 1);
        assert!(!unmanaged.path().join(CARD_DATA_FILE).exists());
        assert!(!held(unmanaged.path(), DRAFT_POOLS_FILE).exists());
        assert!(!retired(unmanaged.path(), DRAFT_POOLS_FILE).exists());
        assert_eq!(read(&unmanaged.path().join(DRAFT_POOLS_FILE)), "POOLS");

        let managed = seed();
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };
        let message = load_data_file_with_key(
            managed.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Err("stale pool shape".to_string()) },
        )
        .await
        .expect_err("an unreachable manifest must fail")
        .to_string();

        assert!(message.contains("replacing it from"), "{message}");
        assert_eq!(read(&managed.path().join(DRAFT_POOLS_FILE)), "POOLS");
    }

    #[tokio::test]
    async fn an_interrupted_replacement_with_a_live_file_is_not_overwritten() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "REFILLED").expect("write live");
        std::fs::write(held(temp.path(), DRAFT_POOLS_FILE), "ORIGINAL").expect("write held");
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };
        let calls = Cell::new(0u32);

        let message = load_data_file_with_key(
            temp.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                Err("stale pool shape".to_string())
            },
        )
        .await
        .expect_err("a held copy blocks another attempt")
        .to_string();

        assert!(
            message.contains(&format!("{DRAFT_POOLS_FILE}.replacing")),
            "{message}"
        );
        assert!(
            message.contains("remove it to allow another automatic replacement"),
            "{message}"
        );
        assert_eq!(read(&held(temp.path(), DRAFT_POOLS_FILE)), "ORIGINAL");
        assert_eq!(read(&temp.path().join(DRAFT_POOLS_FILE)), "REFILLED");
        assert_eq!(calls.get(), 1);
    }

    /// A name outside the manifest-managed set is what makes the refill a no-op
    /// success: it reports `Ok` only when no managed file is missing, which for
    /// a managed name would require the live path this case needs absent.
    #[tokio::test]
    async fn a_resumed_replacement_retires_the_held_copy_when_the_second_load_succeeds() {
        let name = "other-data.json";
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "CARDS").expect("write card data");
        std::fs::write(temp.path().join(DRAFT_POOLS_FILE), "POOLS").expect("write pools");
        std::fs::write(held(temp.path(), name), "ORIGINAL").expect("write held");
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };
        let calls = Cell::new(0u32);

        load_data_file_with_key(
            temp.path(),
            name,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> {
                calls.set(calls.get() + 1);
                match calls.get() {
                    1 => Err("stale shape".to_string()),
                    _ => Ok(()),
                }
            },
        )
        .await
        .expect("the refill finishes the interrupted replacement");

        assert_eq!(calls.get(), 2);
        assert_eq!(read(&retired(temp.path(), name)), "ORIGINAL");
        assert!(!held(temp.path(), name).exists());
        assert!(!temp.path().join(name).exists());
    }

    #[tokio::test]
    async fn a_failed_refill_puts_an_interrupted_replacement_back_at_the_live_path() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "CARDS").expect("write card data");
        std::fs::write(held(temp.path(), DRAFT_POOLS_FILE), "ORIGINAL").expect("write held");
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };

        let message = load_data_file_with_key(
            temp.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Err("pool file is absent".to_string()) },
        )
        .await
        .expect_err("an unreachable manifest must fail")
        .to_string();

        assert!(message.contains("replacing it from"), "{message}");
        assert!(!message.contains("interrupted replacement"), "{message}");
        assert!(message.contains("was put back"), "{message}");
        assert_eq!(read(&temp.path().join(DRAFT_POOLS_FILE)), "ORIGINAL");
        assert!(!held(temp.path(), DRAFT_POOLS_FILE).exists());
    }

    #[tokio::test]
    async fn an_absent_file_holds_no_copy() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(temp.path().join(CARD_DATA_FILE), "CARDS").expect("write card data");
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };

        let message = load_data_file_with_key(
            temp.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Err("pool file is absent".to_string()) },
        )
        .await
        .expect_err("an absent file cannot be loaded")
        .to_string();

        assert!(
            !message.contains("nothing could be moved aside"),
            "{message}"
        );
        assert!(message.contains("replacing it from"), "{message}");
        assert!(!held(temp.path(), DRAFT_POOLS_FILE).exists());
        assert!(!retired(temp.path(), DRAFT_POOLS_FILE).exists());
        assert_eq!(read(&temp.path().join(CARD_DATA_FILE)), "CARDS");
    }

    /// Requires a non-root user: as root `chmod 555` does not stop the rename.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_copy_that_could_not_be_held_is_not_replaced() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(DRAFT_POOLS_FILE);
        std::fs::write(&path, "POOLS").expect("write pools");
        let options = BootstrapOptions {
            manifest_url_override: Some(
                Url::parse("https://127.0.0.1:1/manifest.json").expect("URL"),
            ),
            no_data_download: false,
        };
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o555))
            .expect("make the data directory unwritable");

        let result = load_data_file_with_key(
            temp.path(),
            DRAFT_POOLS_FILE,
            Some(&options),
            None,
            TEST_PUBLIC_KEY,
            |_: &Path| -> Result<(), String> { Err("stale pool shape".to_string()) },
        )
        .await;

        // Before any assertion, so no failing path leaves the directory unremovable.
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
            .expect("restore the data directory mode");

        let message = result
            .expect_err("a file that could not be held is not replaced")
            .to_string();
        assert!(
            message.contains("nothing could be moved aside"),
            "{message}"
        );
        assert_eq!(read(&path), "POOLS");
    }

    /// The required file, where leaving a file absent is not an option: the held
    /// copy must reach the caller so a failed refill can put it back.
    #[test]
    fn hold_unusable_file_hands_back_an_interrupted_replacement_it_did_not_make() {
        let temp = tempfile::tempdir().expect("temp dir");
        let held_path = held(temp.path(), CARD_DATA_FILE);
        std::fs::write(&held_path, "ORIGINAL").expect("write held");

        let carried = hold_unusable_file(temp.path(), CARD_DATA_FILE, "card data is absent")
            .expect("an interrupted replacement is resumed, not refused");

        assert_eq!(carried.as_deref(), Some(held_path.as_path()));
        assert_eq!(read(&held_path), "ORIGINAL");
        assert!(!temp.path().join(CARD_DATA_FILE).exists());
    }

    #[test]
    fn retire_held_copy_sets_the_copy_aside_and_never_destroys_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        std::fs::write(held(temp.path(), DRAFT_POOLS_FILE), "HELD").expect("write held");
        std::fs::write(retired(temp.path(), DRAFT_POOLS_FILE), "OLDER").expect("write retired");

        retire_held_copy(temp.path(), DRAFT_POOLS_FILE);

        assert_eq!(read(&retired(temp.path(), DRAFT_POOLS_FILE)), "HELD");
        assert!(!held(temp.path(), DRAFT_POOLS_FILE).exists());

        let blocked = tempfile::tempdir().expect("temp dir");
        std::fs::write(held(blocked.path(), DRAFT_POOLS_FILE), "HELD").expect("write held");
        std::fs::create_dir(retired(blocked.path(), DRAFT_POOLS_FILE)).expect("create directory");

        retire_held_copy(blocked.path(), DRAFT_POOLS_FILE);

        assert_eq!(read(&held(blocked.path(), DRAFT_POOLS_FILE)), "HELD");
        assert!(retired(blocked.path(), DRAFT_POOLS_FILE).is_dir());

        #[cfg(unix)]
        {
            // rename() would move a dangling link; try_exists() reads it as nothing held.
            let dangling = tempfile::tempdir().expect("temp dir");
            std::os::unix::fs::symlink(
                dangling.path().join("no-such-file"),
                held(dangling.path(), DRAFT_POOLS_FILE),
            )
            .expect("symlink held");

            retire_held_copy(dangling.path(), DRAFT_POOLS_FILE);

            assert!(
                std::fs::symlink_metadata(held(dangling.path(), DRAFT_POOLS_FILE))
                    .is_ok_and(|metadata| metadata.is_symlink())
            );
            assert!(std::fs::symlink_metadata(retired(dangling.path(), DRAFT_POOLS_FILE)).is_err());
        }
    }

    #[test]
    fn restore_held_copy_reports_whether_the_copy_went_back() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join(DRAFT_POOLS_FILE);
        std::fs::write(held(temp.path(), DRAFT_POOLS_FILE), "HELD").expect("write held");

        let clause = restore_held_copy(&held(temp.path(), DRAFT_POOLS_FILE), &path);

        assert!(clause.contains("was put back"), "{clause}");
        assert_eq!(read(&path), "HELD");
        assert!(!held(temp.path(), DRAFT_POOLS_FILE).exists());

        let blocked = tempfile::tempdir().expect("temp dir");
        let blocked_path = blocked.path().join(DRAFT_POOLS_FILE);
        std::fs::write(held(blocked.path(), DRAFT_POOLS_FILE), "HELD").expect("write held");
        std::fs::create_dir(&blocked_path).expect("create directory");

        let clause = restore_held_copy(&held(blocked.path(), DRAFT_POOLS_FILE), &blocked_path);

        assert!(!clause.contains("was put back"), "{clause}");
        assert!(
            clause.contains(&format!("{DRAFT_POOLS_FILE}.replacing")),
            "{clause}"
        );
        assert_eq!(read(&held(blocked.path(), DRAFT_POOLS_FILE)), "HELD");
    }
}
