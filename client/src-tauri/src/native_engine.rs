use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use minisign_verify::{PublicKey, Signature};
use reqwest::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::native_bridge::BridgeHandle;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SERVER_ARTIFACT_PUBLIC_KEY: &str = "RWRDZxG2otNoKLblrgD00kM0a8U0CRZUGHpNCr3W+3ik1E84XHcB6hZe";
const NATIVE_ENGINE_DIRECTORY: &str = "native-engine";
const CACHE_DIRECTORY: &str = "cache/sha256";
const SPAWN_RECORD_FILE: &str = "native-engine-spawn-record.json";
const RELEASE_RATCHET_FILE: &str = "native-engine-highest-release-version.json";
const PREVIEW_RATCHET_FILE: &str = "native-engine-preview-generated-at.json";
const MANIFEST_DATA_FILE: &str = "manifest-data.json";
const RELEASE_ORIGIN: &str = "https://phase-rs.dev";
const PREVIEW_ORIGIN: &str = "https://preview.phase-rs.dev";
const PROGRESS_EVENT: &str = "native-engine-progress";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_GRACE: Duration = Duration::from_millis(250);

/// A server artifact identity supplied by first-party remote content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeEngineKey {
    Release { version: String },
    Preview { fingerprint: String },
}

impl NativeEngineKey {
    fn channel(&self) -> &'static str {
        match self {
            Self::Release { .. } => "release",
            Self::Preview { .. } => "preview",
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::Release { version } => version,
            Self::Preview { fingerprint } => fingerprint,
        }
    }

    fn origin(&self) -> &'static str {
        match self {
            Self::Release { .. } => RELEASE_ORIGIN,
            Self::Preview { .. } => PREVIEW_ORIGIN,
        }
    }

    fn directory_name(&self) -> String {
        format!("{}-{}", self.channel(), self.value())
    }

    fn validate(&self) -> Result<(), NativeEngineError> {
        match self {
            Self::Release { version } => Version::parse(version)
                .map(|_| ())
                .map_err(|error| NativeEngineError::invalid_key(error.to_string())),
            Self::Preview { fingerprint }
                if fingerprint.len() == 16
                    && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                Ok(())
            }
            Self::Preview { .. } => Err(NativeEngineError::invalid_key(
                "preview fingerprints must be 16 hexadecimal characters",
            )),
        }
    }
}

/// The only successful IPC response for `ensure_native_engine`.
#[derive(Clone, Debug, Serialize)]
pub struct NativeEngineReady {
    pub port: u16,
}

/// Progress emitted while the shell prepares a native engine.
#[derive(Clone, Debug, Serialize)]
pub struct NativeEngineProgress {
    pub phase: NativeEngineProgressPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeEngineProgressPhase {
    Resolving,
    DownloadingBinary,
    Verifying,
    DownloadingData,
    Spawning,
    Ready,
}

/// Structured IPC failures let the frontend choose its normal WASM fallback.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

impl NativeEngineError {
    fn invalid_key(detail: impl Into<String>) -> Self {
        Self::InvalidKey {
            detail: detail.into(),
        }
    }

    fn storage(error: impl std::fmt::Display) -> Self {
        Self::Storage {
            detail: error.to_string(),
        }
    }

