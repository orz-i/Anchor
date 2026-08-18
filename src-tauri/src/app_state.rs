use std::sync::Mutex;

use crate::data::DataStore;
use crate::error::AppResult;

pub struct AppState {
    pub data: Mutex<DataStore>,
}

impl AppState {
    pub fn new() -> AppResult<Self> {
        let mut store = DataStore::load()?;
        store.init_shared_secrets()?;
        Ok(Self {
            data: Mutex::new(store),
        })
    }

    pub fn with_data<R>(&self, f: impl FnOnce(&mut DataStore) -> AppResult<R>) -> AppResult<R> {
        let latest = DataStore::load()?;
        let mut guard = self
            .data
            .lock()
            .map_err(|_| crate::error::AppError::Message("data store poisoned".into()))?;
        *guard = latest;
        f(&mut guard)
    }

    pub fn with_workspaces<R>(
        &self,
        f: impl FnOnce(&mut DataStore) -> AppResult<R>,
    ) -> AppResult<R> {
        self.with_data(f)
    }

    pub fn with_settings<R>(&self, f: impl FnOnce(&mut DataStore) -> AppResult<R>) -> AppResult<R> {
        self.with_data(f)
    }

    pub fn reload_data_from_disk(&self) -> AppResult<()> {
        let next = DataStore::load()?;
        let mut guard = self
            .data
            .lock()
            .map_err(|_| crate::error::AppError::Message("data store poisoned".into()))?;
        *guard = next;
        Ok(())
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new().expect("failed to initialize app state")
    }
}

pub fn teardown_workspace(store: &mut DataStore, profile_id: &str) -> AppResult<()> {
    store.remove_workspace_secrets(profile_id)?;
    crate::secret::SecretStore::clear_refresh_replay_state(profile_id)
}
