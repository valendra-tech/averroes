pub mod ask_user;
pub mod bash;
pub mod call_agents;
pub mod checkpoint;
pub mod deep_memory;
pub mod discover_tools;
pub mod enable_tools;
pub mod file_read;
pub mod file_write;
pub mod glob;
pub mod global_memory;
pub mod grep;
pub mod list_agents;
pub mod list_skills;
pub mod list_tools;
pub mod load_skill;
pub mod search_memory;
mod shell_session;
pub mod task;
mod web_browser;
pub mod web_fetch;
#[path = "web_search.rs"]
pub mod web_search_intrernal;

use crate::skill::SkillIndex;
use crate::tool::ToolRegistry;
use std::sync::Arc;

pub fn register_all(registry: &ToolRegistry) {
    registry.register(bash::BashTool::default());
    registry.register(file_read::FileReadTool);
    registry.register(file_write::FileWriteTool);
    registry.register(glob::GlobTool);
    registry.register(grep::GrepTool);
    registry.register(web_fetch::WebFetchTool::default());
    registry.register(web_search_intrernal::WebSearchTool);
    registry.register(discover_tools::DiscoverToolsTool);
    registry.register(enable_tools::EnableToolsTool);
    registry.register(list_agents::ListAgentsTool);
    registry.register(list_tools::ListToolsTool);
    registry.register(call_agents::CallAgentsTool);
}

pub fn register_skill_tools(registry: &ToolRegistry, index: Arc<SkillIndex>) {
    registry.register(list_skills::ListSkillsTool::new(index.clone()));
    registry.register(load_skill::LoadSkillTool::new(index));
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
    }
}
