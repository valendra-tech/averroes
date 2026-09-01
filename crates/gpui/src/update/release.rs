use crate::version::normalize_release_version;
use reqwest::Url;
use semver::Version;
use serde::Deserialize;

use super::UpdateError;

const RELEASE_REPOSITORY_PATH_PREFIX: &str = "/valendra-tech/averroes/releases/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateInfo {
    pub(crate) version: Version,
    pub(crate) tag_name: String,
    pub(crate) release_url: String,
    pub(crate) release_notes: String,
    pub(crate) dmg_url: String,
    pub(crate) dmg_name: String,
    pub(crate) dmg_size: u64,
    pub(crate) dmg_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Architecture {
    Arm64,
    X86_64,
}

impl Architecture {
    pub(crate) fn current() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Arm64
        } else {
            Self::X86_64
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct GithubRelease {
    pub(crate) tag_name: String,
    pub(crate) html_url: String,
    pub(crate) body: Option<String>,
    pub(crate) draft: bool,
    pub(crate) prerelease: bool,
    pub(crate) assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ReleaseAsset {
    pub(crate) name: String,
    pub(crate) browser_download_url: String,
    pub(crate) size: u64,
    #[serde(default)]
    pub(crate) digest: Option<String>,
}

pub(crate) fn release_update(
    current: &Version,
    release: GithubRelease,
    architecture: Architecture,
) -> Result<Option<UpdateInfo>, UpdateError> {
    if release.draft || release.prerelease {
        return Ok(None);
    }

    let version =
        normalize_release_version(&release.tag_name).map_err(|source| UpdateError::InvalidTag {
            tag: release.tag_name.clone(),
            source,
        })?;

    if !version.pre.is_empty() || version <= *current {
        return Ok(None);
    }

    if release.html_url.trim().is_empty() {
        return Err(UpdateError::InvalidRelease("release URL is empty".into()));
    }
    let release_url = validate_asset_url(&release.html_url)?;

    let asset = select_dmg(&release.assets, architecture)?;
    let dmg_url = validate_asset_url(&asset.browser_download_url)?;
    let dmg_sha256 = normalize_asset_digest(&asset)?;
    if asset.size == 0 {
        return Err(UpdateError::InvalidRelease(format!(
            "DMG asset {:?} has an empty size",
            asset.name
        )));
    }

    Ok(Some(UpdateInfo {
        version,
        tag_name: release.tag_name,
        release_url: release_url.to_string(),
        release_notes: release.body.unwrap_or_default(),
        dmg_url: dmg_url.to_string(),
        dmg_name: asset.name.clone(),
        dmg_size: asset.size,
        dmg_sha256,
    }))
}

pub(crate) fn select_dmg(
    assets: &[ReleaseAsset],
    architecture: Architecture,
) -> Result<ReleaseAsset, UpdateError> {
    let architecture_marker = match architecture {
        Architecture::Arm64 => "arm64",
        Architecture::X86_64 => "x86_64",
    };

    assets
        .iter()
        .find(|asset| {
            let name = asset.name.to_ascii_lowercase();
            name.ends_with(".dmg") && name.contains(architecture_marker)
        })
        .cloned()
        .ok_or(UpdateError::MissingDmg { architecture })
}

pub(crate) fn validate_asset_url(url: &str) -> Result<Url, UpdateError> {
    let parsed = Url::parse(url).map_err(|_| UpdateError::UnsafeUrl {
        url: url.to_owned(),
    })?;

    let safe = parsed.scheme() == "https"
        && parsed.host_str() == Some("github.com")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path().starts_with(RELEASE_REPOSITORY_PATH_PREFIX);

    if safe {
        Ok(parsed)
    } else {
        Err(UpdateError::UnsafeUrl {
            url: url.to_owned(),
        })
    }
}

fn normalize_asset_digest(asset: &ReleaseAsset) -> Result<String, UpdateError> {
    let digest = asset
        .digest
        .as_deref()
        .ok_or_else(|| UpdateError::MissingDigest {
            asset: asset.name.clone(),
        })?;
    let Some((algorithm, value)) = digest.split_once(':') else {
        return Err(UpdateError::InvalidDigest {
            asset: asset.name.clone(),
        });
    };
    if algorithm != "sha256"
        || value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(UpdateError::InvalidDigest {
            asset: asset.name.clone(),
        });
    }

    Ok(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{
        release_update, select_dmg, validate_asset_url, Architecture, GithubRelease, ReleaseAsset,
    };
    use crate::update::UpdateError;
    use semver::Version;

    fn fixture_dmg(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.into(),
            browser_download_url: format!(
                "https://github.com/valendra-tech/averroes/releases/download/v1.4.0/{name}"
            ),
            size: 1,
            digest: Some(
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            ),
        }
    }

    fn fixture_release(
        tag_name: &str,
        draft: bool,
        prerelease: bool,
        assets: Vec<ReleaseAsset>,
    ) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.into(),
            html_url: "https://github.com/valendra-tech/averroes/releases/tag/v1.4.0".into(),
            body: Some("Release notes".into()),
            draft,
            prerelease,
            assets,
        }
    }

    #[test]
    fn newer_stable_release_becomes_available() {
        let release = fixture_release(
            "v1.4.0",
            false,
            false,
            vec![fixture_dmg("Averroes-1.4.0-macos-arm64.dmg")],
        );

        let result = release_update(
            &Version::parse("1.3.0").unwrap(),
            release,
            Architecture::Arm64,
        )
        .unwrap();

        let update = result.expect("newer stable release should be available");
        assert_eq!(update.version, Version::parse("1.4.0").unwrap());
        assert_eq!(update.tag_name, "v1.4.0");
        assert_eq!(
            update.release_url,
            "https://github.com/valendra-tech/averroes/releases/tag/v1.4.0"
        );
        assert_eq!(update.release_notes, "Release notes");
        assert_eq!(update.dmg_name, "Averroes-1.4.0-macos-arm64.dmg");
    }

    #[test]
    fn draft_release_is_ignored() {
        let release = fixture_release(
            "v1.4.0",
            true,
            false,
            vec![fixture_dmg("Averroes-1.4.0-macos-arm64.dmg")],
        );

        assert!(release_update(
            &Version::parse("1.3.0").unwrap(),
            release,
            Architecture::Arm64
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn prerelease_is_ignored() {
        let release = fixture_release(
            "v1.4.0-beta.1",
            false,
            true,
            vec![fixture_dmg("Averroes-1.4.0-beta.1-macos-arm64.dmg")],
        );

        assert!(release_update(
            &Version::parse("1.3.0").unwrap(),
            release,
            Architecture::Arm64
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn older_release_is_ignored() {
        let release = fixture_release(
            "v1.2.0",
            false,
            false,
            vec![fixture_dmg("Averroes-1.2.0-macos-arm64.dmg")],
        );

        assert!(release_update(
            &Version::parse("1.3.0").unwrap(),
            release,
            Architecture::Arm64
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn selects_arm64_dmg() {
        let assets = vec![
            fixture_dmg("Averroes-1.4.0-macos-x86_64.dmg"),
            fixture_dmg("Averroes-1.4.0-macos-arm64.dmg"),
        ];

        let selected: ReleaseAsset = select_dmg(&assets, Architecture::Arm64).unwrap();
        assert_eq!(selected.name, "Averroes-1.4.0-macos-arm64.dmg");
    }

    #[test]
    fn selects_x86_64_dmg() {
        let assets = vec![
            fixture_dmg("Averroes-1.4.0-macos-arm64.dmg"),
            fixture_dmg("Averroes-1.4.0-macos-x86_64.dmg"),
        ];

        let selected: ReleaseAsset = select_dmg(&assets, Architecture::X86_64).unwrap();
        assert_eq!(selected.name, "Averroes-1.4.0-macos-x86_64.dmg");
    }

    #[test]
    fn rejects_http_asset_url() {
        assert!(matches!(
            validate_asset_url(
                "http://github.com/valendra-tech/averroes/releases/download/v1.4.0/app.dmg"
            ),
            Err(UpdateError::UnsafeUrl { .. })
        ));
    }

    #[test]
    fn rejects_assets_from_another_github_repository() {
        assert!(matches!(
            validate_asset_url("https://github.com/other/project/releases/download/v1.4.0/app.dmg"),
            Err(UpdateError::UnsafeUrl { .. })
        ));
    }

    #[test]
    fn rejects_assets_without_a_sha256_digest() {
        let mut release = fixture_release(
            "v1.4.0",
            false,
            false,
            vec![fixture_dmg("Averroes-1.4.0-macos-arm64.dmg")],
        );
        release.assets[0].digest = None;

        assert!(matches!(
            release_update(
                &Version::parse("1.3.0").unwrap(),
                release,
                Architecture::Arm64
            ),
            Err(UpdateError::MissingDigest { .. })
        ));
    }

    #[test]
    fn returns_parsed_github_asset_url() {
        let url = validate_asset_url(
            "https://github.com/valendra-tech/averroes/releases/download/v1.4.0/app.dmg",
        )
        .unwrap();

        assert_eq!(
            url.as_str(),
            "https://github.com/valendra-tech/averroes/releases/download/v1.4.0/app.dmg"
        );
    }

    #[test]
    fn rejects_http_release_url() {
        let mut release = fixture_release(
            "v1.4.0",
            false,
            false,
            vec![fixture_dmg("Averroes-1.4.0-macos-arm64.dmg")],
        );
        release.html_url = "http://github.com/valendra-tech/averroes/releases/tag/v1.4.0".into();

        assert!(matches!(
            release_update(
                &Version::parse("1.3.0").unwrap(),
                release,
                Architecture::Arm64
            ),
            Err(UpdateError::UnsafeUrl { .. })
        ));
    }

    #[test]
    fn rejects_wrong_or_missing_dmg() {
        let wrong_architecture = vec![fixture_dmg("Averroes-1.4.0-macos-x86_64.dmg")];
        assert!(matches!(
            select_dmg(&wrong_architecture, Architecture::Arm64),
            Err(UpdateError::MissingDmg { .. })
        ));

        let non_dmg = vec![fixture_dmg("Averroes-1.4.0-macos-arm64.zip")];
        assert!(matches!(
            select_dmg(&non_dmg, Architecture::Arm64),
            Err(UpdateError::MissingDmg { .. })
        ));

        assert!(matches!(
            select_dmg(&[], Architecture::Arm64),
            Err(UpdateError::MissingDmg { .. })
        ));
    }

    #[test]
    fn rejects_malformed_tag() {
        let release = fixture_release(
            "release-latest",
            false,
            false,
            vec![fixture_dmg("Averroes-1.4.0-macos-arm64.dmg")],
        );

        assert!(matches!(
            release_update(
                &Version::parse("1.3.0").unwrap(),
                release,
                Architecture::Arm64
            ),
            Err(UpdateError::InvalidTag { .. })
        ));
    }
}
