use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const STASH_FILE: &str = "legacy-storage-stash.json";
const MIGRATION_MARKER_FILE: &str = "legacy-storage-imported";
const REMOTE_LOAD_OK_MARKER_FILE: &str = "remote-load-ok";
const CHANNEL_PREFERENCE_FILE: &str = "channel-preference.json";

/// The remote content channel the shell loads.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    #[default]
    Release,
    Preview,
}

/// The bootstrap's one-roundtrip migration and remote-navigation state.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct StashLegacyStorageResult {
    pub remote_load_ok: bool,
    pub channel: Channel,
}

struct MigrationFiles {
    directory: PathBuf,
}

impl MigrationFiles {
    fn from_app(app: &AppHandle) -> Result<Self, String> {
        let directory = app
            .path()
            .app_local_data_dir()
            .map_err(|error| error.to_string())?;
        Ok(Self { directory })
    }

    fn stash(&self) -> PathBuf {
        self.directory.join(STASH_FILE)
    }

    fn migration_marker(&self) -> PathBuf {
        self.directory.join(MIGRATION_MARKER_FILE)
    }

    fn remote_load_ok_marker(&self) -> PathBuf {
        self.directory.join(REMOTE_LOAD_OK_MARKER_FILE)
    }

    fn channel_preference(&self) -> PathBuf {
        self.directory.join(CHANNEL_PREFERENCE_FILE)
    }

    fn ensure_directory(&self) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())
    }
}

fn file_exists(path: &Path) -> Result<bool, String> {
    path.try_exists().map_err(|error| error.to_string())
}

fn read_stash(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(json) => Ok(Some(json)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn remove_stash(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_marker(path: &Path) -> Result<(), String> {
    if !file_exists(path)? {
        fs::write(path, "ok\n").map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn stage_legacy_storage_marker(files: &MigrationFiles, json: &str) -> Result<bool, String> {
    let migration_complete = file_exists(&files.migration_marker())?;

    if !migration_complete && !json.trim().is_empty() && !file_exists(&files.stash())? {
        files.ensure_directory()?;
        fs::write(files.stash(), json).map_err(|error| error.to_string())?;
    }

    file_exists(&files.remote_load_ok_marker())
}

fn confirm_legacy_storage(files: &MigrationFiles) -> Result<(), String> {
    files.ensure_directory()?;
    write_marker(&files.migration_marker())?;
    remove_stash(&files.stash())
}

fn mark_remote_load(files: &MigrationFiles) -> Result<(), String> {
    files.ensure_directory()?;
    write_marker(&files.remote_load_ok_marker())
}

fn read_channel_preference(files: &MigrationFiles) -> Channel {
    fs::read_to_string(files.channel_preference())
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn set_channel(files: &MigrationFiles, channel: Channel) -> Result<(), String> {
    files.ensure_directory()?;
    let serialized = serde_json::to_string(&channel).map_err(|error| error.to_string())?;
    fs::write(files.channel_preference(), serialized).map_err(|error| error.to_string())
}

fn stage_legacy_storage(
    files: &MigrationFiles,
    json: &str,
) -> Result<StashLegacyStorageResult, String> {
    let remote_load_ok = stage_legacy_storage_marker(files, json)?;
    Ok(StashLegacyStorageResult {
        remote_load_ok,
        channel: read_channel_preference(files),
    })
}

/// Writes the bootstrap's one-time handoff and returns the remote-load marker
/// plus the stored channel preference. This avoids a second IPC query before
/// the bootstrap navigates.
#[tauri::command]
pub fn stash_legacy_storage(
    app: AppHandle,
    json: String,
) -> Result<StashLegacyStorageResult, String> {
    let files = MigrationFiles::from_app(&app)?;
    stage_legacy_storage(&files, &json)
}

/// Persists the remote content channel selected by first-party shell content.
#[tauri::command]
pub fn set_channel_preference(app: AppHandle, channel: Channel) -> Result<(), String> {
    let files = MigrationFiles::from_app(&app)?;
    set_channel(&files, channel)
}

/// Reads the staged handoff without consuming it so a failed remote import can retry.
#[tauri::command]
pub fn take_legacy_storage(app: AppHandle) -> Result<Option<String>, String> {
    let files = MigrationFiles::from_app(&app)?;
    read_stash(&files.stash())
}

/// Records a completed remote import before removing the staged handoff.
#[tauri::command]
pub fn confirm_legacy_import(app: AppHandle) -> Result<(), String> {
    let files = MigrationFiles::from_app(&app)?;
    confirm_legacy_storage(&files)
}

/// Marks a completed remote app boot, enabling offline navigation on later launches.
#[tauri::command]
pub fn mark_remote_load_ok(app: AppHandle) -> Result<(), String> {
    let files = MigrationFiles::from_app(&app)?;
    mark_remote_load(&files)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn test_files() -> MigrationFiles {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        MigrationFiles {
            directory: std::env::temp_dir().join(format!("phase-tauri-migration-{unique}")),
        }
    }

    #[test]
    fn existing_stash_is_not_overwritten_before_import_confirmation() {
        let files = test_files();
        let result = stage_legacy_storage(&files, "first").unwrap();
        assert!(!result.remote_load_ok);
        assert_eq!(result.channel, Channel::Release);
        assert!(
            !stage_legacy_storage(&files, "second")
                .unwrap()
                .remote_load_ok
        );
        assert_eq!(
            read_stash(&files.stash()).unwrap().as_deref(),
            Some("first")
        );
        fs::remove_dir_all(files.directory).unwrap();
    }

    #[test]
    fn confirmation_writes_marker_and_consumes_stash() {
        let files = test_files();
        stage_legacy_storage(&files, "stash").unwrap();
        confirm_legacy_storage(&files).unwrap();

        assert!(file_exists(&files.migration_marker()).unwrap());
        assert_eq!(read_stash(&files.stash()).unwrap(), None);
        fs::remove_dir_all(files.directory).unwrap();
    }

    #[test]
    fn remote_load_marker_unlocks_unconditional_navigation() {
        let files = test_files();

        mark_remote_load(&files).unwrap();

        assert!(stage_legacy_storage(&files, "").unwrap().remote_load_ok);
        fs::remove_dir_all(files.directory).unwrap();
    }

    #[test]
    fn channel_preference_defaults_to_release_when_missing_or_invalid() {
        let files = test_files();

        assert_eq!(read_channel_preference(&files), Channel::Release);

        files.ensure_directory().unwrap();
        fs::write(files.channel_preference(), "invalid").unwrap();
        assert_eq!(read_channel_preference(&files), Channel::Release);
        fs::remove_dir_all(files.directory).unwrap();
    }

    #[test]
    fn channel_preference_persists() {
        let files = test_files();

        set_channel(&files, Channel::Preview).unwrap();

        assert_eq!(read_channel_preference(&files), Channel::Preview);
        fs::remove_dir_all(files.directory).unwrap();
    }

    #[test]
    fn staged_storage_returns_remote_load_and_channel() {
        let files = test_files();
        set_channel(&files, Channel::Preview).unwrap();
        mark_remote_load(&files).unwrap();

        assert_eq!(
            stage_legacy_storage(&files, "stash").unwrap(),
            StashLegacyStorageResult {
                remote_load_ok: true,
                channel: Channel::Preview,
            }
        );
        fs::remove_dir_all(files.directory).unwrap();
    }
}
