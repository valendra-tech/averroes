pub mod ask_user;
pub mod bash;
pub mod browser;
pub mod call_agents;
pub mod change_directory;
pub mod checkpoint;
pub mod context_window;
pub mod deep_memory;
pub mod desktop;
pub mod discover_tools;
pub mod enable_tools;
pub mod file_read;
pub mod file_write;
pub mod glob;
pub mod global_memory;
pub mod grep;
pub mod history;
pub mod install_skill;
pub mod list_agents;
pub mod list_skills;
pub mod list_tools;
pub mod load_skill;
pub mod notes;
pub mod patch;
pub mod scheduled;
pub mod search_memory;
pub mod search_skills;
mod shell_session;
pub mod task;
pub(crate) mod web_browser;
pub mod web_fetch;
#[path = "web_search.rs"]
pub mod web_search_intrernal;

use crate::skill::SkillIndex;
use crate::tool::ToolRegistry;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Resolve a tool path without confining it to the active workspace.
/// Relative paths start at the conversation's current directory; absolute
/// paths and `..` components intentionally remain valid across projects.
pub(crate) fn resolve_file_path(current_dir: &Path, requested: &str) -> PathBuf {
    let requested = Path::new(requested);
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        current_dir.join(requested)
    }
}

pub fn register_all(registry: &ToolRegistry) {
    registry.register(bash::BashTool::default());
    registry.register(change_directory::ChangeDirectoryTool);
    registry.register(context_window::GetContextRemainingTool);
    registry.register(context_window::NewContextTool);
    registry.register(file_read::FileReadTool);
    registry.register(file_write::FileWriteTool);
    registry.register(patch::PatchTool);
    registry.register(glob::GlobTool);
    registry.register(grep::GrepTool);
    registry.register(web_fetch::WebFetchTool::default());
    registry.register(browser::BrowserTool::default());
    desktop::register(registry);
    registry.register(web_search_intrernal::WebSearchTool);
    registry.register(list_agents::ListAgentsTool);
    registry.register(call_agents::CallAgentsTool);
}

pub fn register_work_tools(registry: &ToolRegistry, database: Arc<crate::work::WorkDatabase>) {
    registry.set_work_database(database.clone());
    registry.register(history::HistoryTool::new(database.clone()));
    registry.register(notes::NotesTool::new(database));
}

pub fn register_scheduled_task_tools(
    registry: &ToolRegistry,
    service: Arc<crate::task::scheduled::ScheduledTaskService>,
) {
    registry.register(scheduled::ScheduledTaskListTool::new(service.clone()));
    registry.register(scheduled::AddScheduledTaskTool::new(service.clone()));
    registry.register(scheduled::UpdateScheduledTaskTool::new(service.clone()));
    registry.register(scheduled::DeleteScheduledTaskTool::new(service));
}

pub fn register_skill_tools(registry: &ToolRegistry, index: Arc<SkillIndex>) {
    registry.register(list_skills::ListSkillsTool::new(index.clone()));
    registry.register(load_skill::LoadSkillTool::new(index));
}

pub fn register_skill_marketplace_tools(
    registry: &ToolRegistry,
    marketplace: Arc<dyn crate::tool::SkillMarketplaceBackend>,
) {
    registry.register(search_skills::SearchSkillsTool::new(marketplace.clone()));
    registry.register(install_skill::InstallSkillTool::new(marketplace));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_all_does_not_expose_compaction_as_an_agent_tool() {
        let registry = ToolRegistry::new();
        register_all(&registry);

        assert!(registry.get("compact_conversation").is_none());
        assert!(!registry
            .catalog()
            .iter()
            .any(|tool| tool.name == "compact_conversation"));
        assert!(!registry
            .bootstrap_names()
            .iter()
            .any(|name| name == "compact_conversation"));
        assert!(registry.get("patch").is_some());
        assert!(registry.get("browser").is_some());
        assert!(registry.get("change_directory").is_some());
        assert!(registry.get("desktop_screenshot").is_some());
        assert!(registry.get("desktop_input").is_some());
        assert!(registry.get("discover_tools").is_none());
        assert!(registry.get("enable_tools").is_none());
        assert!(registry.get("list_tools").is_none());
        assert!(registry.get("list_agents").is_some());
    }

    #[test]
    fn register_all_includes_context_recovery_tools_and_existing_catalog() {
        let registry = ToolRegistry::new();
        register_all(&registry);

        assert!(registry.get("new_context").is_some());
        assert!(registry.get("get_context_remaining").is_some());
        assert!(registry.get("bash").is_some());
        assert!(registry.get("history").is_none());
        assert!(registry.get("notes").is_none());
    }

    #[test]
    fn register_work_tools_adds_database_tools_once_and_preserves_them_in_scopes() {
        let directory = tempfile::tempdir().unwrap();
        let database =
            crate::work::WorkDatabase::open_at(directory.path().join("averroes.db")).unwrap();
        let registry = ToolRegistry::new();

        register_work_tools(&registry, database.clone());
        register_work_tools(&registry, database.clone());

        assert!(registry.get("history").is_some());
        assert!(registry.get("notes").is_some());
        assert_eq!(
            registry
                .catalog()
                .iter()
                .filter(|tool| tool.name == "history" || tool.name == "notes")
                .count(),
            2
        );
        assert!(registry.work_database().is_some());

        let scoped = registry.fork();
        assert!(scoped.get("history").is_some());
        assert!(scoped.get("notes").is_some());
        assert!(scoped.work_database().is_some());
    }
}
