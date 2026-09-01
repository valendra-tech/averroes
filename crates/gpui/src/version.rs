pub const APP_VERSION: &str = match option_env!("AVERROES_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub fn normalize_release_version(value: &str) -> Result<semver::Version, semver::Error> {
    let value = value.trim();
    let value = value.strip_prefix('v').unwrap_or(value);
    semver::Version::parse(value)
}

#[cfg(test)]
mod tests {
    use super::normalize_release_version;

    #[test]
    fn normalizes_release_tags() {
        assert_eq!(
            normalize_release_version("v1.2.3").unwrap().to_string(),
            "1.2.3"
        );
        assert_eq!(
            normalize_release_version("1.2.3").unwrap().to_string(),
            "1.2.3"
        );
    }

    #[test]
    fn trims_release_tags() {
        assert_eq!(
            normalize_release_version("  v1.2.3  ").unwrap().to_string(),
            "1.2.3"
        );
    }

    #[test]
    fn preserves_prerelease_and_build_metadata() {
        assert_eq!(
            normalize_release_version("v1.2.3-alpha.1+build.5")
                .unwrap()
                .to_string(),
            "1.2.3-alpha.1+build.5"
        );
    }

    #[test]
    fn rejects_non_semver_release_tags() {
        assert!(normalize_release_version("release-latest").is_err());
        assert!(normalize_release_version("v1.2").is_err());
    }

    #[test]
    fn rejects_invalid_semver_identifiers() {
        for value in [
            "v01.2.3",
            "1.2.3-",
            "1.2.3-alpha..1",
            "1.2.3-01",
            "1.2.3+build..5",
            "1.2.3+",
        ] {
            assert!(
                normalize_release_version(value).is_err(),
                "expected {value:?} to be rejected"
            );
        }
    }
}