    fn manifest(error: impl std::fmt::Display) -> Self {
        Self::Manifest {
            detail: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DataFile {
    name: String,
    sha256: String,
    url: String,
}

impl DataFile {
    fn validate(&self) -> Result<(), NativeEngineError> {
        let path = Path::new(&self.name);
        if path.file_name().is_none()
            || path.file_name().and_then(|name| name.to_str()) != Some(self.name.as_str())
        {
            return Err(NativeEngineError::manifest(format!(
                "data file name is not a plain filename: {}",
                self.name
            )));
        }
        validate_sha256(&self.sha256)
    }
}

#[derive(Debug, Deserialize)]
struct ReleaseManifest {
    schema: u32,
    channel: String,
    version: String,
    #[allow(dead_code)]
    generated_at: String,
    data: Vec<DataFile>,
}

impl ReleaseManifest {
    fn parse(bytes: &[u8], requested_version: &str) -> Result<Self, NativeEngineError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(NativeEngineError::manifest)?;
        if manifest.schema != 1 {
            return Err(NativeEngineError::manifest(format!(
                "unsupported release manifest schema {}",
                manifest.schema
            )));
        }
        if manifest.channel != "release" {
            return Err(NativeEngineError::manifest(
                "release manifest has the wrong channel",
            ));
        }
        if manifest.version != requested_version {
            return Err(NativeEngineError::manifest(format!(
                "release manifest version {} does not match requested {requested_version}",
                manifest.version
            )));
        }
        validate_data_files(&manifest.data)?;
        Ok(manifest)
    }
}

#[derive(Debug, Deserialize)]
struct PreviewManifest {
    schema: u32,
    channel: String,
    generated_at: String,
    #[allow(dead_code)]
    current: String,
    #[allow(dead_code)]
    previous: Option<String>,
    fingerprints: BTreeMap<String, PreviewManifestEntry>,
}

#[derive(Debug, Deserialize)]
struct PreviewManifestEntry {
    #[allow(dead_code)]
    commit: String,
    binaries: BTreeMap<String, PreviewBinary>,
    data: Vec<DataFile>,
}

#[derive(Debug, Deserialize)]
struct PreviewBinary {
    url: String,
    sig_url: String,
}

impl PreviewManifest {
    fn parse(bytes: &[u8]) -> Result<Self, NativeEngineError> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(NativeEngineError::manifest)?;
        if manifest.schema != 1 {
            return Err(NativeEngineError::manifest(format!(
                "unsupported preview manifest schema {}",
                manifest.schema
            )));
        }
        if manifest.channel != "preview" {
            return Err(NativeEngineError::manifest(
                "preview manifest has the wrong channel",
            ));
        }
        parse_generated_at(&manifest.generated_at)?;
        for (fingerprint, entry) in &manifest.fingerprints {
            NativeEngineKey::Preview {
                fingerprint: fingerprint.clone(),
            }
            .validate()?;
            validate_data_files(&entry.data)?;
        }
        Ok(manifest)
    }

    fn entry_for(&self, fingerprint: &str) -> Result<&PreviewManifestEntry, NativeEngineError> {
        self.fingerprints.get(fingerprint).ok_or_else(|| {
            NativeEngineError::manifest(format!(
                "preview fingerprint {fingerprint} is not in the signed manifest"
            ))
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SpawnRecord {
    pid: u32,
    port: u16,
    key: NativeEngineKey,
}

#[derive(Deserialize, Serialize)]
struct ReleaseRatchet {
    version: String,
}

#[derive(Deserialize, Serialize)]
struct PreviewRatchet {
    generated_at: String,
}

#[derive(Deserialize, Serialize)]
struct StoredManifestData {
    data: Vec<DataFile>,
}

struct NativeEngineFiles {
    app_directory: PathBuf,
    base: PathBuf,
}

impl NativeEngineFiles {
    fn from_app(app: &AppHandle) -> Result<Self, NativeEngineError> {
        let app_directory = app
            .path()
            .app_local_data_dir()
            .map_err(NativeEngineError::storage)?;
        Ok(Self {
            base: app_directory.join(NATIVE_ENGINE_DIRECTORY),
            app_directory,
        })
    }

    fn key_directory(&self, key: &NativeEngineKey) -> PathBuf {
        self.base.join(key.directory_name())
    }

    fn binary(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key).join(binary_file_name())
    }

    fn data_directory(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key).join("data")
    }

    fn cache_directory(&self) -> PathBuf {
        self.base.join(CACHE_DIRECTORY)
    }

    fn cache_blob(&self, sha256: &str) -> PathBuf {
        self.cache_directory().join(sha256)
    }

    fn spawn_record(&self) -> PathBuf {
        self.app_directory.join(SPAWN_RECORD_FILE)
    }

    fn release_ratchet(&self) -> PathBuf {
        self.app_directory.join(RELEASE_RATCHET_FILE)
    }

    fn preview_ratchet(&self) -> PathBuf {
        self.app_directory.join(PREVIEW_RATCHET_FILE)
    }

    fn manifest_data(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key).join(MANIFEST_DATA_FILE)
    }
}

struct ResolvedArtifact {
    binary_url: String,
    binary_signature_url: String,
    data: Vec<DataFile>,
}

enum RunningEngine {
    Child {
        key: NativeEngineKey,
        port: u16,
        child: Child,
        stdin: Option<ChildStdin>,
    },
    Adopted(SpawnRecord),
}

impl RunningEngine {
    fn key(&self) -> &NativeEngineKey {
        match self {
            Self::Child { key, .. } => key,
            Self::Adopted(record) => &record.key,
        }
    }

    fn port(&self) -> u16 {
        match self {
            Self::Child { port, .. } => *port,
            Self::Adopted(record) => record.port,
        }
    }
}

struct NativeEngineState {
    running: Option<RunningEngine>,
    bridges: BTreeMap<u64, BridgeHandle>,
    next_bridge_id: u64,
}

impl Default for NativeEngineState {
    fn default() -> Self {
        Self {
            running: None,
            bridges: BTreeMap::new(),
            next_bridge_id: 1,
        }
    }
}

static ENGINE_STATE: OnceLock<Mutex<NativeEngineState>> = OnceLock::new();
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) enum NativeBridgeRegistryError {
    NotRunning,
    Internal(String),
}

fn engine_state() -> &'static Mutex<NativeEngineState> {
    ENGINE_STATE.get_or_init(|| Mutex::new(NativeEngineState::default()))
}

pub(crate) fn register_native_engine_bridge(
    bridge: BridgeHandle,
) -> Result<(u64, u16, &'static str), NativeBridgeRegistryError> {
    let mut state = engine_state().lock().map_err(|error| {
        NativeBridgeRegistryError::Internal(format!("native engine state lock poisoned: {error}"))
    })?;
    let running = state
        .running
        .as_ref()
        .ok_or(NativeBridgeRegistryError::NotRunning)?;
    let port = running.port();
    let origin = running.key().origin();
    let bridge_id = state.next_bridge_id;
    state.next_bridge_id = state.next_bridge_id.checked_add(1).ok_or_else(|| {
        NativeBridgeRegistryError::Internal("native engine bridge IDs are exhausted".to_owned())
    })?;
    state.bridges.insert(bridge_id, bridge);
    Ok((bridge_id, port, origin))
}

pub(crate) fn native_engine_bridge_sender(
    bridge_id: u64,
) -> Option<tokio::sync::mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Message>> {
    let state = engine_state().lock().ok()?;
    state.bridges.get(&bridge_id).map(BridgeHandle::outbound)
}

pub(crate) fn close_native_engine_bridge(bridge_id: u64) -> bool {
    let bridge = engine_state()
        .lock()
        .ok()
        .and_then(|mut state| state.bridges.remove(&bridge_id));
    if let Some(bridge) = bridge {
        bridge.abort();
        true
    } else {
        false
    }
}

pub(crate) fn remove_native_engine_bridge(bridge_id: u64) {
    if let Ok(mut state) = engine_state().lock() {
        state.bridges.remove(&bridge_id);
    }
}

pub(crate) fn abort_native_engine_bridges_on_navigation() {
    if let Ok(mut state) = engine_state().lock() {
        abort_all_native_engine_bridges(&mut state.bridges);
    }
}

fn abort_all_native_engine_bridges(bridges: &mut BTreeMap<u64, BridgeHandle>) {
    for (_, bridge) in std::mem::take(bridges) {
        bridge.abort();
    }
}

/// Resolves, verifies, provisions, and starts the native server for a typed key.
#[tauri::command]
pub async fn ensure_native_engine(
    app: AppHandle,
    key: NativeEngineKey,
) -> Result<NativeEngineReady, NativeEngineError> {
    tauri::async_runtime::spawn_blocking(move || ensure_native_engine_sync(&app, key))
        .await
        .map_err(|error| NativeEngineError::Internal {
            detail: error.to_string(),
        })?
}

/// Stops the held or adopted native server and removes its persisted record.
#[tauri::command]
pub async fn stop_native_engine(app: AppHandle) -> Result<(), NativeEngineError> {
    tauri::async_runtime::spawn_blocking(move || stop_native_engine_sync(&app))
        .await
        .map_err(|error| NativeEngineError::Internal {
            detail: error.to_string(),
        })?
}

pub fn stop_native_engine_on_exit(app: &AppHandle) {
    let _ = stop_native_engine_sync(app);
}

fn ensure_native_engine_sync(
    app: &AppHandle,
    key: NativeEngineKey,
) -> Result<NativeEngineReady, NativeEngineError> {
    key.validate()?;
    let files = NativeEngineFiles::from_app(app)?;
    fs::create_dir_all(&files.base).map_err(NativeEngineError::storage)?;
    let client = http_client()?;
    let mut state = engine_state()
        .lock()
        .map_err(|error| NativeEngineError::Internal {
            detail: format!("native engine state lock poisoned: {error}"),
        })?;

    check_release_ratchet(&files, &key)?;

    if let Some(running) = state.running.as_mut() {
        if running.key() == &key && health_passes(&client, running.port()) {
            return Ok(NativeEngineReady {
                port: running.port(),
            });
        }
    }

    if let Some(running) = state.running.take() {
        stop_running_engine(running, &files, &mut state.bridges);
        clear_spawn_record(&files)?;
    }

    emit_progress(
        app,
        NativeEngineProgressPhase::Resolving,
        Some(key.directory_name()),
    );
    let preview_resolved = match key {
        NativeEngineKey::Preview { .. } => Some(resolve_artifact(app, &client, &files, &key)?),
        NativeEngineKey::Release { .. } => None,
    };

    if let Some(record) = read_spawn_record(&files)? {
        let healthy = health_passes(&client, record.port);
        if can_adopt(&record, &key, healthy) {
            let port = record.port;
            state.running = Some(RunningEngine::Adopted(record));
            return Ok(NativeEngineReady { port });
        }
        kill_recorded_process_if_ours(&record, &files);
        clear_spawn_record(&files)?;
    }

    let resolved = match preview_resolved {
        Some(resolved) => resolved,
        None => resolve_artifact(app, &client, &files, &key)?,
    };

    emit_progress(
        app,
        NativeEngineProgressPhase::DownloadingBinary,
        Some(key.directory_name()),
    );
    let binary = fetch_bytes(&client, &resolved.binary_url)?;
    let signature = fetch_bytes(&client, &resolved.binary_signature_url)?;
    emit_progress(
        app,
        NativeEngineProgressPhase::Verifying,
        Some("server binary".to_owned()),
    );
    verify_signature(&binary, &signature)?;
    let binary_path = files.binary(&key);
    write_atomically(&binary_path, &binary)?;
    make_executable(&binary_path)?;

    emit_progress(app, NativeEngineProgressPhase::DownloadingData, None);
    assemble_data(&client, Some(app), &files, &key, &resolved.data)?;

    emit_progress(app, NativeEngineProgressPhase::Spawning, None);
    let port = reserve_port()?;
    let (mut child, stdin) = spawn_server(
        &binary_path,
        &files.data_directory(&key),
        port,
        key.origin(),
    )?;
    if let Err(error) = wait_for_health(&client, port, &mut child) {
        let running = RunningEngine::Child {
            key,
            port,
            child,
            stdin: Some(stdin),
        };
        stop_running_engine(running, &files, &mut state.bridges);
        return Err(error);
    }

    let pid = child.id();
    let record = SpawnRecord {
        pid,
        port,
        key: key.clone(),
    };
    write_spawn_record(&files, &record)?;
    persist_release_ratchet(&files, &key)?;
    state.running = Some(RunningEngine::Child {
        key: key.clone(),
        port,
        child,
        stdin: Some(stdin),
    });
    if let Err(error) = gc_after_successful_spawn(&files, &key) {
        eprintln!("native engine GC after successful spawn failed: {error:?}");
    }
    emit_progress(
        app,
        NativeEngineProgressPhase::Ready,
        Some(port.to_string()),
    );
    Ok(NativeEngineReady { port })
}

fn stop_native_engine_sync(app: &AppHandle) -> Result<(), NativeEngineError> {
    let files = NativeEngineFiles::from_app(app)?;
    let mut state = engine_state()
        .lock()
        .map_err(|error| NativeEngineError::Internal {
            detail: format!("native engine state lock poisoned: {error}"),
        })?;
    if let Some(running) = state.running.take() {
        stop_running_engine(running, &files, &mut state.bridges);
        clear_spawn_record(&files)
    } else {
        abort_all_native_engine_bridges(&mut state.bridges);
        // This process owns no engine, so leave the shared on-disk spawn
        // record alone: it may describe another live instance's server
        // (single-instance secondaries hard-exit inside plugin init today,
        // but killing a process this instance did not spawn must never be
        // exit-path behavior). A genuine orphan already self-terminates via
        // `--exit-on-stdin-close` when its shell dies, and any stale record
        // is resolved by the adopt-or-kill at the next ensure_native_engine.
        Ok(())
    }
}

fn http_client() -> Result<Client, NativeEngineError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| NativeEngineError::Download {
            detail: error.to_string(),
        })
}

