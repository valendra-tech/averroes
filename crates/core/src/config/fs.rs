use super::ConfigError;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

pub(crate) fn create_private_dir(path: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| ConfigError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }

    Ok(())
}

pub(crate) fn atomic_private_write(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::InvalidPath(path.into()))?;
    create_private_dir(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ConfigError::InvalidPath(path.into()))?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|source| ConfigError::Io {
            path: temporary.clone(),
            source,
        })?;
        file.write_all(contents)
            .and_then(|_| file.sync_all())
            .map_err(|source| ConfigError::Io {
                path: temporary.clone(),
                source,
            })?;
        std::fs::rename(&temporary, path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |source| ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                },
            )?;
        }

        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
