use super::scheduled::{ScheduledTask, ScheduledTaskSchedule};
use crate::config::create_private_dir;
use std::path::{Path, PathBuf};
use std::process::Command;

const LABEL_PREFIX: &str = "com.valendra.averroes.scheduled.";

#[derive(Debug, Clone)]
pub struct LaunchdManager {
    launch_agents_dir: PathBuf,
    executable: PathBuf,
    logs_dir: PathBuf,
}

impl LaunchdManager {
    pub fn discover() -> Result<Self, LaunchdError> {
        let home = dirs::home_dir().ok_or_else(|| LaunchdError::HomeUnavailable)?;
        let executable =
            std::env::current_exe().map_err(|error| LaunchdError::Io(error.to_string()))?;
        Ok(Self::for_paths(
            home.join("Library/LaunchAgents"),
            executable,
            home.join(".averroes/logs/scheduled"),
        ))
    }

    pub fn for_paths(launch_agents_dir: PathBuf, executable: PathBuf, logs_dir: PathBuf) -> Self {
        Self {
            launch_agents_dir,
            executable,
            logs_dir,
        }
    }

    pub fn label(task_id: &str) -> String {
        format!("{LABEL_PREFIX}{task_id}")
    }

    pub fn plist_path(&self, task_id: &str) -> PathBuf {
        self.launch_agents_dir
            .join(format!("{}.plist", Self::label(task_id)))
    }

    pub fn sync(&self, task: &ScheduledTask) -> Result<(), LaunchdError> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        if task.enabled {
            self.install(task)
        } else {
            self.remove(task)
        }
    }

    pub fn install(&self, task: &ScheduledTask) -> Result<(), LaunchdError> {
        task.validate()
            .map_err(|error| LaunchdError::InvalidTask(error.to_string()))?;
        create_private_dir(&self.launch_agents_dir)
            .map_err(|error| LaunchdError::Io(error.to_string()))?;
        create_private_dir(&self.logs_dir).map_err(|error| LaunchdError::Io(error.to_string()))?;
        let plist = plist_for_task(task, &self.executable, &self.logs_dir);
        let path = self.plist_path(&task.id);
        atomic_write(&path, plist.as_bytes())?;
        self.bootstrap(&path, &task.id)?;
        Ok(())
    }

    pub fn remove(&self, task: &ScheduledTask) -> Result<(), LaunchdError> {
        if !cfg!(target_os = "macos") {
            return Ok(());
        }
        let path = self.plist_path(&task.id);
        let domain = user_domain();
        let plist = path.display().to_string();
        let _ = self.launchctl(&["bootout", &domain, &plist]);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| LaunchdError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn bootstrap(&self, path: &Path, task_id: &str) -> Result<(), LaunchdError> {
        let domain = user_domain();
        let label = Self::label(task_id);
        let plist = path.display().to_string();
        let _ = self.launchctl(&["bootout", &domain, &label]);
        self.launchctl(&["bootstrap", &domain, &plist])
    }

    fn launchctl(&self, args: &[&str]) -> Result<(), LaunchdError> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|error| LaunchdError::Io(error.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        Err(LaunchdError::CommandFailed {
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn user_domain() -> String {
    let uid = std::env::var("UID").unwrap_or_else(|_| "0".into());
    format!("gui/{uid}")
}

fn plist_for_task(task: &ScheduledTask, executable: &Path, logs_dir: &Path) -> String {
    let label = LaunchdManager::label(&task.id);
    let stdout = logs_dir.join(format!("{}.out.log", task.id));
    let stderr = logs_dir.join(format!("{}.err.log", task.id));
    let schedule = match task.schedule {
        ScheduledTaskSchedule::Interval { seconds } => format!(
            "<key>StartInterval</key><integer>{seconds}</integer>"
        ),
        ScheduledTaskSchedule::Daily { hour, minute } => format!(
            "<key>StartCalendarInterval</key><dict><key>Hour</key><integer>{hour}</integer><key>Minute</key><integer>{minute}</integer></dict>"
        ),
        ScheduledTaskSchedule::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "<key>StartCalendarInterval</key><dict><key>Weekday</key><integer>{weekday}</integer><key>Hour</key><integer>{hour}</integer><key>Minute</key><integer>{minute}</integer></dict>"
        ),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--run-scheduled-task</string><string>{}</string></array>{}<key>RunAtLoad</key><false/><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string><key>ProcessType</key><string>Background</string></dict></plist>\n",
        xml_escape(&label),
        xml_escape(&executable.display().to_string()),
        xml_escape(&task.id),
        schedule,
        xml_escape(&stdout.display().to_string()),
        xml_escape(&stderr.display().to_string()),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), LaunchdError> {
    let parent = path
        .parent()
        .ok_or_else(|| LaunchdError::Io("plist has no parent directory".into()))?;
    create_private_dir(parent).map_err(|error| LaunchdError::Io(error.to_string()))?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    std::fs::write(&temporary, contents).map_err(|error| LaunchdError::Io(error.to_string()))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(LaunchdError::Io(error.to_string()));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    #[error("launchd is only available on macOS")]
    UnsupportedPlatform,
    #[error("home directory is unavailable")]
    HomeUnavailable,
    #[error("launchd I/O error: {0}")]
    Io(String),
    #[error("invalid task for launchd: {0}")]
    InvalidTask(String),
    #[error("launchctl command failed ({args:?}): {message}")]
    CommandFailed { args: Vec<String>, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionId, SessionBinding};
    use crate::task::scheduled::{ScheduledTask, ScheduledTaskSchedule};

    fn task() -> ScheduledTask {
        ScheduledTask::new(
            "Nightly <review>",
            "Review & report",
            PathBuf::from("/tmp/workspace"),
            None,
            SessionBinding {
                connection_id: Some(ConnectionId("connection".into())),
                model_id: Some("model".into()),
                ..Default::default()
            },
            ScheduledTaskSchedule::Daily { hour: 9, minute: 5 },
        )
    }

    #[test]
    fn plist_escapes_xml_and_contains_daily_schedule() {
        let task = task();
        let plist = plist_for_task(
            &task,
            Path::new("/Applications/Averroes.app/Contents/MacOS/averroes-gpui"),
            Path::new("/tmp/logs"),
        );
        assert_eq!(
            xml_escape("Nightly <review> & report"),
            "Nightly &lt;review&gt; &amp; report"
        );
        assert!(plist.contains("<key>Hour</key><integer>9</integer>"));
        assert!(plist.contains("--run-scheduled-task"));
    }

    #[test]
    fn launch_agent_path_is_derived_from_the_task_id() {
        let manager = LaunchdManager::for_paths(
            PathBuf::from("/tmp/LaunchAgents"),
            PathBuf::from("/tmp/averroes"),
            PathBuf::from("/tmp/logs"),
        );
        assert_eq!(
            manager.plist_path("nightly"),
            PathBuf::from("/tmp/LaunchAgents/com.valendra.averroes.scheduled.nightly.plist")
        );
    }
}
