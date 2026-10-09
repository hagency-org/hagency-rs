//! Octos projection of the shared host-only task-helper descriptor (ADR-193
//! decision 5): the same scoped tools and guidance as Codex and Claude Code,
//! registered as Hagency's host tools `hagency.<tool>` on the session.
use super::Error;
use crate::task_mcp::{Profile, owned_task_tools};
use std::path::PathBuf;

/// The app every Hagency host tool is registered under. The model sees
/// `hagency_<tool>`.
pub const APP: &str = "hagency";

/// Fixed helper only. No credential, arbitrary args/config, Debug or serde API.
pub struct TaskTools {
    profile: Profile,
}
impl TaskTools {
    pub fn new(executable: PathBuf, task_id: String) -> Result<Self, Error> {
        Profile::new(executable, task_id, None)
            .map(|profile| Self { profile })
            .map_err(|_| Error::Input)
    }
    pub fn with_file_tools(mut self) -> Self {
        self.profile = self.profile.with_file_tools();
        self
    }
    pub fn with_receive_tools(mut self) -> Self {
        self.profile = self.profile.with_receive_tools();
        self
    }
    /// The helper's own tool names this session may call.
    pub fn tools(&self) -> Vec<&'static str> {
        owned_task_tools(self.profile.file_tools, self.profile.receive_tools)
    }
    /// The executable that serves them (`hagency mcp --owned-task-profile`).
    pub fn executable(&self) -> &str {
        &self.profile.executable
    }
    /// The same guidance Codex and Claude Code read, with the names these
    /// tools carry in an Octos session.
    pub fn guidance(&self) -> String {
        format!(
            "{} In this session these tools are named {APP}_<tool>, for example \
             {APP}_complete_task_with_reply, {APP}_read_conversation and {APP}_get_task.",
            self.profile.guidance()
        )
    }
    /// The task input with the guidance before it, as for Claude Code.
    pub fn prompt(&self, input: &str) -> String {
        format!("{}\n\nAssigned task input:\n{}", self.guidance(), input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_octos_task_tools_profile() {
        let tools = TaskTools::new("/bin/hagency".into(), "task_1".into()).unwrap();
        assert!(tools.tools().contains(&"complete_task_with_reply"));
        assert!(!tools.tools().contains(&"send_file"));
        // Every name is a valid Octos host tool name under the app.
        for tool in tools.with_file_tools().with_receive_tools().tools() {
            assert!(super::super::session::host_tool_name(&format!(
                "{APP}.{tool}"
            )));
        }
        let tools = TaskTools::new("/bin/hagency".into(), "task_1".into()).unwrap();
        let prompt = tools.prompt("hello");
        assert!(prompt.contains("task_1"));
        assert!(prompt.contains("hagency_complete_task_with_reply"));
        assert!(prompt.ends_with("Assigned task input:\nhello"));
        assert!(TaskTools::new("relative".into(), "task_1".into()).is_err());
    }
}
