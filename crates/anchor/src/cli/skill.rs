use serde::Serialize;

use crate::data::DataStore;
use crate::error::{AppError, AppResult};
use crate::skills::{SkillCatalog, SkillChannel, SkillSettings};

use super::args::SkillCommand;

pub async fn execute(command: SkillCommand, as_json: bool) -> AppResult<i32> {
    let workspace = match &command {
        SkillCommand::List { workspace }
        | SkillCommand::Validate { workspace, .. }
        | SkillCommand::Install { workspace, .. }
        | SkillCommand::SetChannel { workspace, .. }
        | SkillCommand::Activate { workspace, .. }
        | SkillCommand::Rollback { workspace, .. }
        | SkillCommand::Remove { workspace, .. } => workspace,
    };
    let store = DataStore::load()?;
    let profile = super::resolve_workspace(store.list(), workspace)?.clone();
    drop(store);
    let catalog = SkillCatalog::new(profile.path.into());
    catalog.configure(SkillSettings::new(profile.runtime.skill_service_enabled));

    match command {
        SkillCommand::List { .. } => {
            print_result(&catalog.packages(), as_json, "Skill package store")
        }
        SkillCommand::Validate { path, .. } => {
            let result = catalog.validate_package(&path).map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill package validation")
        }
        SkillCommand::Install {
            path,
            channel,
            activate,
            ..
        } => {
            let channel = SkillChannel::parse(&channel).map_err(AppError::Message)?;
            let result = catalog
                .install_package(&path, channel, activate)
                .map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill package installed")
        }
        SkillCommand::SetChannel {
            name,
            channel,
            version,
            ..
        } => {
            let channel = SkillChannel::parse(&channel).map_err(AppError::Message)?;
            let result = catalog
                .set_channel(&name, channel, &version)
                .map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill channel updated")
        }
        SkillCommand::Activate { name, channel, .. } => {
            let channel = SkillChannel::parse(&channel).map_err(AppError::Message)?;
            let result = catalog
                .activate_package(&name, channel)
                .map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill package activated")
        }
        SkillCommand::Rollback { name, .. } => {
            let result = catalog.rollback_package(&name).map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill package rolled back")
        }
        SkillCommand::Remove { name, version, .. } => {
            let result = catalog
                .remove_package(&name, &version)
                .map_err(AppError::Message)?;
            print_result(&result, as_json, "Skill package removed")
        }
    }
}

fn print_result(value: &impl Serialize, as_json: bool, label: &str) -> AppResult<i32> {
    let encoded = serde_json::to_string_pretty(value)
        .map_err(|error| AppError::Message(error.to_string()))?;
    if as_json {
        println!("{encoded}");
    } else {
        println!("{label}:\n{encoded}");
    }
    Ok(0)
}
