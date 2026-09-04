mod migration;
mod model;
mod secret_protection;
mod storage;
mod store;

pub(crate) use migration::{
    export_portable_config, import_portable_config, ConfigExportSummary, ConfigImportSummary,
    WorkspacePathMapping,
};
pub use model::AppData;
pub(crate) use secret_protection::{
    protect_machine as protect_machine_secret_bytes,
    unprotect_machine as unprotect_machine_secret_bytes,
};
pub(crate) use storage::atomic_write;
pub(crate) use store::validate_workspace_profile;
pub use store::DataStore;
