use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginCommand {
    List,
    Inspect(String),
    Preview(String),
    Install(String),
    Update(String),
    Link(String),
    Enable(String),
    Disable(String),
    Uninstall(String),
    Action(String),
    Qualify(String),
    Logs(String),
    ConfigDir(String),
}
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ManagementParseError {
    #[error("plugin command is missing")]
    Missing,
    #[error("plugin command is unsupported")]
    Unsupported,
    #[error("plugin command requires an id or source")]
    MissingArgument,
}
impl PluginCommand {
    pub fn parse(args: &[&str]) -> Result<Self, ManagementParseError> {
        let Some(command) = args.first().copied() else {
            return Err(ManagementParseError::Missing);
        };
        let one = || {
            args.get(1)
                .map(|value| (*value).to_owned())
                .ok_or(ManagementParseError::MissingArgument)
        };
        match command {
            "list" => Ok(Self::List),
            "inspect" => one().map(Self::Inspect),
            "preview" => one().map(Self::Preview),
            "install" => one().map(Self::Install),
            "update" => one().map(Self::Update),
            "link" => one().map(Self::Link),
            "enable" => one().map(Self::Enable),
            "disable" => one().map(Self::Disable),
            "uninstall" => one().map(Self::Uninstall),
            "action" => one().map(Self::Action),
            "qualify" => one().map(Self::Qualify),
            "logs" => one().map(Self::Logs),
            "config-dir" => one().map(Self::ConfigDir),
            _ => Err(ManagementParseError::Unsupported),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    PluginInvalid,
    PluginUntrusted,
    PluginIncompatible,
    PluginDisabled,
    PluginBusy,
    PluginGitUnavailable,
    PluginBuildFailed,
    PluginRestartRequired,
    PluginOutcomeUnknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartNotice {
    pub restart_needed: bool,
}
impl RestartNotice {
    pub fn runtime_changed() -> Self {
        Self {
            restart_needed: true,
        }
    }
}
