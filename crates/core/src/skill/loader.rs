use super::{Result, SkillError, SkillMeta};
use crate::observability::diagnostics::{self, DiagnosticLevel};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct SkillLoader {
    skill_dirs: Vec<PathBuf>,
}

impl SkillLoader {
    pub fn new(skill_dirs: Vec<PathBuf>) -> Self {
        Self { skill_dirs }
    }

    pub fn add_dir(&mut self, path: PathBuf) {
        self.skill_dirs.push(path);
    }

    pub fn discover_skills(&self) -> Result<Vec<SkillMeta>> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.loader",
            format!(
                "Starting skill discovery across {} configured path(s).",
                self.skill_dirs.len()
            ),
        );
        let mut skills = Vec::new();
        let mut scanned_dirs = 0;
        for dir in &self.skill_dirs {
            if !dir.is_dir() {
                diagnostics::record(
                    DiagnosticLevel::Warning,
                    "skills.loader",
                    format!(
                        "Skipping skill path; it is missing or not a directory: {}.",
                        dir.display()
                    ),
                );
                continue;
            }
            scanned_dirs += 1;
            let before = skills.len();
            diagnostics::record(
                DiagnosticLevel::Info,
                "skills.loader",
                format!("Scanning skill directory: {}.", dir.display()),
            );
            Self::scan_dir(dir, &mut skills)?;
            diagnostics::record(
                DiagnosticLevel::Success,
                "skills.loader",
                format!(
                    "Finished skill directory {}; found {} skill(s).",
                    dir.display(),
                    skills.len().saturating_sub(before)
                ),
            );
        }
        diagnostics::record(
            DiagnosticLevel::Success,
            "skills.loader",
            format!(
                "Skill discovery complete: {} skill(s) found in {} scanned directorie(s).",
                skills.len(),
                scanned_dirs
            ),
        );
        Ok(skills)
    }

    fn scan_dir(dir: &Path, skills: &mut Vec<SkillMeta>) -> Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(source) => {
                diagnostics::record(
                    DiagnosticLevel::Error,
                    "skills.loader",
                    format!(
                        "Could not read skill directory {}: {source}.",
                        dir.display()
                    ),
                );
                return Err(SkillError::Io {
                    path: dir.to_path_buf(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(source) => {
                    diagnostics::record(
                        DiagnosticLevel::Error,
                        "skills.loader",
                        format!(
                            "Could not inspect an entry in skill directory {}: {source}.",
                            dir.display()
                        ),
                    );
                    return Err(SkillError::Io {
                        path: dir.to_path_buf(),
                        source,
                    });
                }
            };
            let path = entry.path();
            if path.is_dir() {
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    match parse_skill_file(&skill_md) {
                        Ok(skill) => {
                            diagnostics::record(
                                DiagnosticLevel::Success,
                                "skills.loader",
                                format!(
                                    "Discovered skill '{}' ({} trigger(s)) from {}.",
                                    skill.name,
                                    skill.triggers.len(),
                                    skill_md.display()
                                ),
                            );
                            skills.push(skill);
                        }
                        Err(SkillError::Parse(error)) => {
                            diagnostics::record(
                                DiagnosticLevel::Warning,
                                "skills.loader",
                                format!("Skipping skill file {}: {error}.", skill_md.display()),
                            );
                        }
                        Err(error) => {
                            diagnostics::record(
                                DiagnosticLevel::Error,
                                "skills.loader",
                                format!(
                                    "Failed to parse skill file {}: {error}.",
                                    skill_md.display()
                                ),
                            );
                            return Err(error);
                        }
                    }
                } else {
                    Self::scan_dir(&path, skills)?;
                }
            } else if is_skill_markdown(&path) {
                match parse_skill_file(&path) {
                    Ok(skill) => {
                        diagnostics::record(
                            DiagnosticLevel::Success,
                            "skills.loader",
                            format!(
                                "Discovered skill '{}' ({} trigger(s)) from {}.",
                                skill.name,
                                skill.triggers.len(),
                                path.display()
                            ),
                        );
                        skills.push(skill);
                    }
                    Err(SkillError::Parse(error)) => {
                        diagnostics::record(
                            DiagnosticLevel::Warning,
                            "skills.loader",
                            format!("Skipping skill file {}: {error}.", path.display()),
                        );
                    }
                    Err(error) => {
                        diagnostics::record(
                            DiagnosticLevel::Error,
                            "skills.loader",
                            format!("Failed to parse skill file {}: {error}.", path.display()),
                        );
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn load_content(&self, meta: &SkillMeta) -> Result<String> {
        diagnostics::record(
            DiagnosticLevel::Info,
            "skills.loader",
            format!(
                "Loading full content for skill '{}' from {}.",
                meta.name,
                meta.path.display()
            ),
        );
        let mut file = match fs::File::open(&meta.path) {
            Ok(file) => file,
            Err(source) => {
                diagnostics::record(
                    DiagnosticLevel::Error,
                    "skills.loader",
                    format!(
                        "Could not open skill '{}' at {}: {source}.",
                        meta.name,
                        meta.path.display()
                    ),
                );
                return Err(SkillError::Io {
                    path: meta.path.clone(),
                    source,
                });
            }
        };
        let mut content = String::new();
        if let Err(source) = file.read_to_string(&mut content) {
            diagnostics::record(
                DiagnosticLevel::Error,
                "skills.loader",
                format!(
                    "Could not read skill '{}' at {}: {source}.",
                    meta.name,
                    meta.path.display()
                ),
            );
            return Err(SkillError::Io {
                path: meta.path.clone(),
                source,
            });
        }
        diagnostics::record(
            DiagnosticLevel::Success,
            "skills.loader",
            format!(
                "Loaded skill '{}' successfully ({} bytes).",
                meta.name,
                content.len()
            ),
        );
        Ok(content)
    }
}

fn is_skill_markdown(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("md")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.eq_ignore_ascii_case("README.md"))
}

fn parse_skill_file(path: &Path) -> Result<SkillMeta> {
    let content = fs::read_to_string(path).map_err(|e| SkillError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let (name, description) = extract_frontmatter(&content).unwrap_or_else(|| {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| *s != "SKILL")
            .or_else(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
            })
            .unwrap_or("unknown")
            .to_string();
        let description = extract_first_heading(&content).unwrap_or_default();
        (name, description)
    });

    let triggers = extract_trigger_patterns(&content);

    Ok(SkillMeta {
        name,
        description,
        triggers,
        path: path.to_path_buf(),
    })
}

fn extract_frontmatter(content: &str) -> Option<(String, String)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }
    let rest = &content[3..];
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let mut name = String::new();
    let mut description = String::new();
    for line in fm.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:").map(str::trim) {
            name = val.to_string();
        } else if let Some(val) = line.strip_prefix("description:").map(str::trim) {
            description = val.to_string();
        }
    }
    if name.is_empty() {
        return None;
    }
    Some((name, description))
}

