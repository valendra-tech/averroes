use chrono::{DateTime, Local};
use minijinja::{context, Environment};

pub mod instructions;

pub use instructions::ProjectInstructions;

pub struct PromptBuilder {
    env: Environment<'static>,
}

struct EnvironmentTime {
    date: String,
    time: String,
    time_zone: String,
}

impl EnvironmentTime {
    fn now() -> Self {
        Self::from_datetime(Local::now())
    }

    fn from_datetime(now: DateTime<Local>) -> Self {
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            time: now.format("%H:%M:%S").to_string(),
            time_zone: format!("{} ({})", now.format("%Z"), now.format("%:z")),
        }
    }
}

fn replace_environment_field(prompt: &mut String, label: &str, value: &str) {
    let prefix = format!("- **{label}**: ");
    let Some(value_start) = prompt.find(&prefix).map(|start| start + prefix.len()) else {
        return;
    };
    let value_end = prompt[value_start..]
        .find('\n')
        .map(|offset| value_start + offset)
        .unwrap_or(prompt.len());
    prompt.replace_range(value_start..value_end, value);
}

fn replace_environment_time(prompt: &mut String, environment_time: &EnvironmentTime) {
    replace_environment_field(prompt, "Current Date", &environment_time.date);
    replace_environment_field(prompt, "Current Time", &environment_time.time);
    replace_environment_field(prompt, "Time Zone", &environment_time.time_zone);
}

pub(crate) fn refresh_system_environment_time(prompt: &mut String) {
    replace_environment_time(prompt, &EnvironmentTime::now());
}

pub(crate) fn refresh_system_working_directory(prompt: &mut String, working_dir: &str) {
    replace_environment_field(prompt, "Working Directory", working_dir);
}

impl PromptBuilder {
    pub fn new() -> Self {
        let mut env = Environment::new();
        let template = include_str!("templates/system.md");
        env.add_template("system", template).ok();
        Self { env }
    }

    pub fn build_system(&self, working_dir: &str, project_instructions: Option<&str>) -> String {
        let environment_time = EnvironmentTime::now();
        let tmpl = self.env.get_template("system").unwrap();
        tmpl.render(context! {
            working_dir => working_dir,
            os => std::env::consts::OS,
            shell => "sh",
            current_date => environment_time.date,
            current_time => environment_time.time,
            time_zone => environment_time.time_zone,
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

        let prompt = builder.build_system("/tmp/workspace", Some("Use the workspace conventions."));

        assert!(prompt.contains("## Project Instructions"));
        assert!(prompt.contains("Use the workspace conventions."));
    }

    #[test]
    fn renders_current_date_time_and_time_zone() {
        let builder = PromptBuilder::new();

        let prompt = builder.build_system("/tmp/workspace", None);

        assert!(prompt.contains("- **Current Date**: "));
        assert!(prompt.contains("- **Current Time**: "));
        assert!(prompt.contains("- **Time Zone**: "));
        assert!(!prompt.contains("{{ current_date }}"));
        assert!(!prompt.contains("{{ current_time }}"));
        assert!(!prompt.contains("{{ time_zone }}"));
    }

    #[test]
    fn refreshes_environment_time_in_an_existing_prompt() {
        let mut prompt = concat!(
            "## Environment\n",
            "- **Current Date**: 2025-01-01\n",
            "- **Current Time**: 01:02:03\n",
            "- **Time Zone**: UTC (+00:00)\n",
        )
        .to_string();
        let values = EnvironmentTime {
            date: "2026-09-02".into(),
            time: "14:35:12".into(),
            time_zone: "CEST (+02:00)".into(),
        };

        replace_environment_time(&mut prompt, &values);

        assert!(prompt.contains("- **Current Date**: 2026-09-02"));
        assert!(prompt.contains("- **Current Time**: 14:35:12"));
        assert!(prompt.contains("- **Time Zone**: CEST (+02:00)"));
        assert!(!prompt.contains("2025-01-01"));
    }

    #[test]
    fn refreshes_working_directory_in_an_existing_prompt() {
        let builder = PromptBuilder::new();
        let mut prompt = builder.build_system("/tmp/old", None);

        refresh_system_working_directory(&mut prompt, "/tmp/new");

        assert!(prompt.contains("- **Working Directory**: /tmp/new"));
        assert!(!prompt.contains("- **Working Directory**: /tmp/old"));
    }

    #[test]
    fn omits_project_section_without_instructions() {
        let builder = PromptBuilder::new();

        let prompt = builder.build_system("/tmp/workspace", None);

        assert!(!prompt.contains("## Project Instructions"));
        assert!(prompt.contains("call `create_global_memory` immediately"));
        assert!(prompt.contains("Strict global-memory protocol"));
        assert!(prompt.contains("Detect first, ask second, save third"));
        assert!(prompt.contains("Never claim, imply, or promise"));
        assert!(prompt.contains("search_deep_memory"));
        assert!(prompt.contains("Deep-memory retrieval protocol"));
        assert!(!prompt.contains("discover_tools"));
        assert!(!prompt.contains("enable_tools"));
        assert!(!prompt.contains("list_tools"));
        assert!(prompt.contains("`list_agents` when you need to choose"));
        assert!(prompt.contains("Internet research delegation"));
        assert!(prompt.contains("Use `web_fetch` first"));
        assert!(prompt.contains("Use\n`browser` only"));
        assert!(prompt.contains("one independent delegated agent for the request by default"));
        assert!(prompt.contains("must never launch another subagent"));
        assert!(prompt.contains("Context management is automatic"));
        assert!(!prompt.contains("compact_conversation"));
    }
}
