use super::{Result, SkillError, SkillMeta};
use crate::observability::diagnostics::{self, DiagnosticLevel};
use crate::skill::loader::SkillLoader;
use std::collections::HashMap;

pub struct SkillIndex {
    skills: HashMap<String, SkillMeta>,
    loader: SkillLoader,
}

impl SkillIndex {
    pub fn build(loader: SkillLoader) -> Result<Self> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.index",
            "Building the skill index from discovered files.",
        );
        let discovered = loader.discover_skills()?;
        let mut skills = HashMap::new();
        for skill in discovered {
            if skills.contains_key(&skill.name) {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "skills.index",
                    format!(
                        "Duplicate skill name '{}' found at {}; replacing the previous entry.",
                        skill.name,
                        skill.path.display()
                    ),
                );
            }
            skills.insert(skill.name.clone(), skill);
        }
        diagnostics::record(
            DiagnosticLevel::Success,
            "skills.index",
            format!("Skill index ready with {} skill(s).", skills.len()),
        );
        Ok(Self { skills, loader })
    }

    pub fn list(&self) -> Vec<&SkillMeta> {
        let mut skills = self.skills.values().collect::<Vec<_>>();
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        skills
    }

    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.get(name)
    }

    pub fn find_by_trigger(&self, text: &str) -> Vec<&SkillMeta> {
        let lower = text.to_lowercase();
        self.skills
            .values()
            .filter(|s| s.triggers.iter().any(|t| lower.contains(&t.to_lowercase())))
            .collect()
    }

    /// Finds skills explicitly triggered by the request or named directly by
    /// the user. The latter keeps standard SKILL.md files useful even when
    /// they do not include Averroes' optional `## Triggers` section.
    pub fn find_relevant(&self, text: &str) -> Vec<&SkillMeta> {
        let normalized_text = normalize_terms(text);
        let padded_text = format!(" {normalized_text} ");
        let mut matches = self.find_by_trigger(text);
        for skill in self.skills.values() {
            let name = normalize_terms(&skill.name);
            if name.is_empty()
                || !padded_text.contains(&format!(" {name} "))
                || matches.iter().any(|matched| matched.name == skill.name)
            {
                continue;
            }
            matches.push(skill);
        }
        matches.sort_by(|left, right| left.name.cmp(&right.name));
        matches
    }

    pub fn load(&self, name: &str) -> Result<String> {
        let Some(meta) = self.get(name) else {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "skills.index",
                format!("Requested skill '{name}' is not present in the index."),
            );
            return Err(SkillError::NotFound(name.to_string()));
        };
        self.loader.load_content(meta)
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

fn normalize_terms(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn setup_temp_skills(tag: &str, files: &[(&str, &str)]) -> (PathBuf, SkillIndex) {
        let dir = std::env::temp_dir().join(format!("averroes-skill-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for (filename, content) in files {
            let path = dir.join(filename);
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(content.as_bytes()).unwrap();
        }

        let loader = SkillLoader::new(vec![dir.clone()]);
        let index = SkillIndex::build(loader).unwrap();
        (dir, index)
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_list_skills() {
        let (dir, index) = setup_temp_skills(
            "list",
            &[
                (
                    "git.md",
                    "# Git Commands\n\n## Triggers\n- git\n- commit\n- branch\n",
                ),
                (
                    "rust.md",
                    "# Rust Tips\n\n## Triggers\n- rust\n- cargo\n- borrow\n",
                ),
            ],
        );

        let list = index.list();
        assert_eq!(list.len(), 2);

        let mut names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["git", "rust"]);

        cleanup(&dir);
    }

    #[test]
    fn test_find_by_trigger() {
        let (dir, index) = setup_temp_skills(
            "trigger",
            &[
                (
                    "git.md",
                    "# Git Commands\n\n## Triggers\n- git\n- commit\n- branch\n",
                ),
                (
                    "rust.md",
                    "# Rust Tips\n\n## Triggers\n- rust\n- cargo\n- borrow\n",
                ),
                (
                    "testing.md",
                    "# Testing Guide\n\n## Triggers\n- test\n- assert\n- verify\n",
                ),
            ],
        );

        let matches = index.find_by_trigger("rust");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "rust");

        let matches = index.find_by_trigger("test");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "testing");

        let matches = index.find_by_trigger("nonexistent");
        assert_eq!(matches.len(), 0);

        cleanup(&dir);
    }

    #[test]
    fn test_find_by_trigger_partial_does_not_match() {
        let (dir, index) = setup_temp_skills(
            "partial",
            &[
                (
                    "git.md",
                    "# Git Commands\n\n## Triggers\n- git\n- commit\n- branch\n",
                ),
                (
                    "rust.md",
                    "# Rust Tips\n\n## Triggers\n- rust\n- cargo\n- borrow\n",
                ),
            ],
        );

        let matches = index.find_by_trigger("gi");
        assert_eq!(matches.len(), 0);

        let matches = index.find_by_trigger("ru");
        assert_eq!(matches.len(), 0);

        let matches = index.find_by_trigger("it");
        assert_eq!(matches.len(), 0);

        cleanup(&dir);
    }

    #[test]
    fn test_find_relevant_matches_skill_name_without_triggers() {
        let (dir, index) = setup_temp_skills(
            "relevant-name",
            &[("pdf.md", "# PDF workflow\n\nCreate PDFs safely.\n")],
        );

        let matches = index.find_relevant("Please use the PDF skill for this task.");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "pdf");

        cleanup(&dir);
    }

    #[test]
    fn test_load_skill() {
        let (dir, index) = setup_temp_skills(
            "load",
            &[(
                "config.md",
                "# Configuration\n\nSome content here.\n\n## Triggers\n- config\n",
            )],
        );

        let content = index.load("config").unwrap();
        assert!(content.contains("Configuration"));
        assert!(content.contains("Some content here"));

        cleanup(&dir);
    }

    #[test]
    fn test_load_missing_skill() {
        let (dir, index) = setup_temp_skills(
            "missing",
            &[("basic.md", "# Basic\n\n## Triggers\n- basic\n")],
        );

        let result = index.load("nonexistent");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SkillError::NotFound(_)));

        cleanup(&dir);
    }
}
