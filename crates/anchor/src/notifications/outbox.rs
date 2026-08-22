use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::model::{NotificationJob, OUTBOX_SCHEMA_VERSION};

struct OutboxLock(File);

impl Drop for OutboxLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn enqueue(root: &Path, job: &NotificationJob) -> Result<bool, String> {
    let dir = workspace_dir(root, &job.workspace_id);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let _lock = acquire_lock(&dir)?;
    let delivered = delivered_path(&dir, &job.id);
    if delivered.exists() {
        return Ok(false);
    }
    let path = job_path(&dir, &job.id);
    if path.exists() {
        return Ok(false);
    }
    write_new_json(&path, job)?;
    Ok(true)
}

pub fn pending(root: &Path, workspace_id: &str) -> Result<Vec<NotificationJob>, String> {
    let dir = workspace_dir(root, workspace_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let _lock = acquire_lock(&dir)?;
    let mut jobs = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if delivered_path(&dir, stem).exists() {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        let job: NotificationJob = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid notification outbox {}: {error}", path.display()))?;
        if job.schema_version != OUTBOX_SCHEMA_VERSION || job.workspace_id != workspace_id {
            continue;
        }
        jobs.push(job);
    }
    jobs.sort_by(|left, right| {
        left.created_at_unix_ms
            .cmp(&right.created_at_unix_ms)
            .then(left.id.cmp(&right.id))
    });
    Ok(jobs)
}

pub fn mark_delivered(root: &Path, job: &NotificationJob) -> Result<(), String> {
    let dir = workspace_dir(root, &job.workspace_id);
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let _lock = acquire_lock(&dir)?;
    let marker = delivered_path(&dir, &job.id);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(mut file) => {
            file.write_all(b"delivered\n")
                .and_then(|_| file.sync_all())
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn workspace_dir(root: &Path, workspace_id: &str) -> PathBuf {
    root.join("notifications").join(workspace_id).join("ilink")
}

fn job_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn delivered_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.delivered"))
}

fn acquire_lock(dir: &Path) -> Result<OutboxLock, String> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(dir.join(".lock"))
        .map_err(|error| error.to_string())?;
    file.lock_exclusive().map_err(|error| error.to_string())?;
    Ok(OutboxLock(file))
}

fn write_new_json(path: &Path, value: &NotificationJob) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> NotificationJob {
        NotificationJob::new("workspace", "profile", "task", "done".into(), 10)
    }

    #[test]
    fn enqueue_is_idempotent_and_delivered_jobs_stay_suppressed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let job = job();
        assert!(enqueue(temp.path(), &job).expect("first enqueue"));
        assert!(!enqueue(temp.path(), &job).expect("second enqueue"));
        assert_eq!(pending(temp.path(), "workspace").expect("pending").len(), 1);
        mark_delivered(temp.path(), &job).expect("delivered");
        assert!(pending(temp.path(), "workspace")
            .expect("pending")
            .is_empty());
        assert!(!enqueue(temp.path(), &job).expect("delivered enqueue"));
    }
}
