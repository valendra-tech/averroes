use crate::version::APP_VERSION;
use reqwest::header::ACCEPT;
use reqwest::redirect::Policy;
use reqwest::Url;
use semver::Version;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use super::{
    releases_update, validate_asset_url, Architecture, GithubRelease, UpdateError, UpdateInfo,
};

const RELEASES_URL: &str =
    "https://api.github.com/repos/valendra-tech/averroes/releases?per_page=20";
const GITHUB_JSON_ACCEPT: &str = "application/vnd.github+json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DOWNLOAD_SIZE: u64 = 512 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct UpdateClient {
    client: reqwest::Client,
}

impl UpdateClient {
    pub(crate) fn new() -> Result<Self, UpdateError> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Averroes/{APP_VERSION}"))
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::custom(|attempt| {
                if is_trusted_redirect_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .map_err(UpdateError::Client)?;

        Ok(Self { client })
    }

    pub(crate) async fn check(&self, current: &Version) -> Result<Option<UpdateInfo>, UpdateError> {
        let response = self
            .client
            .get(RELEASES_URL)
            .header(ACCEPT, GITHUB_JSON_ACCEPT)
            .send()
            .await
            .map_err(UpdateError::Client)?;
        let response = ensure_success(response)?;
        let releases = response
            .json::<Vec<GithubRelease>>()
            .await
            .map_err(UpdateError::Client)?;

        releases_update(current, releases, Architecture::current())
    }

    pub(crate) async fn download(&self, info: &UpdateInfo) -> Result<PathBuf, UpdateError> {
        let url = validate_asset_url(&info.dmg_url)?;
        let asset_name = controlled_asset_name(&info.dmg_name)?;
        let path = std::env::temp_dir().join(format!(
            "averroes-update-{}-{asset_name}",
            Uuid::new_v4().simple()
        ));

        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(UpdateError::Client)
            .and_then(ensure_success)?;
        validate_content_length(response.content_length())?;

        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(UpdateError::DownloadIo)?;
        let mut temp_file = TempDownload::new(path, file);
        let mut bytes_written = 0;
        let mut hasher = Sha256::new();

        if let Some(content_length) = response.content_length() {
            if content_length != info.dmg_size {
                return Err(UpdateError::DownloadSizeMismatch {
                    expected: info.dmg_size,
                    actual: content_length,
                });
            }
        }

        let result = async {
            while let Some(chunk) = response.chunk().await.map_err(UpdateError::Client)? {
                if chunk.is_empty() {
                    continue;
                }

                let next_size = checked_download_size(bytes_written, chunk.len())?;
                temp_file
                    .write_all(&chunk)
                    .await
                    .map_err(UpdateError::DownloadIo)?;
                hasher.update(&chunk);
                bytes_written = next_size;
            }

            if bytes_written == 0 {
                return Err(UpdateError::EmptyDownload);
            }
            if bytes_written != info.dmg_size {
                return Err(UpdateError::DownloadSizeMismatch {
                    expected: info.dmg_size,
                    actual: bytes_written,
                });
            }

            let actual_digest = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if actual_digest != info.dmg_sha256 {
                return Err(UpdateError::ChecksumMismatch {
                    expected: info.dmg_sha256.clone(),
                    actual: actual_digest,
                });
            }

            temp_file.flush().await.map_err(UpdateError::DownloadIo)?;
            Ok::<_, UpdateError>(())
        }
        .await;

        match result {
            Ok(()) => Ok(temp_file.persist()),
            Err(error) => Err(error),
        }
    }
}

struct TempDownload {
    path: PathBuf,
    file: Option<tokio::fs::File>,
    persist: bool,
}

impl TempDownload {
    fn new(path: PathBuf, file: tokio::fs::File) -> Self {
        Self {
            path,
            file: Some(file),
            persist: false,
        }
    }

    async fn write_all(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        self.file
            .as_mut()
            .expect("temporary download file must remain open")
            .write_all(chunk)
            .await
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.file
            .as_mut()
            .expect("temporary download file must remain open")
            .flush()
            .await
    }

