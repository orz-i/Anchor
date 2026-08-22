use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CURSOR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorState {
    schema_version: u32,
    account_key: String,
    cursor: String,
}

pub fn load_cursor(profile_id: &str, account_key: &str) -> Result<String, String> {
    let path = cursor_path(profile_id)?;
    load_cursor_from_path(&path, account_key)
}

fn load_cursor_from_path(path: &Path, account_key: &str) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let state: CursorState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid iLink cursor state {}: {error}", path.display()))?;
    if state.schema_version != CURSOR_SCHEMA_VERSION || state.account_key != account_key {
        return Ok(String::new());
    }
    Ok(state.cursor)
}

pub fn save_cursor(profile_id: &str, account_key: &str, cursor: &str) -> Result<(), String> {
    let path = cursor_path(profile_id)?;
    save_cursor_to_path(&path, account_key, cursor)
}

fn save_cursor_to_path(path: &Path, account_key: &str, cursor: &str) -> Result<(), String> {
    let state = CursorState {
        schema_version: CURSOR_SCHEMA_VERSION,
        account_key: account_key.to_string(),
        cursor: cursor.to_string(),
    };
    write_private_json(path, &state)
}

pub fn reset_cursor(profile_id: &str) -> Result<(), String> {
    let path = cursor_path(profile_id)?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn cursor_path(profile_id: &str) -> Result<PathBuf, String> {
    validate_profile_id(profile_id)?;
    let root = crate::platform::platform()
        .app_config_dir()
        .map_err(|error| error.to_string())?
        .join("data")
        .join("ilink")
        .join(profile_id);
    ensure_private_dir(&root)?;
    Ok(root.join("cursor.json"))
}

fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(&temp, path).map_err(|error| error.to_string())
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn validate_profile_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err("invalid workspace profile id for iLink state".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips_and_is_scoped_to_account() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("cursor.json");
        save_cursor_to_path(&path, "bot-a", "cursor-a").expect("save cursor");
        assert_eq!(
            load_cursor_from_path(&path, "bot-a").expect("load"),
            "cursor-a"
        );
        assert_eq!(
            load_cursor_from_path(&path, "bot-b").expect("other account"),
            ""
        );
    }
}