fn resolve_artifact(
    app: &AppHandle,
    client: &Client,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<ResolvedArtifact, NativeEngineError> {
    match key {
        NativeEngineKey::Release { version } => {
            let asset = format!(
                "phase-server-slim-{}{}",
                target_triple()?,
                executable_suffix()
            );
            let base =
                format!("https://github.com/phase-rs/phase/releases/download/v{version}/{asset}");
            let manifest_url =
                format!("https://data.phase-rs.dev/desktop/release-server-v{version}.json");
            emit_progress(
                app,
                NativeEngineProgressPhase::Verifying,
                Some("release data manifest".to_owned()),
            );
            let manifest =
                ReleaseManifest::parse(&fetch_verified_bytes(client, &manifest_url)?, version)?;
            Ok(ResolvedArtifact {
                binary_url: base.clone(),
                binary_signature_url: format!("{base}.minisig"),
                data: manifest.data,
            })
        }
        NativeEngineKey::Preview { fingerprint } => {
            let manifest_url = "https://data.phase-rs.dev/desktop/preview-server.json";
            emit_progress(
                app,
                NativeEngineProgressPhase::Verifying,
                Some("preview server manifest".to_owned()),
            );
            let manifest = PreviewManifest::parse(&fetch_verified_bytes(client, manifest_url)?)?;
            let entry = manifest.entry_for(fingerprint)?;
            accept_preview_manifest(files, &manifest.generated_at)?;
            let target = target_triple()?;
            let binary = entry
                .binaries
                .get(target)
                .ok_or_else(|| NativeEngineError::Manifest {
                    detail: format!("preview manifest has no {target} binary"),
                })?;
            Ok(ResolvedArtifact {
                binary_url: binary.url.clone(),
                binary_signature_url: binary.sig_url.clone(),
                data: entry.data.clone(),
            })
        }
    }
}

fn fetch_verified_bytes(client: &Client, url: &str) -> Result<Vec<u8>, NativeEngineError> {
    let bytes = fetch_bytes(client, url)?;
    let signature = fetch_bytes(client, &format!("{url}.minisig"))?;
    verify_signature(&bytes, &signature)?;
    Ok(bytes)
}

fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>, NativeEngineError> {
    tauri::async_runtime::block_on(async {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| NativeEngineError::Download {
                detail: format!("{url}: {error}"),
            })?
            .error_for_status()
            .map_err(|error| NativeEngineError::Download {
                detail: format!("{url}: {error}"),
            })?;
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| NativeEngineError::Download {
                detail: format!("{url}: {error}"),
            })
    })
}

