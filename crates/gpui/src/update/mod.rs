use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use std::process::Command;

pub(crate) mod client;
pub(crate) mod release;

pub(crate) use client::UpdateClient;
pub(crate) use release::{
    releases_update, validate_asset_url, Architecture, GithubRelease, UpdateInfo,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateState {
    Idle,
    Checking,
    Available(UpdateInfo),
    Downloading(UpdateInfo),
    ReadyToOpen {
        info: UpdateInfo,
        path: PathBuf,
    },
    Failed {
        info: Option<UpdateInfo>,
        message: String,
    },
}

impl UpdateState {
    pub(crate) fn available(info: UpdateInfo) -> Self {
        Self::Available(info)
    }

    pub(crate) fn begin_download(self) -> Option<(Self, UpdateInfo)> {
        match self {
            Self::Available(info)
            | Self::Failed {
                info: Some(info), ..
            } => Some((Self::Downloading(info.clone()), info)),
            Self::Idle
            | Self::Checking
            | Self::Downloading(_)
            | Self::ReadyToOpen { .. }
            | Self::Failed { info: None, .. } => None,
        }
    }

    pub(crate) fn download_ready(self, path: PathBuf) -> Self {
        match self {
            Self::Downloading(info) => Self::ReadyToOpen { info, path },
            state => state,
        }
    }

    pub(crate) fn download_failed(self, message: impl Into<String>) -> Self {
        let info = match self {
            Self::Available(info) | Self::Downloading(info) | Self::ReadyToOpen { info, .. } => {
                Some(info)
            }
            Self::Failed { info, .. } => info,
            Self::Idle | Self::Checking => None,
        };

        Self::Failed {
            info,
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error("invalid release tag {tag:?}: {source}")]
    InvalidTag {
        tag: String,
        #[source]
        source: semver::Error,
    },

    #[error("invalid GitHub release metadata: {0}")]
    InvalidRelease(String),

    #[error("no DMG asset found for {architecture:?}")]
    MissingDmg { architecture: Architecture },

    #[error("unsafe update URL {url:?}; expected an HTTPS URL in the Averroes GitHub repository")]
    UnsafeUrl { url: String },

    #[error("DMG asset {asset:?} has no SHA-256 digest")]
    MissingDigest { asset: String },

    #[error("DMG asset {asset:?} has an invalid SHA-256 digest")]
    InvalidDigest { asset: String },

    #[error("update client failed: {0}")]
    Client(#[source] reqwest::Error),

    #[error("update request returned HTTP status {status}")]
    HttpStatus { status: reqwest::StatusCode },

    #[error("failed to write update download: {0}")]
    DownloadIo(#[source] std::io::Error),

    #[error("update download response was empty")]
    EmptyDownload,

    #[error("update download size {size} bytes exceeds the {limit} byte limit")]
    DownloadTooLarge { limit: u64, size: u64 },

    #[error("update download size mismatch: expected {expected} bytes, received {actual}")]
    DownloadSizeMismatch { expected: u64, actual: u64 },

    #[error("update download checksum mismatch: expected {expected}, received {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("unsafe update asset name {name:?}")]
    UnsafeAssetName { name: String },

    #[error("installer is unsupported on this platform")]
    UnsupportedPlatform,

    #[error("failed to launch installer: {0}")]
    Installer(#[source] std::io::Error),

    #[error("installer exited with status {status}")]
    InstallerExit { status: std::process::ExitStatus },
}

pub(crate) fn open_installer(path: &Path) -> Result<(), UpdateError> {
    #[cfg(target_os = "macos")]
    {
        let status = installer_command(path)
            .status()
            .map_err(UpdateError::Installer)?;

        if status.success() {
            Ok(())
        } else {
            Err(UpdateError::InstallerExit { status })
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(UpdateError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
fn installer_command(path: &Path) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::{UpdateInfo, UpdateState};
    use semver::Version;
    use std::path::PathBuf;

    fn sample_update_info() -> UpdateInfo {
        UpdateInfo {
            version: Version::new(1, 2, 3),
            tag_name: "v1.2.3".into(),
            release_url: "https://github.com/valendra-tech/averroes/releases/tag/v1.2.3".into(),
            release_notes: "Release notes".into(),
            dmg_url:
                "https://github.com/valendra-tech/averroes/releases/download/v1.2.3/Averroes.dmg"
                    .into(),
            dmg_name: "Averroes.dmg".into(),
            dmg_size: 1,
            dmg_sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        }
    }

    #[test]
    fn available_begins_download_with_update_info() {
        let info = sample_update_info();

        let (state, returned_info) = UpdateState::available(info.clone())
            .begin_download()
            .expect("available update should begin downloading");

        assert_eq!(state, UpdateState::Downloading(info.clone()));
        assert_eq!(returned_info, info);
    }

    #[test]
    fn failed_with_update_info_begins_download() {
        let info = sample_update_info();

        let (state, returned_info) = UpdateState::Failed {
            info: Some(info.clone()),
            message: "previous download failed".into(),
        }
        .begin_download()
        .expect("failed update with info should retry downloading");

        assert_eq!(state, UpdateState::Downloading(info.clone()));
        assert_eq!(returned_info, info);
    }

    #[test]
    fn states_without_downloadable_update_do_not_begin_download() {
        let info = sample_update_info();

        for state in [
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::ReadyToOpen {
                info,
                path: PathBuf::from("/tmp/Averroes.dmg"),
            },
            UpdateState::Failed {
                info: None,
                message: "check failed".into(),
            },
        ] {
            assert_eq!(state.begin_download(), None);
        }
    }

    #[test]
    fn downloading_becomes_ready_to_open() {
        let info = sample_update_info();
        let path = PathBuf::from("/tmp/Averroes.dmg");

        assert_eq!(
            UpdateState::Downloading(info.clone()).download_ready(path.clone()),
            UpdateState::ReadyToOpen { info, path }
        );
    }

    #[test]
    fn download_error_preserves_update_info() {
        let info = sample_update_info();
        let message = "download failed";

        for state in [
            UpdateState::Available(info.clone()),
            UpdateState::Downloading(info.clone()),
            UpdateState::ReadyToOpen {
                info: info.clone(),
                path: PathBuf::from("/tmp/Averroes.dmg"),
            },
            UpdateState::Failed {
                info: Some(info.clone()),
                message: "old failure".into(),
            },
        ] {
            assert_eq!(
                state.download_failed(message),
                UpdateState::Failed {
                    info: Some(info.clone()),
                    message: message.into(),
                }
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installer_command_uses_system_open_without_extra_arguments() {
        let command = super::installer_command(std::path::Path::new("/tmp/averroes-test.dmg"));

        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("/tmp/averroes-test.dmg")]
        );
    }
}
