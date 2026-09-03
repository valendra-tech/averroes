use crate::tool::{
    Result, SkillMarketplaceBackend, SkillMarketplaceEntry, Tool, ToolContext, ToolError,
    ToolResult,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct InstallSkillTool {
    marketplace: Arc<dyn SkillMarketplaceBackend>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallSkillParams {
    skill_id: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl InstallSkillTool {
    pub fn new(marketplace: Arc<dyn SkillMarketplaceBackend>) -> Self {
        Self { marketplace }
    }
}

#[async_trait]
impl Tool for InstallSkillTool {
    fn name(&self) -> &str {
        "install_skill"
    }

    fn description(&self) -> &str {
        "Install a skill from the public marketplace into the active workspace's .averroes/skills directory. Use the exact skill_id returned by search_skills."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "skill_id": {
                    "type": "string",
                    "description": "Exact marketplace skill id returned by search_skills, for example owner/repository/skill-name."
                },
                "source": {
                    "type": "string",
                    "description": "Source returned by search_skills. Optional when it can be derived from skill_id."
                },
                "slug": {
                    "type": "string",
                    "description": "Skill slug returned by search_skills. Optional when it can be derived from skill_id."
                },
                "name": {
                    "type": "string",
                    "description": "Optional display name returned by search_skills."
                }
            },
            "required": ["skill_id"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: InstallSkillParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;
        let skill_id = params.skill_id.trim();
        if skill_id.is_empty() {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "skill_id cannot be empty".into(),
            });
        }
        if skill_id.chars().count() > 1_000 {
            return Err(ToolError::InvalidParams {
                tool: self.name().into(),
                message: "skill_id cannot exceed 1,000 characters".into(),
            });
        }

        let (derived_source, derived_slug) =
            split_skill_id(skill_id).ok_or_else(|| ToolError::InvalidParams {
                tool: self.name().into(),
                message: "skill_id must contain a source and skill slug separated by '/'".into(),
            })?;
        validate_optional_identity("source", params.source.as_deref(), &derived_source)?;
        validate_optional_identity("slug", params.slug.as_deref(), &derived_slug)?;
        let source = derived_source;
        let slug = derived_slug;
        let name = params
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(slug.as_str())
            .to_owned();
        let skill = SkillMarketplaceEntry {
            id: skill_id.to_owned(),
            name,
            description: None,
            source,
            slug,
            installs: 0,
            url: None,
        };

        let installed_slug = self
            .marketplace
            .install(&ctx.working_dir, &skill)
            .await
            .map_err(|error| ToolError::Execution {
                tool: self.name().into(),
                message: error,
            })?;
        let path = ctx
            .working_dir
            .join(".averroes")
            .join("skills")
            .join(&installed_slug);
        Ok(ToolResult::ok(format!(
            "Installed skill '{}' in {}. Use load_skill with name '{}' to inspect it.",
            skill.name,
            path.display(),
            installed_slug,
        ))
        .with_metadata(json!({
            "skill_id": skill.id,
            "skill": skill.name,
            "slug": installed_slug,
            "path": path,
        })))
    }
}

fn split_skill_id(skill_id: &str) -> Option<(String, String)> {
    let (source, slug) = skill_id.rsplit_once('/')?;
    let source = source.trim();
    let slug = slug.trim();
    if source.is_empty() || slug.is_empty() {
        return None;
    }
    Some((source.to_owned(), slug.to_owned()))
}

fn validate_optional_identity(field: &str, supplied: Option<&str>, expected: &str) -> Result<()> {
    let Some(supplied) = supplied.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if supplied != expected {
        return Err(ToolError::InvalidParams {
            tool: "install_skill".into(),
            message: format!("{field} must match the value derived from skill_id"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{split_skill_id, validate_optional_identity};

    #[test]
    fn splits_repository_skill_ids_at_the_last_segment() {
        assert_eq!(
            split_skill_id("vercel-labs/agent-skills/web-design-guidelines"),
            Some((
                "vercel-labs/agent-skills".into(),
                "web-design-guidelines".into()
            ))
        );
    }

    #[test]
    fn rejects_skill_ids_without_a_source_or_slug() {
        assert!(split_skill_id("skill-only").is_none());
        assert!(split_skill_id("/skill").is_none());
        assert!(split_skill_id("source/").is_none());
    }

    #[test]
    fn rejects_identity_fields_that_do_not_match_the_skill_id() {
        assert!(
            validate_optional_identity("source", Some("other/repository"), "owner/repository")
                .is_err()
        );
        assert!(validate_optional_identity("slug", Some("other-skill"), "skill").is_err());
        assert!(
            validate_optional_identity("source", Some("owner/repository"), "owner/repository")
                .is_ok()
        );
    }
}