fn verify_signature(bytes: &[u8], signature: &[u8]) -> Result<(), NativeEngineError> {
    verify_signature_with_key(SERVER_ARTIFACT_PUBLIC_KEY, bytes, signature)
}

fn verify_signature_with_key(
    public_key: &str,
    bytes: &[u8],
    signature: &[u8],
) -> Result<(), NativeEngineError> {
    let public_key =
        PublicKey::from_base64(public_key).map_err(|error| NativeEngineError::Verification {
            detail: error.to_string(),
        })?;
    let signature =
        std::str::from_utf8(signature).map_err(|error| NativeEngineError::Verification {
            detail: error.to_string(),
        })?;
    let signature =
        Signature::decode(signature).map_err(|error| NativeEngineError::Verification {
            detail: error.to_string(),
        })?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|error| NativeEngineError::Verification {
            detail: error.to_string(),
        })
}

fn check_release_ratchet(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<(), NativeEngineError> {
    let NativeEngineKey::Release { version } = key else {
        return Ok(());
    };
    let requested = Version::parse(version).map_err(|error| NativeEngineError::Downgrade {
        detail: error.to_string(),
    })?;
    let Some(ratchet) = read_json_optional::<ReleaseRatchet>(&files.release_ratchet())? else {
        return Ok(());
    };
    let highest = Version::parse(&ratchet.version).map_err(|error| NativeEngineError::Storage {
        detail: format!("invalid persisted release version: {error}"),
    })?;
    if requested < highest {
        return Err(NativeEngineError::Downgrade {
            detail: format!("requested {requested} is older than already spawned {highest}"),
        });
    }
    Ok(())
}

fn persist_release_ratchet(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<(), NativeEngineError> {
    let NativeEngineKey::Release { version } = key else {
        return Ok(());
    };
    let requested = Version::parse(version).map_err(|error| NativeEngineError::Downgrade {
        detail: error.to_string(),
    })?;
    let current = read_json_optional::<ReleaseRatchet>(&files.release_ratchet())?;
    let should_write = current
        .as_ref()
        .map(|ratchet| Version::parse(&ratchet.version).map_or(true, |highest| requested > highest))
        .unwrap_or(true);
    if should_write {
        write_json_atomically(
            &files.release_ratchet(),
            &ReleaseRatchet {
                version: version.clone(),
            },
        )?;
    }
    Ok(())
}

fn accept_preview_manifest(
    files: &NativeEngineFiles,
    generated_at: &str,
) -> Result<(), NativeEngineError> {
    let accepted = parse_generated_at(generated_at)?;
    if let Some(ratchet) = read_json_optional::<PreviewRatchet>(&files.preview_ratchet())? {
        let last = OffsetDateTime::parse(&ratchet.generated_at, &Rfc3339).map_err(|error| {
            NativeEngineError::Storage {
                detail: format!("invalid persisted preview generated_at: {error}"),
            }
        })?;
        if accepted < last {
            return Err(NativeEngineError::Downgrade {
                detail: format!(
                    "preview manifest generated_at {generated_at} is older than accepted {}",
                    ratchet.generated_at
                ),
            });
        }
    }
    write_json_atomically(
        &files.preview_ratchet(),
        &PreviewRatchet {
            generated_at: generated_at.to_owned(),
        },
    )
}

fn parse_generated_at(value: &str) -> Result<OffsetDateTime, NativeEngineError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(NativeEngineError::manifest)
}

fn assemble_data(
    client: &Client,
    app: Option<&AppHandle>,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    data: &[DataFile],
) -> Result<(), NativeEngineError> {
    validate_data_files(data)?;
    let data_directory = files.data_directory(key);
    fs::create_dir_all(&data_directory).map_err(NativeEngineError::storage)?;
    ensure_writable(&data_directory)?;
    for entry in data {
        let cache_blob = files.cache_blob(&entry.sha256);
        if !cache_blob
            .try_exists()
            .map_err(NativeEngineError::storage)?
        {
            if let Some(app) = app {
                emit_progress(
                    app,
                    NativeEngineProgressPhase::DownloadingData,
                    Some(entry.name.clone()),
                );
            }
            let bytes = fetch_bytes(client, &entry.url)?;
            verify_sha256(&bytes, &entry.sha256)?;
            write_atomically(&cache_blob, &bytes)?;
        }
        let destination = data_directory.join(&entry.name);
        remove_file_if_exists(&destination)?;
        link_or_copy(&cache_blob, &destination)?;
    }
    write_json_atomically(
        &files.manifest_data(key),
        &StoredManifestData {
            data: data.to_vec(),
        },
    )
}

fn ensure_writable(directory: &Path) -> Result<(), NativeEngineError> {
    let probe = directory.join(format!(".write-probe-{}", temporary_suffix()));
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe)
        .map_err(NativeEngineError::storage)?;
    remove_file_if_exists(&probe)
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), NativeEngineError> {
    validate_sha256(expected)?;
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(NativeEngineError::Verification {
            detail: format!("sha256 mismatch: expected {expected}, got {actual}"),
        });
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), NativeEngineError> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(NativeEngineError::manifest(format!(
            "invalid sha256 value {value}"
        )))
    }
}

