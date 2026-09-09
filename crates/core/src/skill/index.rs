use super::{Result, SkillError, SkillMeta};
use crate::observability::diagnostics::{self, DiagnosticLevel};
use crate::skill::loader::SkillLoader;
use std::collections::{HashMap, HashSet};

pub struct SkillIndex {
    skills: Vec<SkillMeta>,
    name_indices: HashMap<String, Vec<usize>>,
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
        let mut skills = Vec::new();
        let mut name_indices = HashMap::<String, Vec<usize>>::new();
        for skill in discovered {
            let name_key = normalize_identifier(&skill.name);
            if let Some(existing) = name_indices.get(&name_key) {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "skills.index",
                    format!(
                        "Duplicate skill name '{}' found at {}; keeping all {} matching entries.",
                        skill.name,
                        skill.path.display(),
                        existing.len() + 1
                    ),
                );
            }
            let index = skills.len();
            skills.push(skill);
            name_indices.entry(name_key).or_default().push(index);
        }
        diagnostics::record(
            DiagnosticLevel::Success,
            "skills.index",
            format!("Skill index ready with {} skill(s).", skills.len()),
        );
        Ok(Self {
            skills,
            name_indices,
            loader,
        })
    }

    pub fn list(&self) -> Vec<&SkillMeta> {
        self.skills.iter().collect()
    }

    pub fn get(&self, name: &str) -> Option<&SkillMeta> {
        self.matching_indices(name)
            .first()
            .and_then(|index| self.skills.get(*index))
    }

    pub fn resolve(&self, name: &str) -> Result<&SkillMeta> {
        let matches = self.matching_indices(name);
        match matches.as_slice() {
            [] => Err(SkillError::NotFound(name.trim().to_string())),
            [index] => Ok(&self.skills[*index]),
            _ => Err(SkillError::Ambiguous {
                name: name.trim().to_string(),
                paths: matches
                    .iter()
                    .map(|index| self.skills[*index].path.clone())
                    .collect(),
            }),
        }
    }

    pub fn find_by_trigger(&self, text: &str) -> Vec<&SkillMeta> {
        self.skills
            .iter()
            .filter(|skill| {
                skill
                    .triggers
                    .iter()
                    .any(|trigger| contains_normalized_phrase(text, trigger))
            })
            .collect()
    }

    pub fn explicit_skill_mentions(&self, text: &str) -> Vec<String> {
        let chars = text.chars().collect::<Vec<_>>();
        let mut mentions = Vec::new();
        let mut seen = HashSet::new();
        let mut index = 0;

        while index < chars.len() {
            if chars[index] != '$' || (index > 0 && is_skill_mention_char(chars[index - 1])) {
                index += 1;
                continue;
            }

            let start = index + 1;
            let mut end = start;
            while end < chars.len() && is_skill_mention_char(chars[end]) {
                end += 1;
            }
            let mention = chars[start..end]
                .iter()
                .collect::<String>()
                .trim_matches(|character: char| ".,;:!?)]}".contains(character))
                .to_string();
            if mention
                .chars()
                .next()
                .is_some_and(|character| character.is_alphabetic())
            {
                let key = normalize_identifier(&mention);
                if seen.insert(key) {
                    mentions.push(mention);
                }
            }
            index = end.max(index + 1);
        }

        mentions
    }

    pub fn search(&self, query: &str) -> Vec<&SkillMeta> {
        let normalized_query = normalize_terms(query);
        let query_terms = normalized_query.split_whitespace().collect::<Vec<_>>();
        if query_terms.is_empty() {
            return self.list();
        }

        let mut matches = self
            .skills
            .iter()
            .enumerate()
            .filter_map(|(index, skill)| {
                skill_search_score(skill, &normalized_query, &query_terms)
                    .map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_index, left_score), (right_index, right_score)| {
            left_score
                .cmp(right_score)
                .then_with(|| {
                    self.skills[*left_index]
                        .name
                        .cmp(&self.skills[*right_index].name)
                })
                .then_with(|| {
                    self.skills[*left_index]
                        .path
                        .cmp(&self.skills[*right_index].path)
                })
        });
        matches
            .into_iter()
            .map(|(index, _)| &self.skills[index])
            .collect()
    }

    /// Finds skills explicitly triggered by the request or named directly by
    /// the user. The latter keeps standard SKILL.md files useful even when
    /// they do not include Averroes' optional `## Triggers` section.
    pub fn find_relevant(&self, text: &str) -> Vec<&SkillMeta> {
        let normalized_text = normalize_terms(text);
        let padded_text = format!(" {normalized_text} ");
        let mut matches = self.find_by_trigger(text);
        for skill in &self.skills {
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
        let meta = match self.resolve(name) {
            Ok(meta) => meta,
            Err(error) => {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "skills.index",
                    format!("Could not resolve requested skill '{name}': {error}."),
                );
                return Err(error);
            }
        };
        self.loader.load_content(meta)
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    fn matching_indices(&self, name: &str) -> Vec<usize> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        if let Some(indices) = self.name_indices.get(trimmed) {
            return indices.clone();
        }
        self.name_indices
            .get(&normalize_identifier(trimmed))
            .cloned()
            .unwrap_or_default()
    }
}

