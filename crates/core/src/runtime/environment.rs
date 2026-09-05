use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemEnvironment {
    pub shell: PathBuf,
    pub path: String,
    pub variables: HashMap<String, String>,
}

impl SystemEnvironment {
    pub fn detect() -> Self {
        static DETECTED: OnceLock<SystemEnvironment> = OnceLock::new();
        DETECTED.get_or_init(Self::detect_uncached).clone()
    }

    fn detect_uncached() -> Self {
        let shell = env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .filter(|path| path.exists())
            .or_else(default_shell)
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        let inherited = env::vars()
            .filter(|(key, _)| !is_sensitive_variable(key))
            .collect::<HashMap<_, _>>();
        let mut variables = login_shell_environment(&shell).unwrap_or(inherited);
        let path = variables
            .get("PATH")
            .cloned()
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| env::var("PATH").unwrap_or_default());
        variables.insert("SHELL".into(), shell.display().to_string());
        variables.insert("PATH".into(), path.clone());
        Self {
            shell,
            path,
            variables,
        }
    }

    pub fn shell_name(&self) -> &str {
        self.shell
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sh")
    }
}

fn login_shell_environment(shell: &PathBuf) -> Option<HashMap<String, String>> {
    let output = Command::new(shell).args(["-ilc", "env -0"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed = parse_environment_output(&output.stdout);
    (!parsed.is_empty()).then_some(parsed)
}

fn parse_environment_output(output: &[u8]) -> HashMap<String, String> {
    String::from_utf8_lossy(output)
        .split('\0')
        .filter_map(|entry| {
            let entry = entry.rsplit('\n').next()?.trim();
            let (key, value) = entry.split_once('=')?;
            if key.is_empty()
                || key
                    .chars()
                    .any(|character| character.is_whitespace() || character == '=')
                || is_sensitive_variable(key)
            {
                return None;
            }
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn default_shell() -> Option<PathBuf> {
    ["/bin/zsh", "/bin/bash", "/bin/sh"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}

fn is_sensitive_variable(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "API_KEY",
        "APIKEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTHORIZATION",
    ]
    .iter()
    .any(|part| key.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_real_system_shell_and_preserves_path() {
        let environment = SystemEnvironment::detect();
        assert!(environment.shell.is_absolute());
        assert!(!environment.path.is_empty());
        assert_eq!(environment.variables.get("PATH"), Some(&environment.path));
        assert_eq!(
            environment.variables.get("SHELL"),
            Some(&environment.shell.display().to_string())
        );
    }

    #[test]
    fn filters_secrets_from_environment_snapshot() {
        assert!(is_sensitive_variable("OPENAI_API_KEY"));
        assert!(is_sensitive_variable("GITHUB_TOKEN"));
        assert!(!is_sensitive_variable("LANG"));
    }

    #[test]
    fn parses_login_shell_environment_without_startup_noise_or_secrets() {
        let output = b"Welcome\nSHELL=/bin/zsh\0PATH=/custom/bin:/usr/bin\0KEY=value=with=equals\0bad entry\0OPENAI_API_KEY=secret\0";
        let environment = parse_environment_output(output);

        assert_eq!(environment.get("SHELL"), Some(&"/bin/zsh".to_owned()));
        assert_eq!(
            environment.get("PATH"),
            Some(&"/custom/bin:/usr/bin".to_owned())
        );
        assert_eq!(
            environment.get("KEY"),
            Some(&"value=with=equals".to_owned())
        );
        assert!(!environment.contains_key("OPENAI_API_KEY"));
        assert!(!environment.contains_key("bad entry"));
    }
}
