use crate::error::AppResult;

use super::args::SoftwareCommand;

pub async fn execute(command: SoftwareCommand, _as_json: bool) -> AppResult<i32> {
    match command {
        SoftwareCommand::List => {
            super::print_json(&crate::tunnel::list_software())?;
        }
        SoftwareCommand::Install { kind } => {
            let status = crate::tunnel::install_software(&kind).await?;
            super::print_json(&status)?;
        }
        SoftwareCommand::Uninstall { kind } => {
            let status = crate::tunnel::uninstall_software(&kind)?;
            super::print_json(&status)?;
        }
    }
    Ok(0)
}