fn extract_first_heading(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# ") {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn extract_trigger_patterns(content: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_triggers_section = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "## Triggers" {
            in_triggers_section = true;
            continue;
        }
        if in_triggers_section {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("## ") {
                break;
            }
            if let Some(item) = trimmed.strip_prefix("- ") {
                if !item.is_empty() {
                    patterns.push(item.to_string());
                }
            }
        }
    }

    patterns
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_temp_skill_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_discover_and_load_skills() {
        let dir = std::env::temp_dir().join("averroes-skill-test-loader");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let content =
            "# Test Skill\n\nA skill for testing.\n\n## Triggers\n\n- test\n- verify\n- check\n";
        create_temp_skill_file(&dir, "test-skill.md", content);

        let loader = SkillLoader::new(vec![dir.clone()]);
        let skills = loader.discover_skills().unwrap();
        assert_eq!(skills.len(), 1);

        let skill = &skills[0];
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "Test Skill");
        assert_eq!(skill.triggers.len(), 3);
        assert!(skill.triggers.contains(&"test".to_string()));
        assert!(skill.triggers.contains(&"verify".to_string()));
        assert!(skill.triggers.contains(&"check".to_string()));

        let loaded = loader.load_content(skill).unwrap();
        assert!(loaded.contains("Test Skill"));
        assert!(loaded.contains("## Triggers"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_readme_in_a_skill_directory() {
        let dir = std::env::temp_dir().join("averroes-skill-test-readme");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_temp_skill_file(&dir, "README.md", "# Skill documentation");
        create_temp_skill_file(&dir, "useful.md", "# Useful skill");

        let skills = SkillLoader::new(vec![dir.clone()])
            .discover_skills()
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "useful");

        let _ = fs::remove_dir_all(&dir);
    }
}