fn validate_data_files(data: &[DataFile]) -> Result<(), NativeEngineError> {
    for file in data {
        file.validate()?;
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn link_or_copy(source: &Path, destination: &Path) -> Result<(), NativeEngineError> {
    link_or_copy_with(source, destination, |source, destination| {
        fs::hard_link(source, destination)
    })
}

fn link_or_copy_with<F>(
    source: &Path,
    destination: &Path,
    hard_link: F,
) -> Result<(), NativeEngineError>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    match hard_link(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, destination).map_err(NativeEngineError::storage)?;
            Ok(())
        }
    }
}

fn reserve_port() -> Result<u16, NativeEngineError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| NativeEngineError::Spawn {
        detail: error.to_string(),
    })?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| NativeEngineError::Spawn {
            detail: error.to_string(),
        })
}

fn spawn_server(
    binary: &Path,
    data_directory: &Path,
    port: u16,
    origin: &str,
) -> Result<(Child, ChildStdin), NativeEngineError> {
    let mut child = Command::new(binary)
        .env("PORT", port.to_string())
        .env("PHASE_DATA_DIR", data_directory)
        .args([
            "--bind",
            "127.0.0.1",
            "--exit-on-stdin-close",
            "--allowed-origin",
            origin,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| NativeEngineError::Spawn {
            detail: error.to_string(),
        })?;
    let stdin = child.stdin.take().ok_or_else(|| NativeEngineError::Spawn {
        detail: "native engine stdin pipe was unavailable".to_owned(),
    })?;
    Ok((child, stdin))
}

fn wait_for_health(client: &Client, port: u16, child: &mut Child) -> Result<(), NativeEngineError> {
    let deadline = Instant::now() + HEALTH_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|error| NativeEngineError::Spawn {
            detail: format!("failed to poll native engine after spawn: {error}"),
        })? {
            return Err(NativeEngineError::Spawn {
                detail: format!(
                    "native engine exited before becoming healthy on port {port}: {status}"
                ),
            });
        }
        if health_passes(client, port) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(NativeEngineError::Health {
        detail: format!("native engine did not become healthy on port {port}"),
    })
}

fn health_passes(client: &Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/health");
    tauri::async_runtime::block_on(async {
        client
            .get(url)
            .send()
            .await
            .map(|response| response.status() == reqwest::StatusCode::OK)
            .unwrap_or(false)
    })
}

fn read_spawn_record(files: &NativeEngineFiles) -> Result<Option<SpawnRecord>, NativeEngineError> {
    read_json_optional(&files.spawn_record())
}

fn write_spawn_record(
    files: &NativeEngineFiles,
    record: &SpawnRecord,
) -> Result<(), NativeEngineError> {
    write_json_atomically(&files.spawn_record(), record)
}

