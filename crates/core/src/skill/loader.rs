use super::{Result, SkillError, SkillMeta};
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
        let mut skills = Vec::new();
        for dir in &self.skill_dirs {
            if !dir.is_dir() {
                continue;
            }
            let entries = fs::read_dir(dir).map_err(|e| SkillError::Io {
                path: dir.clone(),
                source: e,
            })?;
            for entry in entries {
                let entry = entry.map_err(|e| SkillError::Io {
                    path: dir.clone(),
                    source: e,
                })?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("md") {
                    match parse_skill_file(&path) {
                        Ok(skill) => skills.push(skill),
                        Err(SkillError::Parse(_)) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        Ok(skills)
    }

    pub fn load_content(&self, meta: &SkillMeta) -> Result<String> {
        let mut file = fs::File::open(&meta.path).map_err(|e| SkillError::Io {
            path: meta.path.clone(),
            source: e,
        })?;
        let mut content = String::new();
        file.read_to_string(&mut content)
            .map_err(|e| SkillError::Io {
                path: meta.path.clone(),
                source: e,
            })?;
        Ok(content)
    }
}

fn parse_skill_file(path: &Path) -> Result<SkillMeta> {
    let content = fs::read_to_string(path).map_err(|e| SkillError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let description = extract_first_heading(&content).unwrap_or_default();
    let triggers = extract_trigger_patterns(&content);

    Ok(SkillMeta {
        name,
        description,
        triggers,
        path: path.to_path_buf(),
    })
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
}
