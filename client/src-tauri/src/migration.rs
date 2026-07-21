use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use tauri::{AppHandle, Manager};

const STASH_FILE: &str = "legacy-storage-stash.json";
const MIGRATION_MARKER_FILE: &str = "legacy-storage-imported";
const REMOTE_LOAD_OK_MARKER_FILE: &str = "remote-load-ok";

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

fn stage_legacy_storage(files: &MigrationFiles, json: &str) -> Result<bool, String> {
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

/// Writes the bootstrap's one-time handoff and returns whether remote content
/// has successfully loaded before. The return value avoids a fifth IPC command
/// solely to query the remote-load marker.
#[tauri::command]
pub fn stash_legacy_storage(app: AppHandle, json: String) -> Result<bool, String> {
    let files = MigrationFiles::from_app(&app)?;
    stage_legacy_storage(&files, &json)
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
        let marker_exists = stage_legacy_storage(&files, "first").unwrap();
        assert!(!marker_exists);
        assert!(!stage_legacy_storage(&files, "second").unwrap());
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

        assert!(stage_legacy_storage(&files, "").unwrap());
        fs::remove_dir_all(files.directory).unwrap();
    }
}