fn is_skill_mention_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
}

fn normalize_identifier(value: &str) -> String {
    normalize_terms(value).replace(' ', "-")
}

fn contains_normalized_phrase(value: &str, phrase: &str) -> bool {
    let value = normalize_terms(value);
    let phrase = normalize_terms(phrase);
    !phrase.is_empty() && format!(" {value} ").contains(&format!(" {phrase} "))
}

fn all_query_terms_match(value: &str, query_terms: &[&str]) -> bool {
    let tokens = normalize_terms(value);
    let tokens = tokens.split_whitespace().collect::<Vec<_>>();
    query_terms
        .iter()
        .all(|term| tokens.iter().any(|token| token.contains(term)))
}

fn skill_search_score(
    skill: &SkillMeta,
    normalized_query: &str,
    query_terms: &[&str],
) -> Option<u8> {
    let name = normalize_terms(&skill.name);
    if name == normalized_query {
        return Some(0);
    }
    if name.starts_with(normalized_query) {
        return Some(1);
    }
    if all_query_terms_match(&name, query_terms) {
        return Some(2);
    }
    if skill
        .triggers
        .iter()
        .any(|trigger| contains_normalized_phrase(trigger, normalized_query))
        || skill
            .triggers
            .iter()
            .any(|trigger| all_query_terms_match(trigger, query_terms))
    {
        return Some(3);
    }
    if all_query_terms_match(&skill.description, query_terms) {
        return Some(4);
    }
    None
}

fn normalize_terms(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character.to_lowercase().collect::<String>()
            } else {
                " ".to_string()
            }
        })
        .collect::<Vec<_>>()
        .concat()
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
    fn test_trigger_matching_uses_word_boundaries() {
        let (dir, index) = setup_temp_skills(
            "trigger-boundary",
            &[("art.md", "# Art\n\n## Triggers\n- art\n")],
        );

        assert!(index.find_by_trigger("article").is_empty());
        assert_eq!(index.find_by_trigger("make art")[0].name, "art");

        cleanup(&dir);
    }

    #[test]
    fn test_explicit_skill_mentions_are_deduplicated_and_strip_punctuation() {
        let (dir, index) =
            setup_temp_skills("explicit-mentions", &[("daily-work.md", "# Daily work\n")]);

        assert_eq!(
            index.explicit_skill_mentions("Use $daily-work, then $daily-work. Ignore $100."),
            vec!["daily-work"]
        );

        cleanup(&dir);
    }

    #[test]
    fn test_unicode_skill_names_resolve_and_parse_explicit_mentions() {
        let (dir, index) = setup_temp_skills("unicode-name", &[("árbol.md", "# Árbol\n")]);

        assert_eq!(index.resolve("ÁRBOL").unwrap().name, "árbol");
        assert_eq!(index.explicit_skill_mentions("Use $ÁRBOL."), vec!["ÁRBOL"]);

        cleanup(&dir);
    }

    #[test]
    fn test_resolve_accepts_case_and_separator_variants() {
        let (dir, index) =
            setup_temp_skills("resolve-normalized", &[("daily-work.md", "# Daily work\n")]);

        assert_eq!(index.resolve("DAILY WORK").unwrap().name, "daily-work");

        cleanup(&dir);
    }

    #[test]
    fn test_resolve_reports_duplicate_names_as_ambiguous() {
        let root = std::env::temp_dir().join("averroes-skill-test-ambiguous");
        let first = root.join("first");
        let second = root.join("second");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("shared.md"), "# First shared\n").unwrap();
        std::fs::write(second.join("shared.md"), "# Second shared\n").unwrap();

        let index = SkillIndex::build(SkillLoader::new(vec![first, second])).unwrap();

        assert!(matches!(
            index.resolve("shared"),
            Err(SkillError::Ambiguous { .. })
        ));

        cleanup(&root);
    }

    #[test]
    fn test_search_ranks_name_trigger_and_description_matches() {
        let (dir, index) = setup_temp_skills(
            "search-ranking",
            &[
                (
                    "pdf.md",
                    "---\nname: pdf\ndescription: Create documents and reports.\n---\n\n# PDF\n",
                ),
                (
                    "git.md",
                    "---\nname: git\ndescription: Version control workflow.\n---\n\n## Triggers\n- commit\n",
                ),
                (
                    "writing.md",
                    "---\nname: writing\ndescription: Create polished documents.\n---\n\n# Writing\n",
                ),
            ],
        );

        assert_eq!(index.search("PDF")[0].name, "pdf");
        assert_eq!(index.search("commit")[0].name, "git");
        assert_eq!(index.search("documents")[0].name, "pdf");

        cleanup(&dir);
    }

    #[test]
    fn test_search_prefers_trigger_matches_over_description_matches() {
        let (dir, index) = setup_temp_skills(
            "search-precedence",
            &[
                (
                    "trigger-only.md",
                    "---\nname: trigger-only\ndescription: General notes.\n---\n\n## Triggers\n- archive\n",
                ),
                (
                    "description-only.md",
                    "---\nname: description-only\ndescription: Archive documents.\n---\n",
                ),
            ],
        );

        assert_eq!(index.search("archive")[0].name, "trigger-only");

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
