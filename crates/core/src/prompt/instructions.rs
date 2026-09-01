use crate::observability::diagnostics::{self, DiagnosticLevel};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const INSTRUCTIONS_FILE_NAME: &str = "AGENTS.md";
const MAX_INSTRUCTIONS_BYTES: u64 = 512 * 1024;

/// Instructions discovered for the currently active workspace.
///
/// Files are loaded from the workspace root towards the agent's working
/// directory. More specific files are appended last so they can refine the
/// shared workspace rules without pulling instructions from sibling projects.
#[derive(Debug, Clone, Default)]
pub struct ProjectInstructions {
    files: Vec<PathBuf>,
    content: String,
}

impl ProjectInstructions {
    pub fn load(workspace_root: &Path, working_dir: &Path) -> Self {
        let directories = search_directories(workspace_root, working_dir);
        diagnostics::record(
            DiagnosticLevel::Info,
            "agents.instructions",
            format!(
                "Checking {} workspace {} for {}.",
                directories.len(),
                if directories.len() == 1 {
                    "directory"
                } else {
                    "directories"
                },
                INSTRUCTIONS_FILE_NAME
            ),
        );

        let mut instructions = Self::default();
        for directory in directories {
            let Some(path) = find_instructions_file(&directory) else {
                continue;
            };

            match read_instructions(&path) {
                Ok(content) => {
                    diagnostics::record(
                        DiagnosticLevel::Success,
                        "agents.instructions",
                        format!("Loaded {} ({} bytes).", path.display(), content.len()),
                    );
                    instructions.files.push(path.clone());
                    append_file(&mut instructions.content, &path, workspace_root, &content);
                }
                Err(error) => {
                    diagnostics::record(
                        DiagnosticLevel::Warning,
                        "agents.instructions",
                        format!("Could not read {}: {error}.", path.display()),
                    );
                }
            }
        }

        if instructions.files.is_empty() {
            diagnostics::record(
                DiagnosticLevel::Info,
                "agents.instructions",
                format!(
                    "No {} found for workspace {}.",
                    INSTRUCTIONS_FILE_NAME,
                    workspace_root.display()
                ),
            );
        } else {
            diagnostics::record(
                DiagnosticLevel::Success,
                "agents.instructions",
                format!(
                    "Workspace instructions ready with {} file(s).",
                    instructions.files.len()
                ),
            );
        }

        instructions
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }

    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }
}

fn search_directories(workspace_root: &Path, working_dir: &Path) -> Vec<PathBuf> {
    let working_dir = if working_dir.starts_with(workspace_root) {
        working_dir
    } else {
        workspace_root
    };

    let mut directories = Vec::new();
    let mut current = Some(working_dir);
    while let Some(directory) = current {
        directories.push(directory.to_path_buf());
        if directory == workspace_root {
            break;
        }
        current = directory
            .parent()
            .filter(|parent| parent.starts_with(workspace_root));
    }

    if directories
        .last()
        .is_none_or(|directory| directory != workspace_root)
    {
        directories.push(workspace_root.to_path_buf());
    }
    directories.reverse();
    directories
}

fn find_instructions_file(directory: &Path) -> Option<PathBuf> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            diagnostics::record(
                DiagnosticLevel::Warning,
                "agents.instructions",
                format!("Could not scan {}: {error}.", directory.display()),
            );
            return None;
        }
    };

    let mut matches = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .eq_ignore_ascii_case(INSTRUCTIONS_FILE_NAME)
                })
        })
        .collect::<Vec<_>>();
    matches.sort();

    if matches.len() > 1 {
        diagnostics::record(
            DiagnosticLevel::Warning,
            "agents.instructions",
            format!(
                "Multiple case variants of {} found in {}; using {}.",
                INSTRUCTIONS_FILE_NAME,
                directory.display(),
                matches[0].display()
            ),
        );
    }

    matches.into_iter().next()
}

fn read_instructions(path: &Path) -> io::Result<String> {
    let size = fs::metadata(path)?.len();
    if size > MAX_INSTRUCTIONS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "file is too large ({} bytes; limit is {} bytes)",
                size, MAX_INSTRUCTIONS_BYTES
            ),
        ));
    }
    fs::read_to_string(path)
}

fn append_file(content: &mut String, path: &Path, workspace_root: &Path, file_content: &str) {
    if !content.is_empty() {
        content.push('\n');
    }
    let display_path = path.strip_prefix(workspace_root).unwrap_or(path);
    content.push_str("### ");
    content.push_str(&display_path.display().to_string());
    content.push_str("\n\n");
    content.push_str(file_content.trim());
    content.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_root_and_nested_instructions_in_precedence_order() {
        let directory = tempfile::tempdir().expect("temporary workspace");
        let nested = directory.path().join("src");
        fs::create_dir_all(&nested).expect("nested directory");
        fs::write(directory.path().join("AGENTS.md"), "root rules").expect("root instructions");
        fs::write(nested.join("agents.md"), "nested rules").expect("nested instructions");

        let instructions = ProjectInstructions::load(directory.path(), &nested);

        assert_eq!(instructions.files().len(), 2);
        assert!(
            instructions.content().find("root rules") < instructions.content().find("nested rules")
        );
    }

    #[test]
    fn ignores_missing_instruction_files() {
        let directory = tempfile::tempdir().expect("temporary workspace");

        let instructions = ProjectInstructions::load(directory.path(), directory.path());

        assert!(instructions.is_empty());
        assert!(instructions.files().is_empty());
    }
}
