use gpui::{px, svg, Styled, Svg};

/// Returns the local icon for a tool shown in the sources panel.
///
/// Tool names can come from built-ins or integrations, so unknown names use a
/// neutral tool mark instead of making the source row iconless.
pub fn tool_icon(tool_name: &str, size: f32) -> Svg {
    svg()
        .flex_none()
        .size(px(size))
        .path(tool_icon_path(tool_name))
}

fn tool_icon_path(tool_name: &str) -> &'static str {
    match tool_name.to_ascii_lowercase().as_str() {
        "bash" | "shell" | "terminal" => "tools/terminal.svg",
        "file_read" | "read_file" | "read" => "tools/file-read.svg",
        "file_write" | "write_file" | "write" => "tools/file-write.svg",
        "patch" => "tools/file-write.svg",
        "change_directory" => "tools/folder-search.svg",
        "glob" | "find_files" => "tools/folder-search.svg",
        "grep" | "search" => "tools/search.svg",
        "web_fetch" | "web_search_intrernal" | "browser" => "tools/globe.svg",
        "checkpoint" => "tools/checkpoint.svg",
        "task_list" | "add_task" | "mark_task_as_done" => "tools/task.svg",
        "ask_user" => "tools/ask-user.svg",
        "list_tools" => "tools/tool.svg",
        "list_skills" | "search_skills" => "tools/skills.svg",
        "load_skill" | "install_skill" => "tools/skill.svg",
        "compact_conversation" => "tools/tool.svg",
        _ => "tools/tool.svg",
    }
}

#[cfg(test)]
mod tests {
    use super::tool_icon_path;

    #[test]
    fn maps_known_tools_to_distinct_icons() {
        assert_eq!(tool_icon_path("bash"), "tools/terminal.svg");
        assert_eq!(tool_icon_path("file_read"), "tools/file-read.svg");
        assert_eq!(tool_icon_path("file_write"), "tools/file-write.svg");
        assert_eq!(tool_icon_path("patch"), "tools/file-write.svg");
        assert_eq!(
            tool_icon_path("change_directory"),
            "tools/folder-search.svg"
        );
        assert_eq!(tool_icon_path("web_fetch"), "tools/globe.svg");
        assert_eq!(tool_icon_path("browser"), "tools/globe.svg");
        assert_eq!(tool_icon_path("add_task"), "tools/task.svg");
        assert_eq!(tool_icon_path("ask_user"), "tools/ask-user.svg");
        assert_eq!(tool_icon_path("search_skills"), "tools/skills.svg");
        assert_eq!(tool_icon_path("install_skill"), "tools/skill.svg");
        assert_eq!(tool_icon_path("load_skill"), "tools/skill.svg");
    }

    #[test]
    fn keeps_unknown_integration_tools_visible() {
        assert_eq!(tool_icon_path("linear_create_issue"), "tools/tool.svg");
    }
}