    fn persist(mut self) -> PathBuf {
        self.persist = true;
        drop(self.file.take());
        self.path.clone()
    }
}

impl Drop for TempDownload {
    fn drop(&mut self) {
        if !self.persist {
            drop(self.file.take());
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn ensure_success(response: reqwest::Response) -> Result<reqwest::Response, UpdateError> {
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else {
        Err(UpdateError::HttpStatus { status })
    }
}

fn controlled_asset_name(name: &str) -> Result<String, UpdateError> {
    let name = name.trim();
    let unsafe_name = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control);

    if unsafe_name {
        Err(UpdateError::UnsafeAssetName {
            name: name.to_owned(),
        })
    } else {
        Ok(name.to_owned())
    }
}

fn is_trusted_redirect_url(url: &Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(is_trusted_github_host)
}

fn is_trusted_github_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "github.com"
        || host.ends_with(".github.com")
        || host == "githubusercontent.com"
        || host.ends_with(".githubusercontent.com")
}

fn validate_content_length(content_length: Option<u64>) -> Result<(), UpdateError> {
    if let Some(size) = content_length {
        if size > MAX_DOWNLOAD_SIZE {
            return Err(UpdateError::DownloadTooLarge {
                limit: MAX_DOWNLOAD_SIZE,
                size,
            });
        }
    }

    Ok(())
}

fn checked_download_size(bytes_written: u64, chunk_len: usize) -> Result<u64, UpdateError> {
    let size = bytes_written.saturating_add(chunk_len as u64);
    if size > MAX_DOWNLOAD_SIZE {
        Err(UpdateError::DownloadTooLarge {
            limit: MAX_DOWNLOAD_SIZE,
            size,
        })
    } else {
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        checked_download_size, is_trusted_redirect_url, validate_content_length, MAX_DOWNLOAD_SIZE,
    };
    use crate::update::UpdateError;

    #[test]
    fn allows_https_github_hosts_for_redirects() {
        for url in [
            "https://api.github.com/repos/valendra-tech/averroes/releases/latest",
            "https://github.com/valendra-tech/averroes/releases/latest",
            "https://objects.githubusercontent.com/releases/download/update.dmg",
            "https://githubusercontent.com/update.dmg",
        ] {
            assert!(
                is_trusted_redirect_url(&url.parse().unwrap()),
                "expected trusted redirect URL: {url}"
            );
        }
    }

    #[test]
    fn rejects_untrusted_redirect_schemes_and_hosts() {
        for url in [
            "http://api.github.com/releases/latest",
            "https://github.com.example.com/releases/latest",
            "https://evilgithub.com/releases/latest",
            "https://example.com/update.dmg",
        ] {
            assert!(
                !is_trusted_redirect_url(&url.parse().unwrap()),
                "expected untrusted redirect URL: {url}"
            );
        }
    }

    #[test]
    fn accepts_downloads_up_to_the_maximum_size() {
        assert!(matches!(
            validate_content_length(Some(MAX_DOWNLOAD_SIZE)),
            Ok(())
        ));
        assert!(matches!(validate_content_length(None), Ok(())));
        assert!(matches!(
            checked_download_size(MAX_DOWNLOAD_SIZE - 1, 1),
            Ok(size) if size == MAX_DOWNLOAD_SIZE
        ));
    }

    #[test]
    fn rejects_downloads_that_exceed_the_maximum_size() {
        let too_large = MAX_DOWNLOAD_SIZE + 1;

        assert!(matches!(
            validate_content_length(Some(too_large)),
            Err(UpdateError::DownloadTooLarge {
                limit: MAX_DOWNLOAD_SIZE,
                size,
            }) if size == too_large
        ));
        assert!(matches!(
            checked_download_size(MAX_DOWNLOAD_SIZE, 1),
            Err(UpdateError::DownloadTooLarge {
                limit: MAX_DOWNLOAD_SIZE,
                size,
            }) if size == too_large
        ));
    }
}
