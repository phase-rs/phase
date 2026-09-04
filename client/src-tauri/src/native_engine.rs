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
use crate::native_engine_contract::{
    NativeEngineCapabilities, NativeEngineError, NativeEngineIntent, NativeEngineKey,
    NativeEngineProgress, NativeEngineProgressPhase, NativeEngineReady,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` — suppress the console window Windows allocates for a
/// console-subsystem child when spawned from this GUI-subsystem shell. Without
/// it, launching the native `phase-server.exe` flashes/leaves a cmd window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SERVER_ARTIFACT_PUBLIC_KEY: &str = "RWRDZxG2otNoKLblrgD00kM0a8U0CRZUGHpNCr3W+3ik1E84XHcB6hZe";
const NATIVE_ENGINE_DIRECTORY: &str = "native-engine";
const CACHE_DIRECTORY: &str = "cache/sha256";
const LOG_DIRECTORY: &str = "logs";
const SPAWN_RECORD_FILE: &str = "native-engine-spawn-record.json";
const RELEASE_RATCHET_FILE: &str = "native-engine-highest-release-version.json";
const PREVIEW_RATCHET_FILE: &str = "native-engine-preview-generated-at.json";
const MANIFEST_DATA_FILE: &str = "manifest-data.json";
const SIGNED_MANIFEST_ENVELOPE_FILE: &str = "signed-manifest-envelope.json";
const RELEASE_ORIGIN: &str = "https://phase-rs.dev";
const PREVIEW_ORIGIN: &str = "https://preview.phase-rs.dev";
const PROGRESS_EVENT: &str = "native-engine-progress";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(20);
const STOP_GRACE: Duration = Duration::from_millis(250);

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

#[derive(Deserialize, Serialize)]
struct StoredSignedManifestEnvelope {
    manifest: Vec<u8>,
    signature: Vec<u8>,
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

    fn binary_signature(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key)
            .join(format!("{}.minisig", binary_file_name()))
    }

    fn data_directory(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key).join("data")
    }

    /// Version-independent path for the server's game-persistence database.
    /// Keyed by channel only (not the version/fingerprint that names
    /// `key_directory`), so saved games survive engine updates within a channel
    /// while `preview` and `release` stay isolated — they load different
    /// content and must not share sessions.
    fn games_db(&self, key: &NativeEngineKey) -> PathBuf {
        self.base
            .join("games")
            .join(format!("{}.db", key.channel()))
    }

    fn cache_directory(&self) -> PathBuf {
        self.base.join(CACHE_DIRECTORY)
    }

    fn cache_blob(&self, sha256: &str) -> PathBuf {
        self.cache_directory().join(sha256)
    }

    fn log_directory(&self) -> PathBuf {
        self.base.join(LOG_DIRECTORY)
    }

    fn startup_log(&self) -> PathBuf {
        self.log_directory().join("server-startup.log")
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

    fn signed_manifest_envelope(&self, key: &NativeEngineKey) -> PathBuf {
        self.key_directory(key).join(SIGNED_MANIFEST_ENVELOPE_FILE)
    }
}

struct ResolvedArtifact {
    binary_url: String,
    binary_signature_url: String,
    data: Vec<DataFile>,
    fetched_envelope: Option<StoredSignedManifestEnvelope>,
}

struct PreparedSpawnPlan {
    binary: PathBuf,
    data_directory: PathBuf,
    arguments: Vec<String>,
    preflight: ArtifactPreflight,
}

struct DataPreflight {
    entry: DataFile,
    cache_is_valid: bool,
    destination_is_valid: bool,
}

struct ArtifactPreflight {
    binary_is_valid: bool,
    data: Vec<DataPreflight>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleDecision {
    RetainAndVerify,
    ReturnReady,
    RefuseWithoutSideEffects,
    Replace,
    CleanStale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactResolutionOrder {
    BeforePersistedLifecycle,
    AfterPersistedLifecycle,
}

enum PersistedRecordOutcome {
    Continue,
    Retain(SpawnRecord),
    ReturnReady(NativeEngineReady),
}

fn adopt_persisted_record(state: &mut NativeEngineState, record: SpawnRecord) -> NativeEngineReady {
    let ready = NativeEngineReady { port: record.port };
    state.running = Some(RunningEngine::Adopted(record));
    ready
}

fn artifact_resolution_order(
    key: &NativeEngineKey,
    intent: NativeEngineIntent,
) -> ArtifactResolutionOrder {
    match (key, intent) {
        (NativeEngineKey::Preview { .. }, NativeEngineIntent::StartOnline)
        | (NativeEngineKey::Preview { .. }, NativeEngineIntent::PrepareForOffline) => {
            ArtifactResolutionOrder::BeforePersistedLifecycle
        }
        (NativeEngineKey::Release { .. }, _)
        | (NativeEngineKey::Preview { .. }, NativeEngineIntent::StartOffline) => {
            ArtifactResolutionOrder::AfterPersistedLifecycle
        }
    }
}

fn lifecycle_decision(
    intent: NativeEngineIntent,
    requested: &NativeEngineKey,
    current: &NativeEngineKey,
    healthy: bool,
) -> LifecycleDecision {
    if !healthy {
        return LifecycleDecision::CleanStale;
    }
    if current == requested {
        return match intent {
            NativeEngineIntent::PrepareForOffline => LifecycleDecision::RetainAndVerify,
            NativeEngineIntent::StartOnline | NativeEngineIntent::StartOffline => {
                LifecycleDecision::ReturnReady
            }
        };
    }
    if intent == NativeEngineIntent::PrepareForOffline {
        LifecycleDecision::RefuseWithoutSideEffects
    } else {
        LifecycleDecision::Replace
    }
}

fn preparation_conflicts(
    requested: &NativeEngineKey,
    held: Option<(&NativeEngineKey, bool)>,
    record: Option<(&NativeEngineKey, bool)>,
) -> bool {
    held.is_some_and(|(current, healthy)| healthy && current != requested)
        || record.is_some_and(|(current, healthy)| healthy && current != requested)
}

fn require_preparation_preflight(
    requested: &NativeEngineKey,
    held: Option<(&NativeEngineKey, bool)>,
    record: Option<(&NativeEngineKey, bool)>,
) -> Result<(), NativeEngineError> {
    if preparation_conflicts(requested, held, record) {
        return Err(NativeEngineError::Health {
            detail: "a healthy native engine for a different key is already running".to_owned(),
        });
    }
    Ok(())
}

fn after_preparation_preflight<T>(
    intent: NativeEngineIntent,
    requested: &NativeEngineKey,
    held: Option<(&NativeEngineKey, bool)>,
    record: Option<(&NativeEngineKey, bool)>,
    continue_with: impl FnOnce() -> Result<T, NativeEngineError>,
) -> Result<T, NativeEngineError> {
    if intent == NativeEngineIntent::PrepareForOffline {
        require_preparation_preflight(requested, held, record)?;
    }
    continue_with()
}

fn apply_persisted_record_lifecycle(
    state: &mut NativeEngineState,
    files: &NativeEngineFiles,
    requested: &NativeEngineKey,
    intent: NativeEngineIntent,
    record: Option<(SpawnRecord, bool)>,
    resolved_before_record: Option<&ResolvedArtifact>,
) -> Result<PersistedRecordOutcome, NativeEngineError> {
    let Some((record, healthy)) = record else {
        return Ok(PersistedRecordOutcome::Continue);
    };
    match lifecycle_decision(intent, requested, &record.key, healthy) {
        LifecycleDecision::ReturnReady => {
            if let Some(resolved) = resolved_before_record {
                persist_fetched_envelope(files, requested, resolved)?;
            }
            Ok(PersistedRecordOutcome::ReturnReady(adopt_persisted_record(
                state, record,
            )))
        }
        LifecycleDecision::RetainAndVerify => Ok(PersistedRecordOutcome::Retain(record)),
        LifecycleDecision::RefuseWithoutSideEffects => Err(NativeEngineError::Health {
            detail: "a healthy native engine for a different key is already running".to_owned(),
        }),
        LifecycleDecision::Replace | LifecycleDecision::CleanStale => {
            kill_recorded_process_if_ours(&record, files);
            clear_spawn_record(files)?;
            Ok(PersistedRecordOutcome::Continue)
        }
    }
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
static LATEST_PROGRESS: OnceLock<Mutex<Option<NativeEngineProgress>>> = OnceLock::new();
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) enum NativeBridgeRegistryError {
    NotRunning,
    Internal(String),
}

fn engine_state() -> &'static Mutex<NativeEngineState> {
    ENGINE_STATE.get_or_init(|| Mutex::new(NativeEngineState::default()))
}

fn latest_progress() -> &'static Mutex<Option<NativeEngineProgress>> {
    LATEST_PROGRESS.get_or_init(|| Mutex::new(None))
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
    intent: Option<NativeEngineIntent>,
) -> Result<NativeEngineReady, NativeEngineError> {
    let progress_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        ensure_native_engine_sync(&app, key, intent.unwrap_or(NativeEngineIntent::StartOnline))
    })
    .await
    .map_err(|error| NativeEngineError::Internal {
        detail: error.to_string(),
    })
    .and_then(|result| result);
    // Single authority for the terminal phase. `ensure_native_engine_sync`
    // returns early on both the healthy-in-process and adopted-record paths,
    // so emitting `Ready` inside it would leave those runs ending on a
    // non-terminal phase — and a listener waiting for one would wait forever.
    match &result {
        Ok(ready) => emit_progress(
            &progress_app,
            NativeEngineProgressPhase::Ready,
            Some(ready.port.to_string()),
        ),
        Err(_) => emit_progress(&progress_app, NativeEngineProgressPhase::Failed, None),
    }
    result
}

/// Read-only version-skew boundary for remote web content.
#[tauri::command]
pub fn native_engine_capabilities() -> NativeEngineCapabilities {
    NativeEngineCapabilities { intent_contract: 1 }
}

/// Returns the latest provisioning progress for listeners that register late.
#[tauri::command]
pub fn native_engine_progress() -> Option<NativeEngineProgress> {
    latest_progress().lock().ok()?.clone()
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
    intent: NativeEngineIntent,
) -> Result<NativeEngineReady, NativeEngineError> {
    key.validate()?;
    let files = NativeEngineFiles::from_app(app)?;
    let client = http_client()?;
    let fetch = |url: &str| fetch_bytes(&client, url);
    let mut state = engine_state()
        .lock()
        .map_err(|error| NativeEngineError::Internal {
            detail: format!("native engine state lock poisoned: {error}"),
        })?;

    check_release_ratchet(&files, &key)?;

    // Preparation must protect every potentially live owner before it mutates
    // disk or lifecycle state. A persisted record can outlive this process, so
    // checking only `state.running` would still permit a remote manifest write
    // or stale cleanup to disturb a healthy different-key engine.
    let held_snapshot = state.running.as_ref().map(|running| {
        (
            running.key().clone(),
            health_passes(&client, running.port()),
        )
    });
    let persisted_record = read_spawn_record(&files)?;
    // This is deliberately after the preparation conflict preflight: refusing
    // to prepare must leave even an absent cache directory untouched.
    after_preparation_preflight(
        intent,
        &key,
        held_snapshot
            .as_ref()
            .map(|(held_key, healthy)| (held_key, *healthy)),
        persisted_record
            .as_ref()
            .map(|record| (&record.key, health_passes(&client, record.port))),
        || {
            if intent != NativeEngineIntent::PrepareForOffline {
                fs::create_dir_all(&files.base).map_err(NativeEngineError::storage)
            } else {
                Ok(())
            }
        },
    )?;

    let held_is_healthy_for_key = if let Some(running) = state.running.as_mut() {
        match lifecycle_decision(
            intent,
            &key,
            running.key(),
            health_passes(&client, running.port()),
        ) {
            LifecycleDecision::ReturnReady => {
                return Ok(NativeEngineReady {
                    port: running.port(),
                });
            }
            LifecycleDecision::RetainAndVerify => true,
            LifecycleDecision::RefuseWithoutSideEffects => {
                return Err(NativeEngineError::Health {
                    detail: "a healthy native engine for a different key is already running"
                        .to_owned(),
                });
            }
            LifecycleDecision::Replace | LifecycleDecision::CleanStale => false,
        }
    } else {
        false
    };

    if !held_is_healthy_for_key {
        if let Some(running) = state.running.take() {
            stop_running_engine(running, &files, &mut state.bridges);
            clear_spawn_record(&files)?;
        }
    }

    emit_progress(
        app,
        NativeEngineProgressPhase::Resolving,
        Some(key.directory_name()),
    );
    let preview_resolved = match artifact_resolution_order(&key, intent) {
        ArtifactResolutionOrder::BeforePersistedLifecycle => {
            Some(resolve_artifact(app, &fetch, &files, &key, intent)?)
        }
        ArtifactResolutionOrder::AfterPersistedLifecycle => None,
    };

    let persisted_outcome = apply_persisted_record_lifecycle(
        &mut state,
        &files,
        &key,
        intent,
        persisted_record.map(|record| {
            let healthy = health_passes(&client, record.port);
            (record, healthy)
        }),
        preview_resolved.as_ref(),
    )?;
    let retained_record = match persisted_outcome {
        PersistedRecordOutcome::Continue => None,
        PersistedRecordOutcome::Retain(record) => Some(record),
        PersistedRecordOutcome::ReturnReady(ready) => return Ok(ready),
    };

    let resolved = match preview_resolved {
        Some(resolved) => resolved,
        None => resolve_artifact(app, &fetch, &files, &key, intent)?,
    };
    let repair_allowed = !held_is_healthy_for_key && retained_record.is_none();

    let spawn_plan = provision_resolved_artifact(
        Some(app),
        &fetch,
        &files,
        &key,
        &resolved,
        intent,
        repair_allowed,
    )?;

    if held_is_healthy_for_key {
        let port = state.running.as_ref().expect("checked above").port();
        if health_passes(&client, port) {
            persist_release_ratchet(&files, &key)?;
            return Ok(NativeEngineReady { port });
        }
        if let Some(running) = state.running.take() {
            stop_running_engine(running, &files, &mut state.bridges);
            clear_spawn_record(&files)?;
        }
    }
    if !held_is_healthy_for_key {
        if let Some(record) = retained_record {
            if health_passes(&client, record.port) {
                persist_release_ratchet(&files, &key)?;
                return Ok(adopt_persisted_record(&mut state, record));
            }
            kill_recorded_process_if_ours(&record, &files);
            clear_spawn_record(&files)?;
            abort_all_native_engine_bridges(&mut state.bridges);
        }
    }

    emit_progress(app, NativeEngineProgressPhase::Spawning, None);
    let port = reserve_port()?;
    let (mut child, stdin) = spawn_server(
        &spawn_plan.binary,
        &spawn_plan.data_directory,
        &files.games_db(&key),
        &files.log_directory(),
        &files.startup_log(),
        port,
        &spawn_plan.arguments,
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

fn resolve_artifact<F>(
    app: &AppHandle,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    intent: NativeEngineIntent,
) -> Result<ResolvedArtifact, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    if intent == NativeEngineIntent::StartOffline {
        return resolve_cached_artifact(files, key);
    }

    emit_progress(
        app,
        NativeEngineProgressPhase::Verifying,
        Some(
            match key {
                NativeEngineKey::Release { .. } => "release data manifest",
                NativeEngineKey::Preview { .. } => "preview server manifest",
            }
            .to_owned(),
        ),
    );
    resolve_online_artifact(fetch, files, key)
}

fn resolve_online_artifact<F>(
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<ResolvedArtifact, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    resolve_online_artifact_with_key(SERVER_ARTIFACT_PUBLIC_KEY, fetch, files, key)
}

fn resolve_online_artifact_with_key<F>(
    public_key: &str,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<ResolvedArtifact, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    let manifest_url = match key {
        NativeEngineKey::Release { version } => {
            format!("https://data.phase-rs.dev/desktop/release-server-v{version}.json")
        }
        NativeEngineKey::Preview { .. } => {
            "https://data.phase-rs.dev/desktop/preview-server.json".to_owned()
        }
    };
    let envelope = fetch_signed_manifest_with_key(public_key, fetch, &manifest_url)?;
    let mut resolved = resolved_artifact_from_envelope_with_key(public_key, files, key, &envelope)?;
    resolved.fetched_envelope = Some(envelope);
    Ok(resolved)
}

fn persist_fetched_envelope(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
) -> Result<(), NativeEngineError> {
    let Some(envelope) = &resolved.fetched_envelope else {
        return Ok(());
    };
    write_json_atomically(&files.signed_manifest_envelope(key), envelope)?;
    if let NativeEngineKey::Preview { .. } = key {
        persist_preview_ratchet(files, &preview_generated_at(&envelope.manifest)?)?;
    }
    Ok(())
}

fn resolve_cached_artifact(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<ResolvedArtifact, NativeEngineError> {
    resolve_cached_artifact_with_key(SERVER_ARTIFACT_PUBLIC_KEY, files, key)
}

fn resolve_cached_artifact_with_key(
    public_key: &str,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<ResolvedArtifact, NativeEngineError> {
    let envelope = read_signed_manifest_envelope(files, key)?;
    let resolved = resolved_artifact_from_envelope_with_key(public_key, files, key, &envelope)?;
    if let NativeEngineKey::Preview { .. } = key {
        persist_preview_ratchet(files, &preview_generated_at(&envelope.manifest)?)?;
    }
    Ok(resolved)
}

fn read_signed_manifest_envelope(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<StoredSignedManifestEnvelope, NativeEngineError> {
    let path = files.signed_manifest_envelope(key);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(NativeEngineError::Manifest {
                detail: "the exact native-engine signed manifest is not cached".to_owned(),
            });
        }
        Err(error) => return Err(NativeEngineError::storage(error)),
    };
    serde_json::from_slice(&bytes).map_err(|error| NativeEngineError::Verification {
        detail: format!("invalid cached signed manifest envelope: {error}"),
    })
}

fn fetch_signed_manifest_with_key<F>(
    public_key: &str,
    fetch: &F,
    manifest_url: &str,
) -> Result<StoredSignedManifestEnvelope, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    let envelope = StoredSignedManifestEnvelope {
        manifest: fetch(manifest_url)?,
        signature: fetch(&format!("{manifest_url}.minisig"))?,
    };
    verify_signature_with_key(public_key, &envelope.manifest, &envelope.signature)?;
    Ok(envelope)
}

fn preview_generated_at(manifest: &[u8]) -> Result<String, NativeEngineError> {
    Ok(PreviewManifest::parse(manifest)?.generated_at)
}

fn resolved_artifact_from_envelope_with_key(
    public_key: &str,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    envelope: &StoredSignedManifestEnvelope,
) -> Result<ResolvedArtifact, NativeEngineError> {
    verify_signature_with_key(public_key, &envelope.manifest, &envelope.signature)?;
    match key {
        NativeEngineKey::Release { version } => {
            let asset = format!(
                "phase-server-slim-{}{}",
                target_triple()?,
                executable_suffix()
            );
            let base =
                format!("https://github.com/phase-rs/phase/releases/download/v{version}/{asset}");
            let manifest = ReleaseManifest::parse(&envelope.manifest, version)?;
            Ok(ResolvedArtifact {
                binary_url: base.clone(),
                binary_signature_url: format!("{base}.minisig"),
                data: manifest.data,
                fetched_envelope: None,
            })
        }
        NativeEngineKey::Preview { fingerprint } => {
            let manifest = PreviewManifest::parse(&envelope.manifest)?;
            let entry = manifest.entry_for(fingerprint)?;
            check_preview_ratchet(files, &manifest.generated_at)?;
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
                fetched_envelope: None,
            })
        }
    }
}

/// Reuses a previously verified server binary for the same typed key. The
/// minisign signature is retained alongside the executable so every launch
/// still verifies what it is about to execute; a missing or invalid cache is
/// simply replaced from the first-party artifact source.
fn provision_binary_with_key<F>(
    public_key: &str,
    app: Option<&AppHandle>,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
    intent: NativeEngineIntent,
    repair_allowed: bool,
    binary_is_valid: bool,
) -> Result<PathBuf, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    let binary_path = files.binary(key);
    if binary_is_valid {
        return Ok(binary_path);
    }
    if intent == NativeEngineIntent::StartOffline {
        return Err(NativeEngineError::Verification {
            detail: "the exact native-engine binary is missing or invalid offline".to_owned(),
        });
    }
    if !repair_allowed {
        return Err(NativeEngineError::Verification {
            detail: "native-engine binary repair would replace a file used by the healthy engine"
                .to_owned(),
        });
    }

    if let Some(app) = app {
        emit_progress(
            app,
            NativeEngineProgressPhase::DownloadingBinary,
            Some(key.directory_name()),
        );
    }
    let binary = fetch(&resolved.binary_url)?;
    let signature = fetch(&resolved.binary_signature_url)?;
    if let Some(app) = app {
        emit_progress(
            app,
            NativeEngineProgressPhase::Verifying,
            Some("server binary".to_owned()),
        );
    }
    verify_signature_with_key(public_key, &binary, &signature)?;
    write_atomically(&binary_path, &binary)?;
    write_atomically(&files.binary_signature(key), &signature)?;
    make_executable(&binary_path)?;
    Ok(binary_path)
}

fn cached_binary_is_verified_with_key(
    public_key: &str,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
) -> Result<bool, NativeEngineError> {
    let binary = match fs::read(files.binary(key)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(NativeEngineError::storage(error)),
    };
    let signature = match fs::read(files.binary_signature(key)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(NativeEngineError::storage(error)),
    };
    Ok(verify_signature_with_key(public_key, &binary, &signature).is_ok())
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

fn check_preview_ratchet(
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
    Ok(())
}

fn persist_preview_ratchet(
    files: &NativeEngineFiles,
    generated_at: &str,
) -> Result<(), NativeEngineError> {
    parse_generated_at(generated_at)?;
    write_json_atomically(
        &files.preview_ratchet(),
        &PreviewRatchet {
            generated_at: generated_at.to_owned(),
        },
    )
}

#[cfg(test)]
fn accept_preview_manifest(
    files: &NativeEngineFiles,
    generated_at: &str,
) -> Result<(), NativeEngineError> {
    check_preview_ratchet(files, generated_at)?;
    persist_preview_ratchet(files, generated_at)
}

fn parse_generated_at(value: &str) -> Result<OffsetDateTime, NativeEngineError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(NativeEngineError::manifest)
}

fn preflight_artifacts_with_key(
    public_key: &str,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
) -> Result<ArtifactPreflight, NativeEngineError> {
    Ok(ArtifactPreflight {
        binary_is_valid: cached_binary_is_verified_with_key(public_key, files, key)?,
        data: preflight_data(files, key, &resolved.data)?,
    })
}

fn preflight_data(
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    entries: &[DataFile],
) -> Result<Vec<DataPreflight>, NativeEngineError> {
    validate_data_files(entries)?;
    entries
        .iter()
        .map(|entry| {
            Ok(DataPreflight {
                entry: entry.clone(),
                cache_is_valid: file_matches_sha256(
                    &files.cache_blob(&entry.sha256),
                    &entry.sha256,
                )?,
                destination_is_valid: file_matches_sha256(
                    &files.data_directory(key).join(&entry.name),
                    &entry.sha256,
                )?,
            })
        })
        .collect()
}

fn require_retained_destinations(preflight: &ArtifactPreflight) -> Result<(), NativeEngineError> {
    if !preflight.binary_is_valid {
        return Err(NativeEngineError::Verification {
            detail: "native-engine binary repair would replace a file used by the healthy engine"
                .to_owned(),
        });
    }
    if let Some(data) = preflight
        .data
        .iter()
        .find(|data| !data.destination_is_valid)
    {
        return Err(NativeEngineError::Verification {
            detail: format!(
                "native-engine data destination {} would be replaced while healthy",
                data.entry.name
            ),
        });
    }
    Ok(())
}

fn assemble_data<F>(
    fetch: &F,
    app: Option<&AppHandle>,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    data: &[DataPreflight],
    intent: NativeEngineIntent,
    repair_allowed: bool,
) -> Result<(), NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    if intent == NativeEngineIntent::StartOffline {
        if let Some(data) = data.iter().find(|data| !data.cache_is_valid) {
            return Err(NativeEngineError::Verification {
                detail: format!(
                    "cached native-engine data {} is missing or invalid",
                    data.entry.name
                ),
            });
        }
    }
    let data_directory = files.data_directory(key);
    fs::create_dir_all(&data_directory).map_err(NativeEngineError::storage)?;
    ensure_writable(&data_directory)?;
    for data in data {
        let entry = &data.entry;
        let cache_blob = files.cache_blob(&entry.sha256);
        let destination = data_directory.join(&entry.name);
        if !data.cache_is_valid {
            if let Some(app) = app {
                emit_progress(
                    app,
                    NativeEngineProgressPhase::DownloadingData,
                    Some(entry.name.clone()),
                );
            }
            let bytes = fetch(&entry.url)?;
            verify_sha256(&bytes, &entry.sha256)?;
            write_atomically(&cache_blob, &bytes)?;
        }
        if !data.destination_is_valid {
            if !repair_allowed {
                return Err(NativeEngineError::Verification {
                    detail: format!(
                        "native-engine data destination {} would be replaced while healthy",
                        entry.name
                    ),
                });
            }
            remove_file_if_exists(&destination)?;
            link_or_copy(&cache_blob, &destination)?;
        }
        verify_sha256(
            &fs::read(&destination).map_err(NativeEngineError::storage)?,
            &entry.sha256,
        )?;
    }
    write_json_atomically(
        &files.manifest_data(key),
        &StoredManifestData {
            data: data.iter().map(|data| data.entry.clone()).collect(),
        },
    )
}

fn file_matches_sha256(path: &Path, expected: &str) -> Result<bool, NativeEngineError> {
    match fs::read(path) {
        Ok(bytes) => Ok(verify_sha256(&bytes, expected).is_ok()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(NativeEngineError::storage(error)),
    }
}

fn plan_spawn_with_key(
    public_key: &str,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
    intent: NativeEngineIntent,
    repair_allowed: bool,
) -> Result<PreparedSpawnPlan, NativeEngineError> {
    let preflight = preflight_artifacts_with_key(public_key, files, key, resolved)?;
    if !repair_allowed {
        require_retained_destinations(&preflight)?;
    }
    Ok(PreparedSpawnPlan {
        binary: files.binary(key),
        data_directory: files.data_directory(key),
        arguments: server_arguments(key.origin(), intent),
        preflight,
    })
}

fn apply_spawn_plan_with_key<F>(
    public_key: &str,
    app: Option<&AppHandle>,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
    intent: NativeEngineIntent,
    repair_allowed: bool,
    plan: &PreparedSpawnPlan,
) -> Result<(), NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    let binary = provision_binary_with_key(
        public_key,
        app,
        fetch,
        files,
        key,
        resolved,
        intent,
        repair_allowed,
        plan.preflight.binary_is_valid,
    )?;
    debug_assert_eq!(binary, plan.binary);
    if let Some(app) = app {
        emit_progress(app, NativeEngineProgressPhase::DownloadingData, None);
    }
    assemble_data(
        fetch,
        app,
        files,
        key,
        &plan.preflight.data,
        intent,
        repair_allowed,
    )?;
    Ok(())
}

fn provision_resolved_artifact<F>(
    app: Option<&AppHandle>,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
    intent: NativeEngineIntent,
    repair_allowed: bool,
) -> Result<PreparedSpawnPlan, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    provision_resolved_artifact_with_key(
        SERVER_ARTIFACT_PUBLIC_KEY,
        app,
        fetch,
        files,
        key,
        resolved,
        intent,
        repair_allowed,
    )
}

fn provision_resolved_artifact_with_key<F>(
    public_key: &str,
    app: Option<&AppHandle>,
    fetch: &F,
    files: &NativeEngineFiles,
    key: &NativeEngineKey,
    resolved: &ResolvedArtifact,
    intent: NativeEngineIntent,
    repair_allowed: bool,
) -> Result<PreparedSpawnPlan, NativeEngineError>
where
    F: Fn(&str) -> Result<Vec<u8>, NativeEngineError>,
{
    let plan = plan_spawn_with_key(public_key, files, key, resolved, intent, repair_allowed)?;
    persist_fetched_envelope(files, key, resolved)?;
    apply_spawn_plan_with_key(
        public_key,
        app,
        fetch,
        files,
        key,
        resolved,
        intent,
        repair_allowed,
        &plan,
    )?;
    Ok(plan)
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
    games_db: &Path,
    log_directory: &Path,
    startup_log: &Path,
    port: u16,
    arguments: &[String],
) -> Result<(Child, ChildStdin), NativeEngineError> {
    fs::create_dir_all(log_directory).map_err(NativeEngineError::storage)?;
    let startup_log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(startup_log)
        .map_err(NativeEngineError::storage)?;
    let mut command = Command::new(binary);
    command
        .env("PORT", port.to_string())
        .env("PHASE_DATA_DIR", data_directory)
        .env("PHASE_GAMES_DB", games_db)
        .args(arguments)
        .arg("--log-dir")
        .arg(log_directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        // Early startup failures happen before phase-server initializes its
        // structured logger. Keep the latest one beside its rolling logs so a
        // server that exits before `/health` is diagnosable without a console.
        .stderr(Stdio::from(startup_log));

    // The shell is a GUI-subsystem app but phase-server is console-subsystem, so
    // Windows would otherwise pop a console window for the child on every launch.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| NativeEngineError::Spawn {
        detail: error.to_string(),
    })?;
    let stdin = child.stdin.take().ok_or_else(|| NativeEngineError::Spawn {
        detail: "native engine stdin pipe was unavailable".to_owned(),
    })?;
    Ok((child, stdin))
}

fn server_arguments(origin: &str, intent: NativeEngineIntent) -> Vec<String> {
    let mut arguments = vec![
        "--bind".to_owned(),
        "127.0.0.1".to_owned(),
        "--exit-on-stdin-close".to_owned(),
        // A desktop shell hosts one local player: keep the suspended solo
        // game resumable until replaced (no stale purge, no reconnect expiry).
        "--single-user".to_owned(),
        "--allowed-origin".to_owned(),
        origin.to_owned(),
    ];
    if intent != NativeEngineIntent::StartOnline {
        arguments.push("--no-data-download".to_owned());
    }
    arguments
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

#[cfg(test)]
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
            .creation_flags(CREATE_NO_WINDOW)
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
        .creation_flags(CREATE_NO_WINDOW)
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
    let progress = NativeEngineProgress { phase, detail };
    if let Ok(mut latest) = latest_progress().lock() {
        *latest = Some(progress.clone());
    }
    let _ = app.emit(PROGRESS_EVENT, progress);
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, fs, time::Duration};

    use super::*;

    const TEST_PUBLIC_KEY: &str = "RWRkGDPsxuBykSbl2mdODJL2Wa/o8ow/1LHjD7Vg8ucmQEM4loTWhAyw";
    const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRURkGDPsxuBykQ6p3ycswk/p9Fz+J1mcc/Upp6IqSVJs79jQN6+zqHp6eacgwWwzh1wzX5J7dEsr645KO34Otj6mVlBJ37dahwc=\ntrusted comment: timestamp:1784645991\tfile:fixture.bin\thashed\nAd07FDyWa2WfkYAA776JZtBLynAeiVzEfCFPDtS+KNovBOF6dS9w/YV1jerLhEGlX2oJHujsY2hPCN+hmKiwDg==";
    const TEST_BYTES: &[u8] = b"native-engine-test-fixture\n";
    // Generated locally for this test fixture with `minisign -G -W` and only
    // this public key/signature are retained; the temporary private key was
    // never added to the repository.
    const TEST_MANIFEST_PUBLIC_KEY: &str =
        "RWShXyki5XOg0I93KFq/y1ZmJM80FRzQ2yw7POGQ9KSjxscp/2FDTqNU";
    const TEST_RELEASE_MANIFEST: &[u8] = br#"{"schema":1,"channel":"release","version":"1.2.3","generated_at":"2026-01-01T00:00:00Z","data":[]}"#;
    const TEST_RELEASE_MANIFEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUShXyki5XOg0GM/CqvIehBL/PgNuvRzKsR+fjxvdYZq3TWNW5QrsDlAsSCra8g3dGsB5V2Kf6QwUO9jjYbCwznNEpfqNJkHAwE=\ntrusted comment: timestamp:1788355523\tfile:release.json\thashed\nxYDP6Cn8xpjf4DJ3dwQ5UUXEAlRK15QJyis1l2/TFXc4kxRRgmxJwIAJ1nwuk4zM6nrob0dsIEJIRv5l265OBw==";
    const TEST_PREVIEW_MANIFEST: &[u8] = br#"{"schema":1,"channel":"preview","generated_at":"2026-01-02T00:00:00Z","current":"0123456789abcdef","previous":null,"fingerprints":{"0123456789abcdef":{"commit":"abc","binaries":{"aarch64-apple-darwin":{"url":"https://example.test/macos","sig_url":"https://example.test/macos.minisig"},"x86_64-pc-windows-msvc":{"url":"https://example.test/windows","sig_url":"https://example.test/windows.minisig"},"x86_64-unknown-linux-musl":{"url":"https://example.test/linux","sig_url":"https://example.test/linux.minisig"}},"data":[]}}}"#;
    const TEST_PREVIEW_MANIFEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUShXyki5XOg0Hztqsw1GFwxMgrX5o0/vRLNsbcGz32R1gVODVfUg+ZR4L/PreI9Nsu8u+BGPoGHYw5CNXQlpWHn6ndKe/vFVwM=\ntrusted comment: timestamp:1788355523\tfile:preview.json\thashed\nQwTR8roTR23UbV+hOm3MZMChfMFtZzbZHFH3fPLoPSp6y0HH2zxx7Jqo2/51r+4oeKjzyjptqWFOXk+1mdRaDQ==";

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
    fn signed_release_envelope_is_exact_key_scoped_and_tamper_evident() {
        let files = test_files("signed-release-envelope");
        let envelope = StoredSignedManifestEnvelope {
            manifest: TEST_RELEASE_MANIFEST.to_vec(),
            signature: TEST_RELEASE_MANIFEST_SIGNATURE.as_bytes().to_vec(),
        };
        let key = release_key("1.2.3");
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &envelope,
        )
        .is_ok());
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &release_key("1.2.4"),
            &envelope,
        )
        .is_err());
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &preview_key("0123456789abcdef"),
            &envelope,
        )
        .is_err());
        let mut tampered = envelope;
        tampered.manifest.push(b'!');
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &tampered,
        )
        .is_err());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn signed_manifest_fetch_uses_manifest_then_signature_and_rejects_tampering() {
        let requested = RefCell::new(Vec::new());
        let fetch = |url: &str| {
            requested.borrow_mut().push(url.to_owned());
            match url {
                "https://example.test/release.json" => Ok(TEST_RELEASE_MANIFEST.to_vec()),
                "https://example.test/release.json.minisig" => {
                    Ok(TEST_RELEASE_MANIFEST_SIGNATURE.as_bytes().to_vec())
                }
                _ => Err(NativeEngineError::Download {
                    detail: "unexpected fixture URL".to_owned(),
                }),
            }
        };

        let envelope = fetch_signed_manifest_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &fetch,
            "https://example.test/release.json",
        )
        .unwrap();
        assert_eq!(
            requested.into_inner(),
            [
                "https://example.test/release.json",
                "https://example.test/release.json.minisig"
            ]
        );
        assert_eq!(envelope.manifest, TEST_RELEASE_MANIFEST);

        let tampered_fetch = |url: &str| {
            if url.ends_with(".minisig") {
                Ok(TEST_RELEASE_MANIFEST_SIGNATURE.as_bytes().to_vec())
            } else {
                Ok(b"tampered".to_vec())
            }
        };
        assert!(fetch_signed_manifest_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &tampered_fetch,
            "https://example.test/release.json",
        )
        .is_err());
    }

    #[test]
    fn online_resolution_fetches_its_channel_and_never_falls_back_to_cached_envelope() {
        let files = test_files("online-resolution");
        let release = release_key("1.2.3");
        write_json_atomically(
            &files.signed_manifest_envelope(&release),
            &StoredSignedManifestEnvelope {
                manifest: b"stale cached content".to_vec(),
                signature: b"stale cached signature".to_vec(),
            },
        )
        .unwrap();
        let requested = RefCell::new(Vec::new());
        let fetch = |url: &str| {
            requested.borrow_mut().push(url.to_owned());
            match url {
                "https://data.phase-rs.dev/desktop/release-server-v1.2.3.json" => {
                    Ok(TEST_RELEASE_MANIFEST.to_vec())
                }
                "https://data.phase-rs.dev/desktop/release-server-v1.2.3.json.minisig" => {
                    Ok(TEST_RELEASE_MANIFEST_SIGNATURE.as_bytes().to_vec())
                }
                _ => Err(NativeEngineError::Download {
                    detail: "unexpected fixture URL".to_owned(),
                }),
            }
        };

        let resolved =
            resolve_online_artifact_with_key(TEST_MANIFEST_PUBLIC_KEY, &fetch, &files, &release)
                .unwrap();
        assert_eq!(
            read_signed_manifest_envelope(&files, &release)
                .unwrap()
                .manifest,
            b"stale cached content"
        );
        persist_fetched_envelope(&files, &release, &resolved).unwrap();
        assert_eq!(
            requested.into_inner(),
            [
                "https://data.phase-rs.dev/desktop/release-server-v1.2.3.json",
                "https://data.phase-rs.dev/desktop/release-server-v1.2.3.json.minisig"
            ]
        );
        let stored = read_signed_manifest_envelope(&files, &release).unwrap();
        assert_eq!(stored.manifest, TEST_RELEASE_MANIFEST);
        let invalid_fetch = |url: &str| {
            if url.ends_with(".minisig") {
                Ok(TEST_RELEASE_MANIFEST_SIGNATURE.as_bytes().to_vec())
            } else {
                Ok(b"tampered manifest".to_vec())
            }
        };
        assert!(resolve_online_artifact_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &invalid_fetch,
            &files,
            &release,
        )
        .is_err());
        assert_eq!(
            read_signed_manifest_envelope(&files, &release)
                .unwrap()
                .manifest,
            TEST_RELEASE_MANIFEST
        );

        let preview = preview_key("0123456789abcdef");
        let preview_fetch = |url: &str| match url {
            "https://data.phase-rs.dev/desktop/preview-server.json" => {
                Ok(TEST_PREVIEW_MANIFEST.to_vec())
            }
            "https://data.phase-rs.dev/desktop/preview-server.json.minisig" => {
                Ok(TEST_PREVIEW_MANIFEST_SIGNATURE.as_bytes().to_vec())
            }
            _ => Err(NativeEngineError::Download {
                detail: "release URL was used for preview".to_owned(),
            }),
        };
        let preview_resolved = resolve_online_artifact_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &preview_fetch,
            &files,
            &preview,
        )
        .unwrap();
        assert!(!files.signed_manifest_envelope(&preview).exists());
        assert!(!files.preview_ratchet().exists());
        persist_fetched_envelope(&files, &preview, &preview_resolved).unwrap();
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn signed_preview_envelope_is_channel_fingerprint_and_ratchet_exact() {
        let files = test_files("signed-preview-envelope");
        let key = preview_key("0123456789abcdef");
        let envelope = StoredSignedManifestEnvelope {
            manifest: TEST_PREVIEW_MANIFEST.to_vec(),
            signature: TEST_PREVIEW_MANIFEST_SIGNATURE.as_bytes().to_vec(),
        };
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &envelope,
        )
        .is_ok());
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &preview_key("fedcba9876543210"),
            &envelope,
        )
        .is_err());
        assert!(resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &release_key("1.2.3"),
            &envelope,
        )
        .is_err());

        write_json_atomically(&files.signed_manifest_envelope(&key), &envelope).unwrap();
        assert!(!files.preview_ratchet().exists());
        resolve_cached_artifact_with_key(TEST_MANIFEST_PUBLIC_KEY, &files, &key).unwrap();
        assert!(files.preview_ratchet().exists());
        fs::remove_file(files.preview_ratchet()).unwrap();
        write_json_atomically(
            &files.preview_ratchet(),
            &PreviewRatchet {
                generated_at: "2026-01-01T00:00:00Z".to_owned(),
            },
        )
        .unwrap();
        resolve_cached_artifact_with_key(TEST_MANIFEST_PUBLIC_KEY, &files, &key).unwrap();
        assert_eq!(
            read_json_optional::<PreviewRatchet>(&files.preview_ratchet())
                .unwrap()
                .unwrap()
                .generated_at,
            "2026-01-02T00:00:00Z"
        );
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn cached_binary_requires_its_matching_signature() {
        let files = test_files("binary-cache");
        let key = release_key("1.2.3");
        write_atomically(&files.binary(&key), TEST_BYTES).unwrap();
        write_atomically(&files.binary_signature(&key), TEST_SIGNATURE.as_bytes()).unwrap();

        assert!(cached_binary_is_verified_with_key(TEST_PUBLIC_KEY, &files, &key).unwrap());

        write_atomically(&files.binary(&key), b"tampered").unwrap();
        assert!(!cached_binary_is_verified_with_key(TEST_PUBLIC_KEY, &files, &key).unwrap());

        remove_file_if_exists(&files.binary_signature(&key)).unwrap();
        assert!(!cached_binary_is_verified_with_key(TEST_PUBLIC_KEY, &files, &key).unwrap());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn signed_manifest_envelopes_are_atomic_and_exact_key_scoped() {
        let files = test_files("signed-envelope");
        let release = release_key("1.2.3");
        let preview = preview_key("0123456789abcdef");
        let envelope = StoredSignedManifestEnvelope {
            manifest: b"manifest".to_vec(),
            signature: b"signature".to_vec(),
        };

        write_json_atomically(&files.signed_manifest_envelope(&release), &envelope).unwrap();

        let restored = read_json_optional::<StoredSignedManifestEnvelope>(
            &files.signed_manifest_envelope(&release),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored.manifest, b"manifest");
        assert_eq!(restored.signature, b"signature");
        assert!(read_json_optional::<StoredSignedManifestEnvelope>(
            &files.signed_manifest_envelope(&preview)
        )
        .unwrap()
        .is_none());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn offline_resolution_requires_the_exact_cached_signed_envelope() {
        let files = test_files("offline-envelope");
        assert!(matches!(
            resolve_cached_artifact(&files, &release_key("1.2.3")),
            Err(NativeEngineError::Manifest { .. })
        ));
        let key = release_key("1.2.3");
        write_atomically(&files.signed_manifest_envelope(&key), b"not-json").unwrap();
        assert!(matches!(
            read_signed_manifest_envelope(&files, &key),
            Err(NativeEngineError::Verification { .. })
        ));
        fs::remove_dir_all(files.app_directory).unwrap();
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
        let no_fetch = |_url: &str| -> Result<Vec<u8>, NativeEngineError> {
            panic!("valid local data must not fetch")
        };
        assemble_data(
            &no_fetch,
            None,
            &files,
            &key,
            &preflight_data(&files, &key, std::slice::from_ref(&file)).unwrap(),
            NativeEngineIntent::StartOnline,
            true,
        )
        .unwrap();
        let destination = files.data_directory(&key).join(&file.name);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        remove_file_if_exists(&destination).unwrap();
        assemble_data(
            &no_fetch,
            None,
            &files,
            &key,
            &preflight_data(&files, &key, std::slice::from_ref(&file)).unwrap(),
            NativeEngineIntent::StartOffline,
            true,
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        write_atomically(&files.cache_blob(&file.sha256), b"tampered").unwrap();
        assert!(matches!(
            assemble_data(
                &no_fetch,
                None,
                &files,
                &key,
                &preflight_data(&files, &key, std::slice::from_ref(&file)).unwrap(),
                NativeEngineIntent::StartOffline,
                false,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        assert_eq!(
            fs::read(files.cache_blob(&file.sha256)).unwrap(),
            b"tampered"
        );
        write_atomically(&files.cache_blob(&file.sha256), b"tampered").unwrap();
        write_atomically(&destination, b"tampered").unwrap();
        assert!(matches!(
            assemble_data(
                &no_fetch,
                None,
                &files,
                &key,
                &preflight_data(&files, &key, std::slice::from_ref(&file)).unwrap(),
                NativeEngineIntent::StartOffline,
                true,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn preparation_repairs_an_invalid_blob_without_replacing_a_valid_destination() {
        let files = test_files("prepare-blob-repair");
        let key = release_key("1.2.3");
        let bytes = b"card data";
        let file = data_file("card-data.json", bytes);
        let destination = files.data_directory(&key).join(&file.name);
        write_atomically(&destination, bytes).unwrap();
        write_atomically(&files.cache_blob(&file.sha256), b"corrupt").unwrap();
        let preflight = preflight_data(&files, &key, std::slice::from_ref(&file)).unwrap();
        assert!(!preflight[0].cache_is_valid);
        assert!(preflight[0].destination_is_valid);
        let fetches = RefCell::new(0);
        let fetch = |_url: &str| {
            *fetches.borrow_mut() += 1;
            Ok(bytes.to_vec())
        };

        assemble_data(
            &fetch,
            None,
            &files,
            &key,
            &preflight,
            NativeEngineIntent::PrepareForOffline,
            false,
        )
        .unwrap();

        assert_eq!(*fetches.borrow(), 1);
        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert_eq!(fs::read(files.cache_blob(&file.sha256)).unwrap(), bytes);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn spawn_plan_uses_verified_cache_offline_without_fetching() {
        let files = test_files("offline-spawn-plan");
        let key = release_key("1.2.3");
        let data = data_file("card-data.json", b"card data");
        write_atomically(&files.binary(&key), TEST_BYTES).unwrap();
        write_atomically(&files.binary_signature(&key), TEST_SIGNATURE.as_bytes()).unwrap();
        write_atomically(&files.cache_blob(&data.sha256), b"card data").unwrap();
        let resolved = ResolvedArtifact {
            binary_url: "https://example.test/server".to_owned(),
            binary_signature_url: "https://example.test/server.minisig".to_owned(),
            data: vec![data.clone()],
            fetched_envelope: None,
        };
        let fetches = RefCell::new(0);
        let no_fetch = |_url: &str| -> Result<Vec<u8>, NativeEngineError> {
            *fetches.borrow_mut() += 1;
            Err(NativeEngineError::Download {
                detail: "offline must not fetch".to_owned(),
            })
        };

        let plan = provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &no_fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::StartOffline,
            true,
        )
        .unwrap();
        assert_eq!(*fetches.borrow(), 0);
        assert_eq!(plan.binary, files.binary(&key));
        assert_eq!(
            fs::read(plan.data_directory.join(&data.name)).unwrap(),
            b"card data"
        );
        assert!(plan
            .arguments
            .iter()
            .any(|argument| argument == "--no-data-download"));
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn offline_spawn_plan_rejects_invalid_binary_or_blob_without_fetching() {
        let files = test_files("offline-invalid-spawn-plan");
        let key = release_key("1.2.3");
        let data = data_file("card-data.json", b"card data");
        let resolved = ResolvedArtifact {
            binary_url: "https://example.test/server".to_owned(),
            binary_signature_url: "https://example.test/server.minisig".to_owned(),
            data: vec![data.clone()],
            fetched_envelope: None,
        };
        let fetches = RefCell::new(0);
        let fetch = |_url: &str| -> Result<Vec<u8>, NativeEngineError> {
            *fetches.borrow_mut() += 1;
            Ok(TEST_BYTES.to_vec())
        };
        assert!(matches!(
            provision_resolved_artifact_with_key(
                TEST_PUBLIC_KEY,
                None,
                &fetch,
                &files,
                &key,
                &resolved,
                NativeEngineIntent::StartOffline,
                true,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        assert_eq!(*fetches.borrow(), 0);

        write_atomically(&files.binary(&key), TEST_BYTES).unwrap();
        write_atomically(&files.binary_signature(&key), TEST_SIGNATURE.as_bytes()).unwrap();
        write_atomically(&files.data_directory(&key).join(&data.name), b"card data").unwrap();
        assert!(matches!(
            provision_resolved_artifact_with_key(
                TEST_PUBLIC_KEY,
                None,
                &fetch,
                &files,
                &key,
                &resolved,
                NativeEngineIntent::StartOffline,
                true,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        assert_eq!(*fetches.borrow(), 0);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn retained_preparation_preflights_every_destination_before_fetching_or_replacing() {
        let files = test_files("retained-preparation-preflight");
        let key = release_key("1.2.3");
        let data = data_file("card-data.json", b"card data");
        write_atomically(&files.binary(&key), TEST_BYTES).unwrap();
        write_atomically(&files.binary_signature(&key), TEST_SIGNATURE.as_bytes()).unwrap();
        write_atomically(&files.cache_blob(&data.sha256), b"corrupt").unwrap();
        let resolved = ResolvedArtifact {
            binary_url: "https://example.test/server".to_owned(),
            binary_signature_url: "https://example.test/server.minisig".to_owned(),
            data: vec![data.clone()],
            fetched_envelope: None,
        };
        let fetches = RefCell::new(0);
        let fetch = |_url: &str| -> Result<Vec<u8>, NativeEngineError> {
            *fetches.borrow_mut() += 1;
            Ok(b"card data".to_vec())
        };

        assert!(matches!(
            provision_resolved_artifact_with_key(
                TEST_PUBLIC_KEY,
                None,
                &fetch,
                &files,
                &key,
                &resolved,
                NativeEngineIntent::PrepareForOffline,
                false,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        assert_eq!(*fetches.borrow(), 0);
        assert!(!files.data_directory(&key).exists());

        write_atomically(&files.data_directory(&key).join(&data.name), b"card data").unwrap();
        provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::PrepareForOffline,
            false,
        )
        .unwrap();
        assert_eq!(*fetches.borrow(), 1);
        assert_eq!(
            fs::read(files.data_directory(&key).join(&data.name)).unwrap(),
            b"card data"
        );
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn online_spawn_plan_repairs_missing_or_corrupt_binary_and_reuses_valid_cache() {
        let files = test_files("online-spawn-plan");
        let key = release_key("1.2.3");
        let resolved = ResolvedArtifact {
            binary_url: "https://example.test/server".to_owned(),
            binary_signature_url: "https://example.test/server.minisig".to_owned(),
            data: Vec::new(),
            fetched_envelope: None,
        };
        let requests = RefCell::new(Vec::new());
        let fetch = |url: &str| {
            requests.borrow_mut().push(url.to_owned());
            if url.ends_with(".minisig") {
                Ok(TEST_SIGNATURE.as_bytes().to_vec())
            } else {
                Ok(TEST_BYTES.to_vec())
            }
        };

        provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::StartOnline,
            true,
        )
        .unwrap();
        assert_eq!(requests.borrow().len(), 2);
        requests.borrow_mut().clear();
        provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::StartOnline,
            true,
        )
        .unwrap();
        assert!(requests.borrow().is_empty());
        write_atomically(&files.binary(&key), b"corrupt").unwrap();
        provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::StartOnline,
            true,
        )
        .unwrap();
        assert_eq!(requests.borrow().len(), 2);
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
    fn lifecycle_decisions_preserve_preparation_safety_and_clean_stale_state() {
        let requested = release_key("2.0.0");
        let other = release_key("1.0.0");
        let files = test_files("preparation-preflight");
        assert!(!files.base.exists());
        let effects = RefCell::new(Vec::new());
        assert!(matches!(
            after_preparation_preflight(
                NativeEngineIntent::PrepareForOffline,
                &requested,
                Some((&other, true)),
                None,
                || {
                    effects
                        .borrow_mut()
                        .push("create/ratchet/envelope/artifact");
                    Ok(())
                },
            ),
            Err(NativeEngineError::Health { .. })
        ));
        assert!(effects.borrow().is_empty());
        assert!(!files.base.exists());
        assert!(matches!(
            after_preparation_preflight(
                NativeEngineIntent::PrepareForOffline,
                &requested,
                None,
                Some((&other, true)),
                || {
                    effects.borrow_mut().push("clear-record/abort-bridges");
                    Ok(())
                },
            ),
            Err(NativeEngineError::Health { .. })
        ));
        assert!(effects.borrow().is_empty());
        assert!(!files.base.exists());
        fs::remove_dir_all(files.app_directory).unwrap();
        assert_eq!(
            lifecycle_decision(
                NativeEngineIntent::PrepareForOffline,
                &requested,
                &requested,
                true,
            ),
            LifecycleDecision::RetainAndVerify
        );
        assert_eq!(
            lifecycle_decision(
                NativeEngineIntent::PrepareForOffline,
                &requested,
                &other,
                true,
            ),
            LifecycleDecision::RefuseWithoutSideEffects
        );
        assert_eq!(
            lifecycle_decision(
                NativeEngineIntent::StartOffline,
                &requested,
                &requested,
                false,
            ),
            LifecycleDecision::CleanStale
        );
        assert_eq!(
            lifecycle_decision(NativeEngineIntent::StartOnline, &requested, &other, true,),
            LifecycleDecision::Replace
        );
    }

    #[test]
    fn orchestration_resolves_preview_before_adoption_and_release_after_adoption() {
        assert_eq!(
            artifact_resolution_order(&release_key("1.2.3"), NativeEngineIntent::StartOnline,),
            ArtifactResolutionOrder::AfterPersistedLifecycle
        );
        assert_eq!(
            artifact_resolution_order(
                &preview_key("0123456789abcdef"),
                NativeEngineIntent::StartOnline,
            ),
            ArtifactResolutionOrder::BeforePersistedLifecycle
        );
        assert_eq!(
            artifact_resolution_order(
                &preview_key("0123456789abcdef"),
                NativeEngineIntent::PrepareForOffline,
            ),
            ArtifactResolutionOrder::BeforePersistedLifecycle
        );
        assert_eq!(
            artifact_resolution_order(
                &preview_key("0123456789abcdef"),
                NativeEngineIntent::StartOffline,
            ),
            ArtifactResolutionOrder::AfterPersistedLifecycle
        );
    }

    #[test]
    fn persisted_record_lifecycle_adopts_release_without_a_remote_resolution() {
        let files = test_files("release-record-adoption");
        let key = release_key("1.2.3");
        let record = SpawnRecord {
            pid: 123,
            port: 9374,
            key: key.clone(),
        };
        write_spawn_record(&files, &record).unwrap();
        let (abort, _registration) = futures_util::future::AbortHandle::new_pair();
        let (outbound, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = NativeEngineState::default();
        state.bridges.insert(1, BridgeHandle::new(abort, outbound));

        let outcome = apply_persisted_record_lifecycle(
            &mut state,
            &files,
            &key,
            NativeEngineIntent::StartOnline,
            Some((record, true)),
            None,
        )
        .unwrap();

        assert!(matches!(
            outcome,
            PersistedRecordOutcome::ReturnReady(NativeEngineReady { port: 9374 })
        ));
        assert_eq!(state.running.as_ref().unwrap().key(), &key);
        assert_eq!(state.running.as_ref().unwrap().port(), 9374);
        assert!(state.bridges.contains_key(&1));
        assert_eq!(read_spawn_record(&files).unwrap().unwrap().key, key);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn preview_record_adoption_persists_verified_envelope_and_ratchet_before_returning() {
        let files = test_files("preview-record-adoption");
        let key = preview_key("0123456789abcdef");
        let envelope = StoredSignedManifestEnvelope {
            manifest: TEST_PREVIEW_MANIFEST.to_vec(),
            signature: TEST_PREVIEW_MANIFEST_SIGNATURE.as_bytes().to_vec(),
        };
        let mut resolved = resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &envelope,
        )
        .unwrap();
        resolved.fetched_envelope = Some(envelope);
        let record = SpawnRecord {
            pid: 123,
            port: 9374,
            key: key.clone(),
        };
        let mut state = NativeEngineState::default();

        let outcome = apply_persisted_record_lifecycle(
            &mut state,
            &files,
            &key,
            NativeEngineIntent::StartOnline,
            Some((record, true)),
            Some(&resolved),
        )
        .unwrap();

        assert!(matches!(
            outcome,
            PersistedRecordOutcome::ReturnReady(NativeEngineReady { port: 9374 })
        ));
        assert!(files.signed_manifest_envelope(&key).exists());
        assert_eq!(
            read_json_optional::<PreviewRatchet>(&files.preview_ratchet())
                .unwrap()
                .unwrap()
                .generated_at,
            "2026-01-02T00:00:00Z"
        );
        assert_eq!(state.running.as_ref().unwrap().key(), &key);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn fetched_preview_authority_persists_after_preflight_and_before_artifact_apply() {
        let files = test_files("preview-authority-before-apply");
        let key = preview_key("0123456789abcdef");
        let envelope = StoredSignedManifestEnvelope {
            manifest: TEST_PREVIEW_MANIFEST.to_vec(),
            signature: TEST_PREVIEW_MANIFEST_SIGNATURE.as_bytes().to_vec(),
        };
        let mut resolved = resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &envelope,
        )
        .unwrap();
        resolved.fetched_envelope = Some(envelope);
        let fetches = RefCell::new(0);
        let fetch = |url: &str| {
            assert!(files.signed_manifest_envelope(&key).exists());
            assert!(files.preview_ratchet().exists());
            *fetches.borrow_mut() += 1;
            if url.ends_with(".minisig") {
                Ok(TEST_SIGNATURE.as_bytes().to_vec())
            } else {
                Ok(TEST_BYTES.to_vec())
            }
        };

        provision_resolved_artifact_with_key(
            TEST_PUBLIC_KEY,
            None,
            &fetch,
            &files,
            &key,
            &resolved,
            NativeEngineIntent::PrepareForOffline,
            true,
        )
        .unwrap();

        assert_eq!(*fetches.borrow(), 2);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn retained_preflight_refusal_leaves_prior_preview_authority_untouched() {
        let files = test_files("retained-preflight-authority");
        let key = preview_key("0123456789abcdef");
        let prior = StoredSignedManifestEnvelope {
            manifest: b"prior authority".to_vec(),
            signature: b"prior signature".to_vec(),
        };
        write_json_atomically(&files.signed_manifest_envelope(&key), &prior).unwrap();
        let envelope = StoredSignedManifestEnvelope {
            manifest: TEST_PREVIEW_MANIFEST.to_vec(),
            signature: TEST_PREVIEW_MANIFEST_SIGNATURE.as_bytes().to_vec(),
        };
        let mut resolved = resolved_artifact_from_envelope_with_key(
            TEST_MANIFEST_PUBLIC_KEY,
            &files,
            &key,
            &envelope,
        )
        .unwrap();
        resolved.fetched_envelope = Some(envelope);
        let fetches = RefCell::new(0);
        let fetch = |_url: &str| -> Result<Vec<u8>, NativeEngineError> {
            *fetches.borrow_mut() += 1;
            Ok(TEST_BYTES.to_vec())
        };

        assert!(matches!(
            provision_resolved_artifact_with_key(
                TEST_PUBLIC_KEY,
                None,
                &fetch,
                &files,
                &key,
                &resolved,
                NativeEngineIntent::PrepareForOffline,
                false,
            ),
            Err(NativeEngineError::Verification { .. })
        ));
        assert_eq!(*fetches.borrow(), 0);
        assert_eq!(
            read_signed_manifest_envelope(&files, &key)
                .unwrap()
                .manifest,
            b"prior authority"
        );
        assert!(!files.preview_ratchet().exists());
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn retained_preparation_adopts_the_full_record_without_aborting_bridges() {
        let files = test_files("retained-record-adoption");
        let key = release_key("1.2.3");
        let record = SpawnRecord {
            pid: 123,
            port: 9374,
            key: key.clone(),
        };
        let (abort, _registration) = futures_util::future::AbortHandle::new_pair();
        let (outbound, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = NativeEngineState::default();
        state.bridges.insert(1, BridgeHandle::new(abort, outbound));

        let outcome = apply_persisted_record_lifecycle(
            &mut state,
            &files,
            &key,
            NativeEngineIntent::PrepareForOffline,
            Some((record, true)),
            None,
        )
        .unwrap();
        let PersistedRecordOutcome::Retain(record) = outcome else {
            panic!("healthy exact preparation must retain the full record");
        };
        let ready = adopt_persisted_record(&mut state, record);

        assert_eq!(ready.port, 9374);
        assert_eq!(state.running.as_ref().unwrap().key(), &key);
        assert!(state.bridges.contains_key(&1));
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn persisted_preparation_refusal_preserves_running_record_and_bridges() {
        let files = test_files("record-refusal");
        let requested = release_key("1.2.3");
        let other = release_key("2.0.0");
        let retained = SpawnRecord {
            pid: 111,
            port: 9374,
            key: requested.clone(),
        };
        let conflicting = SpawnRecord {
            pid: 222,
            port: 9375,
            key: other.clone(),
        };
        write_spawn_record(&files, &conflicting).unwrap();
        let (abort, _registration) = futures_util::future::AbortHandle::new_pair();
        let (outbound, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut state = NativeEngineState {
            running: Some(RunningEngine::Adopted(retained)),
            bridges: BTreeMap::from([(1, BridgeHandle::new(abort, outbound))]),
            next_bridge_id: 2,
        };

        assert!(matches!(
            apply_persisted_record_lifecycle(
                &mut state,
                &files,
                &requested,
                NativeEngineIntent::PrepareForOffline,
                Some((conflicting, true)),
                None,
            ),
            Err(NativeEngineError::Health { .. })
        ));
        assert_eq!(state.running.as_ref().unwrap().key(), &requested);
        assert!(state.bridges.contains_key(&1));
        assert_eq!(read_spawn_record(&files).unwrap().unwrap().key, other);
        fs::remove_dir_all(files.app_directory).unwrap();
    }

    #[test]
    fn health_timeout_constant_is_short_and_bounded() {
        assert!(HEALTH_TIMEOUT >= Duration::from_secs(10));
        assert!(HEALTH_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn offline_and_preparation_server_arguments_disable_child_data_downloads() {
        let online = server_arguments(RELEASE_ORIGIN, NativeEngineIntent::StartOnline);
        let offline = server_arguments(RELEASE_ORIGIN, NativeEngineIntent::StartOffline);
        let preparation = server_arguments(RELEASE_ORIGIN, NativeEngineIntent::PrepareForOffline);

        assert!(!online
            .iter()
            .any(|argument| argument == "--no-data-download"));
        assert!(offline
            .iter()
            .any(|argument| argument == "--no-data-download"));
        assert!(preparation
            .iter()
            .any(|argument| argument == "--no-data-download"));
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
