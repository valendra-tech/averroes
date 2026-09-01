use crate::tool::ToolRegistry;
use minijinja::{context, Environment};

pub mod instructions;

pub use instructions::ProjectInstructions;

pub struct PromptBuilder {
    env: Environment<'static>,
}

impl PromptBuilder {
    pub fn new() -> Self {
        let mut env = Environment::new();
        let template = include_str!("templates/system.md");
        env.add_template("system", template).ok();
        Self { env }
    }

    pub fn build_system(
        &self,
        tools: &ToolRegistry,
        enabled_tools: &[String],
        working_dir: &str,
        project_instructions: Option<&str>,
    ) -> String {
        let tool_list: Vec<String> = enabled_tools
            .iter()
            .map(|name| {
                let desc = tools
                    .get(name)
                    .map(|t| t.description().to_string())
                    .unwrap_or_default();
                format!("- **{}**: {}", name, desc)
            })
            .collect();

        let tmpl = self.env.get_template("system").unwrap();
        tmpl.render(context! {
            tools => tool_list,
            working_dir => working_dir,
            os => std::env::consts::OS,
            shell => "sh",
            project_instructions => project_instructions.unwrap_or_default(),
        })
        .unwrap_or_else(|e| format!("System prompt error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_project_instructions_when_available() {
        let builder = PromptBuilder::new();
        let tools = ToolRegistry::new();

        let prompt = builder.build_system(
            &tools,
            &[],
            "/tmp/workspace",
            Some("Use the workspace conventions."),
        );

        assert!(prompt.contains("## Project Instructions"));
        assert!(prompt.contains("Use the workspace conventions."));
    }

    #[test]
    fn omits_project_section_without_instructions() {
        let builder = PromptBuilder::new();
        let tools = ToolRegistry::new();

        let prompt = builder.build_system(&tools, &[], "/tmp/workspace", None);

        assert!(!prompt.contains("## Project Instructions"));
        assert!(prompt.contains("discover and enable `create_global_memory`"));
        assert!(prompt.contains("Strict global-memory protocol"));
        assert!(prompt.contains("Never claim, imply, or promise"));
        assert!(prompt.contains("search_deep_memory"));
        assert!(prompt.contains("Deep-memory retrieval protocol"));
        assert!(prompt.contains("Use `discover_tools`"));
        assert!(prompt.contains("discover `list_agents` and `call_agents`"));
        assert!(prompt.contains("Internet research delegation"));
        assert!(prompt.contains("one independent delegated agent per topic"));
        assert!(prompt.contains("must never launch another subagent"));
        assert!(prompt.contains("Context management is automatic"));
        assert!(!prompt.contains("compact_conversation"));
    }
}
