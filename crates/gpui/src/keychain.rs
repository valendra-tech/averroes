use averroes_core::credentials::{VaultError, VaultKeyProvider};
use zeroize::Zeroizing;

const SERVICE: &str = "com.valendra.averroes";
const ACCOUNT: &str = "provider-vault-key-v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct MacKeychainKeyProvider;

impl MacKeychainKeyProvider {
    /// Ask macOS to unlock the user's default keychain when necessary. macOS
    /// does not show a permission dialog merely because an app creates its
    /// own generic-password item, but it does present the native unlock UI
    /// when the keychain is locked or an existing item needs authorization.
    #[cfg(target_os = "macos")]
    pub fn request_access(&self) -> Result<(), VaultError> {
        use security_framework::os::macos::keychain::SecKeychain;

        let mut keychain =
            SecKeychain::default().map_err(|error| VaultError::KeyProvider(error.to_string()))?;
        keychain
            .unlock(None)
            .map_err(|error| VaultError::KeyProvider(error.to_string()))
    }

    #[cfg(not(target_os = "macos"))]
    pub fn request_access(&self) -> Result<(), VaultError> {
        Err(VaultError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
impl VaultKeyProvider for MacKeychainKeyProvider {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
        use security_framework::passwords::get_generic_password;
        use security_framework_sys::base::errSecItemNotFound;
        use security_framework_sys::keychain::SecKeychainSetUserInteractionAllowed;

        // A previous library call (or a host embedding Averroes) may have
        // disabled Keychain UI for this process. Restore the default before
        // reading credentials so macOS can ask for authorization if needed.
        let interaction_status = unsafe { SecKeychainSetUserInteractionAllowed(1u8) };
        if interaction_status != 0 {
            return Err(VaultError::KeyProvider(format!(
                "could not enable Keychain interaction (status {interaction_status})"
            )));
        }

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