fn clear_spawn_record(files: &NativeEngineFiles) -> Result<(), NativeEngineError> {
    remove_file_if_exists(&files.spawn_record())
}

fn can_adopt(record: &SpawnRecord, requested: &NativeEngineKey, healthy: bool) -> bool {
    record.key == *requested && healthy
}

fn stop_running_engine(
    running: RunningEngine,
    files: &NativeEngineFiles,
    bridges: &mut BTreeMap<u64, BridgeHandle>,
) {
    abort_all_native_engine_bridges(bridges);
    match running {
        RunningEngine::Child {
            key: _,
            port: _,
            mut child,
            mut stdin,
        } => {
            stdin.take();
            thread::sleep(STOP_GRACE);
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        RunningEngine::Adopted(record) => kill_recorded_process_if_ours(&record, files),
    }
}

fn kill_recorded_process_if_ours(record: &SpawnRecord, files: &NativeEngineFiles) {
    let binary = files.binary(&record.key);
    if !process_is_plausibly_ours(record.pid, &binary) {
        return;
    }
    #[cfg(unix)]
    {
        let pid = record.pid.to_string();
        let _ = Command::new("kill").args(["-TERM", &pid]).status();
        thread::sleep(STOP_GRACE);
        // The PID may have been recycled while we slept; only escalate to KILL
        // if it still looks like our binary.
        if process_is_plausibly_ours(record.pid, &binary) {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &record.pid.to_string(), "/T", "/F"])
            .status();
    }
}

#[cfg(target_os = "linux")]
fn process_is_plausibly_ours(pid: u32, binary: &Path) -> bool {
    let expected = binary.canonicalize().ok();
    let actual = fs::read_link(format!("/proc/{pid}/exe")).ok();
    expected
        .zip(actual)
        .is_some_and(|(expected, actual)| expected == actual)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_plausibly_ours(pid: u32, binary: &Path) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output();
    let Ok(output) = output else {
        return false;
    };
    let command = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let expected = binary.canonicalize().ok();
    // `ps -o comm=` may report a bare executable name rather than the full
    // path on BSD-derived systems; the PID already comes from our own spawn
    // record, so a basename match is sufficient identification here.
    expected.as_ref().is_some_and(|path| {
        let command_path = Path::new(&command);
        path == command_path
            || path
                .file_name()
                .is_some_and(|name| name == command_path.as_os_str())
    })
}

#[cfg(target_os = "windows")]
fn process_is_plausibly_ours(pid: u32, binary: &Path) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output();
    let Ok(output) = output else {
        return false;
    };
    let expected = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    String::from_utf8_lossy(&output.stdout)
        .to_ascii_lowercase()
        .contains(&expected.to_ascii_lowercase())
}

fn gc_after_successful_spawn(
    files: &NativeEngineFiles,
    retained: &NativeEngineKey,
) -> Result<(), NativeEngineError> {
    gc_channel_directories(files, retained)?;
    gc_cache(files)
}

fn gc_channel_directories(
    files: &NativeEngineFiles,
    retained: &NativeEngineKey,
) -> Result<(), NativeEngineError> {
    let retained_name = retained.directory_name();
    let prefix = format!("{}-", retained.channel());
    for entry in fs::read_dir(&files.base).map_err(NativeEngineError::storage)? {
        let entry = entry.map_err(NativeEngineError::storage)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix)
            && name != retained_name
            && entry
                .file_type()
                .map_err(NativeEngineError::storage)?
                .is_dir()
        {
            fs::remove_dir_all(entry.path()).map_err(NativeEngineError::storage)?;
        }
    }
    Ok(())
}

fn gc_cache(files: &NativeEngineFiles) -> Result<(), NativeEngineError> {
    let mut referenced = HashSet::new();
    if !files
        .base
        .try_exists()
        .map_err(NativeEngineError::storage)?
    {
        return Ok(());
    }
    for entry in fs::read_dir(&files.base).map_err(NativeEngineError::storage)? {
        let entry = entry.map_err(NativeEngineError::storage)?;
        if !entry
            .file_type()
            .map_err(NativeEngineError::storage)?
            .is_dir()
        {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("release-") || name.starts_with("preview-")) {
            continue;
        }
        let manifest = entry.path().join(MANIFEST_DATA_FILE);
        if let Some(manifest) = read_json_optional::<StoredManifestData>(&manifest)? {
            for file in manifest.data {
                referenced.insert(file.sha256);
            }
        }
    }
    let cache = files.cache_directory();
    if !cache.try_exists().map_err(NativeEngineError::storage)? {
        return Ok(());
    }
    for entry in fs::read_dir(cache).map_err(NativeEngineError::storage)? {
        let entry = entry.map_err(NativeEngineError::storage)?;
        let name = entry.file_name();
        if !referenced.contains(&name.to_string_lossy().to_string()) {
            remove_file_if_exists(&entry.path())?;
        }
    }
    Ok(())
}

