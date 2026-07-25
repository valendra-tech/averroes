pub mod bash;
pub mod file_read;
pub mod file_write;
pub mod glob;
pub mod grep;
pub mod web_fetch;
pub mod list_skills;
pub mod load_skill;

use crate::skill::SkillIndex;
use crate::tool::ToolRegistry;
use std::sync::Arc;

pub fn register_all(registry: &ToolRegistry) {
    registry.register(bash::BashTool);
    registry.register(file_read::FileReadTool);
    registry.register(file_write::FileWriteTool);
    registry.register(glob::GlobTool);
    registry.register(grep::GrepTool);
    registry.register(web_fetch::WebFetchTool);
}

pub fn register_skill_tools(registry: &ToolRegistry, index: Arc<SkillIndex>) {
    registry.register(list_skills::ListSkillsTool::new(index.clone()));
    registry.register(load_skill::LoadSkillTool::new(index));
}
