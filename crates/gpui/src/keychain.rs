use averroes_core::credentials::{VaultError, VaultKeyProvider};
use zeroize::Zeroizing;

const SERVICE: &str = "com.valendra.averroes";
const ACCOUNT: &str = "provider-vault-key-v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct MacKeychainKeyProvider;

#[cfg(target_os = "macos")]
impl VaultKeyProvider for MacKeychainKeyProvider {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
        use security_framework::passwords::get_generic_password;
        use security_framework_sys::base::errSecItemNotFound;

        match get_generic_password(SERVICE, ACCOUNT) {
            Ok(key) => Ok(Some(Zeroizing::new(key))),
            Err(error) if error.code() == errSecItemNotFound => Ok(None),
            Err(error) => Err(VaultError::KeyProvider(error.to_string())),
        }
    }

    fn save(&self, key: &[u8]) -> Result<(), VaultError> {
        security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key)
            .map_err(|error| VaultError::KeyProvider(error.to_string()))
    }

    fn delete(&self) -> Result<(), VaultError> {
        use security_framework::passwords::delete_generic_password;
        use security_framework_sys::base::errSecItemNotFound;

        match delete_generic_password(SERVICE, ACCOUNT) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == errSecItemNotFound => Ok(()),
            Err(error) => Err(VaultError::KeyProvider(error.to_string())),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl VaultKeyProvider for MacKeychainKeyProvider {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
        Err(VaultError::UnsupportedPlatform)
    }

    fn save(&self, _key: &[u8]) -> Result<(), VaultError> {
        Err(VaultError::UnsupportedPlatform)
    }

    fn delete(&self) -> Result<(), VaultError> {
        Err(VaultError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_identity_is_stable_and_contains_no_provider_secret() {
        assert_eq!(SERVICE, "com.valendra.averroes");
        assert_eq!(ACCOUNT, "provider-vault-key-v1");
        assert!(!SERVICE.contains("openai"));
        assert!(!SERVICE.contains("anthropic"));
    }
}