fn read_json_optional<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, NativeEngineError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(NativeEngineError::storage),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(NativeEngineError::storage(error)),
    }
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<(), NativeEngineError> {
    let bytes = serde_json::to_vec(value).map_err(NativeEngineError::storage)?;
    write_atomically(path, &bytes)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), NativeEngineError> {
    let parent = path.parent().ok_or_else(|| NativeEngineError::Storage {
        detail: format!("{} has no parent directory", path.display()),
    })?;
    fs::create_dir_all(parent).map_err(NativeEngineError::storage)?;
    let temporary = parent.join(format!(".{}-{}.tmp", file_name(path), temporary_suffix()));
    let write_result = (|| -> Result<(), NativeEngineError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(NativeEngineError::storage)?;
        file.write_all(bytes).map_err(NativeEngineError::storage)?;
        file.sync_all().map_err(NativeEngineError::storage)?;
        // std::fs::rename replaces an existing destination on every supported
        // platform (MOVEFILE_REPLACE_EXISTING on Windows) — no pre-delete,
        // which would open a crash window with the file missing entirely.
        fs::rename(&temporary, path).map_err(NativeEngineError::storage)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn remove_file_if_exists(path: &Path) -> Result<(), NativeEngineError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(NativeEngineError::storage(error)),
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("native-engine")
        .to_owned()
}

fn temporary_suffix() -> String {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{timestamp}-{counter}", std::process::id())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), NativeEngineError> {
    let mut permissions = fs::metadata(path)
        .map_err(NativeEngineError::storage)?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(NativeEngineError::storage)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), NativeEngineError> {
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn target_triple() -> Result<&'static str, NativeEngineError> {
    Ok("aarch64-apple-darwin")
}

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn target_triple() -> Result<&'static str, NativeEngineError> {
    Ok("x86_64-pc-windows-msvc")
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn target_triple() -> Result<&'static str, NativeEngineError> {
    Ok("x86_64-unknown-linux-musl")
}

#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
fn target_triple() -> Result<&'static str, NativeEngineError> {
    Err(NativeEngineError::UnsupportedPlatform {
        detail: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    })
}

#[cfg(target_os = "windows")]
fn executable_suffix() -> &'static str {
    ".exe"
}

#[cfg(not(target_os = "windows"))]
fn executable_suffix() -> &'static str {
    ""
}

fn binary_file_name() -> String {
    format!("phase-server{}", executable_suffix())
}

fn emit_progress(app: &AppHandle, phase: NativeEngineProgressPhase, detail: Option<String>) {
    let _ = app.emit(PROGRESS_EVENT, NativeEngineProgress { phase, detail });
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use super::*;

    const TEST_PUBLIC_KEY: &str = "RWRkGDPsxuBykSbl2mdODJL2Wa/o8ow/1LHjD7Vg8ucmQEM4loTWhAyw";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRURkGDPsxuBykQ6p3ycswk/p9Fz+J1mcc/Upp6IqSVJs79jQN6+zqHp6eacgwWwzh1wzX5J7dEsr645KO34Otj6mVlBJ37dahwc=\ntrusted comment: timestamp:1784645991\tfile:fixture.bin\thashed\nAd07FDyWa2WfkYAA776JZtBLynAeiVzEfCFPDtS+KNovBOF6dS9w/YV1jerLhEGlX2oJHujsY2hPCN+hmKiwDg==";
    const TEST_BYTES: &[u8] = b"native-engine-test-fixture\n";

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "phase-tauri-native-engine-{name}-{}",
            temporary_suffix()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_files(name: &str) -> NativeEngineFiles {
        let app_directory = test_directory(name);
        NativeEngineFiles {
            base: app_directory.join(NATIVE_ENGINE_DIRECTORY),
            app_directory,
        }
    }

    fn data_file(name: &str, bytes: &[u8]) -> DataFile {
        DataFile {
            name: name.to_owned(),
            sha256: sha256_hex(bytes),
            url: "https://data.phase-rs.dev/test".to_owned(),
        }
    }

    fn release_key(version: &str) -> NativeEngineKey {
        NativeEngineKey::Release {
            version: version.to_owned(),
        }
    }

    fn preview_key(fingerprint: &str) -> NativeEngineKey {
        NativeEngineKey::Preview {
            fingerprint: fingerprint.to_owned(),
        }
    }

    #[test]
    fn key_serde_round_trips_with_snake_case_variant_tags() {
        let release = release_key("1.2.3");
        let preview = preview_key("0123456789abcdef");
        assert_eq!(
            serde_json::to_string(&release).unwrap(),
            r#"{"release":{"version":"1.2.3"}}"#
        );
        assert_eq!(
            serde_json::to_string(&preview).unwrap(),
            r#"{"preview":{"fingerprint":"0123456789abcdef"}}"#
        );
        assert_eq!(
            serde_json::from_str::<NativeEngineKey>(&serde_json::to_string(&release).unwrap())
                .unwrap(),
            release
        );
        assert_eq!(
            serde_json::from_str::<NativeEngineKey>(&serde_json::to_string(&preview).unwrap())
                .unwrap(),
            preview
        );
    }

    #[test]
    fn key_validation_rejects_invalid_semver_and_preview_fingerprint() {
        assert!(matches!(
            release_key("not-semver").validate(),
            Err(NativeEngineError::InvalidKey { .. })
        ));
        assert!(matches!(
            preview_key("0123456789abcdeg").validate(),
            Err(NativeEngineError::InvalidKey { .. })
        ));
    }

    #[test]
    fn native_engine_error_serializes_to_kind_and_detail() {
        let error = NativeEngineError::Health {
            detail: "native engine did not become healthy".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"kind":"health","detail":"native engine did not become healthy"}"#
        );
    }

    #[test]
    fn manifests_parse_with_unknown_fields_and_reject_unknown_schemas() {
        let release = br#"{"schema":1,"channel":"release","version":"1.2.3","generated_at":"2026-01-01T00:00:00Z","data":[],"future":true}"#;
        assert!(ReleaseManifest::parse(release, "1.2.3").is_ok());
        let preview = br#"{"schema":1,"channel":"preview","generated_at":"2026-01-01T00:00:00Z","current":"0123456789abcdef","previous":null,"fingerprints":{"0123456789abcdef":{"commit":"abc","binaries":{},"data":[],"future":true}},"future":true}"#;
        let preview = PreviewManifest::parse(preview).unwrap();
        assert!(preview.entry_for("0123456789abcdef").is_ok());
        assert!(preview.entry_for("fedcba9876543210").is_err());
        assert!(ReleaseManifest::parse(br#"{"schema":2,"channel":"release","version":"1.2.3","generated_at":"2026-01-01T00:00:00Z","data":[]}"#, "1.2.3").is_err());
        assert!(PreviewManifest::parse(br#"{"schema":2,"channel":"preview","generated_at":"2026-01-01T00:00:00Z","current":"0123456789abcdef","previous":null,"fingerprints":{}}"#).is_err());
    }

    #[test]
    fn preview_generated_at_ratchet_accepts_equal_or_newer_and_refuses_older() {
        let files = test_files("preview-ratchet");
        accept_preview_manifest(&files, "2026-01-01T00:00:00Z").unwrap();
        accept_preview_manifest(&files, "2026-01-01T00:00:00Z").unwrap();
        accept_preview_manifest(&files, "2026-01-02T00:00:00Z").unwrap();
        assert!(accept_preview_manifest(&files, "2026-01-01T00:00:00Z").is_err());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn minisign_signature_fixture_accepts_and_rejects_tampering() {
        verify_signature_with_key(TEST_PUBLIC_KEY, TEST_BYTES, TEST_SIGNATURE.as_bytes()).unwrap();
        assert!(
            verify_signature_with_key(TEST_PUBLIC_KEY, b"tampered", TEST_SIGNATURE.as_bytes())
                .is_err()
        );
    }

    #[test]
    fn cache_hash_atomic_write_and_data_assembly_work() {
        let files = test_files("cache-assembly");
        let key = release_key("1.2.3");
        let bytes = b"card data";
        let file = data_file("card-data.json", bytes);
        verify_sha256(bytes, &file.sha256).unwrap();
        write_atomically(&files.cache_blob(&file.sha256), bytes).unwrap();
        assert_eq!(fs::read(files.cache_blob(&file.sha256)).unwrap(), bytes);
        let client = http_client().unwrap();
        assemble_data(&client, None, &files, &key, std::slice::from_ref(&file)).unwrap();
        let destination = files.data_directory(&key).join(&file.name);
        assert_eq!(fs::read(destination).unwrap(), bytes);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn hard_link_failure_falls_back_to_copy() {
        let directory = test_directory("copy-fallback");
        let source = directory.join("source");
        let destination = directory.join("destination");
        fs::write(&source, b"cache").unwrap();
        link_or_copy_with(&source, &destination, |_, _| {
            Err(io::Error::other("cross-device link"))
        })
        .unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"cache");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn manifest_diff_cache_gc_and_different_key_directory_gc() {
        let files = test_files("gc");
        let current = release_key("2.0.0");
        let old_release = release_key("1.0.0");
        let preview = preview_key("0123456789abcdef");
        let current_data = data_file("card-data.json", b"current");
        let preview_data = data_file("draft-pools.json", b"preview");
        let stale_hash = sha256_hex(b"stale");
        for hash in [&current_data.sha256, &preview_data.sha256, &stale_hash] {
            write_atomically(&files.cache_blob(hash), hash.as_bytes()).unwrap();
        }
        for (key, data) in [
            (&current, vec![current_data.clone()]),
            (&old_release, vec![preview_data.clone()]),
            (&preview, vec![preview_data.clone()]),
        ] {
            fs::create_dir_all(files.key_directory(key)).unwrap();
            write_json_atomically(&files.manifest_data(key), &StoredManifestData { data }).unwrap();
        }
        gc_after_successful_spawn(&files, &current).unwrap();
        assert!(files.key_directory(&current).exists());
        assert!(!files.key_directory(&old_release).exists());
        assert!(files.key_directory(&preview).exists());
        assert!(files.cache_blob(&current_data.sha256).exists());
        assert!(files.cache_blob(&preview_data.sha256).exists());
        assert!(!files.cache_blob(&stale_hash).exists());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn release_ratchet_and_spawn_record_adoption_are_key_exact() {
        let files = test_files("ratchet-record");
        let newer = release_key("2.0.0");
        persist_release_ratchet(&files, &newer).unwrap();
        assert!(check_release_ratchet(&files, &release_key("1.0.0")).is_err());
        assert!(check_release_ratchet(&files, &release_key("2.0.0")).is_ok());
        let record = SpawnRecord {
            pid: 123,
            port: 456,
            key: newer.clone(),
        };
        write_spawn_record(&files, &record).unwrap();
        assert_eq!(read_spawn_record(&files).unwrap().unwrap().pid, 123);
        assert!(can_adopt(&record, &newer, true));
        assert!(!can_adopt(&record, &release_key("2.0.1"), true));
        assert!(!can_adopt(&record, &newer, false));
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn health_timeout_constant_is_short_and_bounded() {
        assert!(HEALTH_TIMEOUT >= Duration::from_secs(10));
        assert!(HEALTH_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn stop_without_running_engine_sweeps_bridge_registry() {
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        let (outbound, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = NativeEngineState::default();
        state.bridges.insert(1, BridgeHandle::new(abort, outbound));

        abort_all_native_engine_bridges(&mut state.bridges);

        assert!(state.running.is_none());
        assert!(state.bridges.is_empty());
        let result = tauri::async_runtime::block_on(async {
            futures_util::future::Abortable::new(std::future::pending::<()>(), registration).await
        });
        assert!(result.is_err());
    }
}
