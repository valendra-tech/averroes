use crate::config::{atomic_private_write, create_private_dir, ConfigError, ConfigPaths};
use crate::connection::CredentialRef;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

pub const VAULT_KEY_BYTES: usize = 32;
const VAULT_NONCE_BYTES: usize = 24;
const VAULT_VERSION: u8 = 1;
const VAULT_ALGORITHM: &str = "xchacha20poly1305";
const VAULT_AAD: &[u8] = b"averroes.providers.v1";

pub trait VaultKeyProvider: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError>;
    fn save(&self, key: &[u8]) -> Result<(), VaultError>;
    fn delete(&self) -> Result<(), VaultError>;
}

#[derive(Debug, Serialize, Deserialize)]
struct VaultEnvelope {
    version: u8,
    algorithm: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VaultPayload {
    entries: BTreeMap<String, String>,
}

impl Zeroize for VaultPayload {
    fn zeroize(&mut self) {
        for secret in self.entries.values_mut() {
            secret.zeroize();
        }
        self.entries.clear();
    }
}

impl ZeroizeOnDrop for VaultPayload {}

impl Drop for VaultPayload {
    fn drop(&mut self) {
        self.zeroize();
    }
}

#[derive(Clone)]
pub struct CredentialVault {
    paths: ConfigPaths,
    key_provider: Arc<dyn VaultKeyProvider>,
}

impl CredentialVault {
    pub fn new(paths: ConfigPaths, key_provider: Arc<dyn VaultKeyProvider>) -> Self {
        Self {
            paths,
            key_provider,
        }
    }

    pub fn put(&self, credential: &CredentialRef, secret: &str) -> Result<(), VaultError> {
        if secret.trim().is_empty() {
            return Err(VaultError::EmptySecret);
        }
        let key = self.load_or_create_key()?;
        let mut payload = self.load_payload(&key)?;
        payload
            .entries
            .insert(credential.0.clone(), secret.to_owned());
        self.save_payload(&key, &payload)
    }

    pub fn get(&self, credential: &CredentialRef) -> Result<Zeroizing<String>, VaultError> {
        if !self.paths.vault.exists() {
            return Err(VaultError::CredentialNotFound(credential.0.clone()));
        }
        let key = self.key_provider.load()?.ok_or(VaultError::Locked)?;
        validate_key(&key)?;
        let payload = self.load_payload(&key)?;
        payload
            .entries
            .get(&credential.0)
            .cloned()
            .map(Zeroizing::new)
            .ok_or_else(|| VaultError::CredentialNotFound(credential.0.clone()))
    }

    pub fn delete(&self, credential: &CredentialRef) -> Result<bool, VaultError> {
        if !self.paths.vault.exists() {
            return Ok(false);
        }
        let key = self.key_provider.load()?.ok_or(VaultError::Locked)?;
        validate_key(&key)?;
        let mut payload = self.load_payload(&key)?;
        let removed = payload.entries.remove(&credential.0).is_some();
        if removed {
            self.save_payload(&key, &payload)?;
        }
        Ok(removed)
    }

    pub fn contains(&self, credential: &CredentialRef) -> Result<bool, VaultError> {
        match self.get(credential) {
            Ok(_) => Ok(true),
            Err(VaultError::CredentialNotFound(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn reset(&self) -> Result<(), VaultError> {
        match std::fs::remove_file(&self.paths.vault) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(VaultError::Io {
                    path: self.paths.vault.clone(),
                    source,
                });
            }
        }
        self.key_provider.delete()
    }

    pub fn path(&self) -> &std::path::Path {
        &self.paths.vault
    }

    /// Initialize and touch the platform credential store before the UI is
    /// shown. The vault key is not a provider credential; creating it here
    /// guarantees that first launch cannot silently skip the Keychain access
    /// path because there is no item to read yet.
    pub fn ensure_access(&self) -> Result<(), VaultError> {
        let key = self.load_or_create_key()?;
        validate_key(&key)?;
        Ok(())
    }

    fn load_or_create_key(&self) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if let Some(key) = self.key_provider.load()? {
            validate_key(&key)?;
            return Ok(key);
        }
        if self.paths.vault.exists() {
            return Err(VaultError::Locked);
        }

        let mut key = Zeroizing::new(vec![0_u8; VAULT_KEY_BYTES]);
        getrandom::fill(&mut key).map_err(|error| VaultError::Random(error.to_string()))?;
        self.key_provider.save(&key)?;
        Ok(key)
    }

    fn load_payload(&self, key: &[u8]) -> Result<VaultPayload, VaultError> {
        if !self.paths.vault.exists() {
            return Ok(VaultPayload::default());
        }
        validate_key(key)?;
        let envelope_bytes = std::fs::read(&self.paths.vault).map_err(|source| VaultError::Io {
            path: self.paths.vault.clone(),
            source,
        })?;
        let envelope: VaultEnvelope = serde_json::from_slice(&envelope_bytes)
            .map_err(|source| VaultError::InvalidEnvelope(source.to_string()))?;
        if envelope.version != VAULT_VERSION || envelope.algorithm != VAULT_ALGORITHM {
            return Err(VaultError::UnsupportedFormat);
        }

        let nonce = base64::engine::general_purpose::STANDARD
            .decode(envelope.nonce)
            .map_err(|source| VaultError::InvalidEnvelope(source.to_string()))?;
        if nonce.len() != VAULT_NONCE_BYTES {
            return Err(VaultError::InvalidNonce);
        }
        let ciphertext = base64::engine::general_purpose::STANDARD
            .decode(envelope.ciphertext)
            .map_err(|source| VaultError::InvalidEnvelope(source.to_string()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| VaultError::InvalidKey)?;
        let plaintext = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: VAULT_AAD,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| VaultError::AuthenticationFailed)?;
        serde_json::from_slice(&plaintext)
            .map_err(|source| VaultError::InvalidPayload(source.to_string()))
    }

    fn save_payload(&self, key: &[u8], payload: &VaultPayload) -> Result<(), VaultError> {
        validate_key(key)?;
        create_private_dir(&self.paths.root).map_err(VaultError::from_config)?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(payload)
                .map_err(|source| VaultError::InvalidPayload(source.to_string()))?,
        );
        let mut nonce = [0_u8; VAULT_NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|error| VaultError::Random(error.to_string()))?;
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| VaultError::InvalidKey)?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| VaultError::AuthenticationFailed)?;
        let envelope = VaultEnvelope {
            version: VAULT_VERSION,
            algorithm: VAULT_ALGORITHM.into(),
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(ciphertext),
        };
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|source| VaultError::InvalidEnvelope(source.to_string()))?;
        atomic_private_write(&self.paths.vault, &bytes).map_err(VaultError::from_config)
    }
}

fn validate_key(key: &[u8]) -> Result<(), VaultError> {
    if key.len() == VAULT_KEY_BYTES {
        Ok(())
    } else {
        Err(VaultError::InvalidKey)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("credential vault is locked because its Keychain key is unavailable")]
    Locked,
    #[error("credential vault authentication failed")]
    AuthenticationFailed,
    #[error("credential vault key has an invalid length")]
    InvalidKey,
    #[error("credential vault nonce is invalid")]
    InvalidNonce,
    #[error("credential vault format is unsupported")]
    UnsupportedFormat,
    #[error("credential vault envelope is invalid: {0}")]
    InvalidEnvelope(String),
    #[error("credential vault payload is invalid: {0}")]
    InvalidPayload(String),
    #[error("credential not found: {0}")]
    CredentialNotFound(String),
    #[error("empty credentials cannot be stored")]
    EmptySecret,
    #[error("secure random generation failed: {0}")]
    Random(String),
    #[error("Keychain error: {0}")]
    KeyProvider(String),
    #[error("credential storage is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl VaultError {
    fn from_config(error: ConfigError) -> Self {
        match error {
            ConfigError::Io { path, source } => Self::Io { path, source },
            other => Self::InvalidEnvelope(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct MemoryKeyProvider {
        key: Mutex<Option<Vec<u8>>>,
        saves: AtomicUsize,
    }

    impl MemoryKeyProvider {
        fn clear(&self) {
            self.key.lock().take();
        }

        fn save_count(&self) -> usize {
            self.saves.load(Ordering::SeqCst)
        }
    }

    impl VaultKeyProvider for MemoryKeyProvider {
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, VaultError> {
            Ok(self.key.lock().clone().map(Zeroizing::new))
        }

        fn save(&self, key: &[u8]) -> Result<(), VaultError> {
            *self.key.lock() = Some(key.to_vec());
            self.saves.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn delete(&self) -> Result<(), VaultError> {
            self.clear();
            Ok(())
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        paths: ConfigPaths,
        keys: Arc<MemoryKeyProvider>,
        vault: CredentialVault,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::for_home(temp.path());
        let keys = Arc::new(MemoryKeyProvider::default());
        let vault = CredentialVault::new(paths.clone(), keys.clone());
        Fixture {
            _temp: temp,
            paths,
            keys,
            vault,
        }
    }

    #[test]
    fn first_put_generates_one_key_and_round_trips() {
        let fixture = fixture();
        let id = CredentialRef("credential:a".into());
        fixture.vault.put(&id, "sk-secret").unwrap();
        assert_eq!(fixture.keys.save_count(), 1);
        assert_eq!(&*fixture.vault.get(&id).unwrap(), "sk-secret");
        let file = std::fs::read_to_string(&fixture.paths.vault).unwrap();
        assert!(!file.contains("sk-secret"));
    }

    #[test]
    fn ensure_access_initializes_the_platform_key_without_creating_a_vault_file() {
        let fixture = fixture();
        fixture.vault.ensure_access().unwrap();
        assert_eq!(fixture.keys.save_count(), 1);
        assert!(!fixture.paths.vault.exists());
    }

    #[test]
    fn equal_payloads_use_different_nonces() {
        let fixture = fixture();
        let id = CredentialRef("credential:a".into());
        fixture.vault.put(&id, "same").unwrap();
        let first = std::fs::read(&fixture.paths.vault).unwrap();
        fixture.vault.put(&id, "same").unwrap();
        let second = std::fs::read(&fixture.paths.vault).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn ciphertext_tampering_fails_closed() {
        let fixture = fixture();
        let id = CredentialRef("credential:a".into());
        fixture.vault.put(&id, "secret").unwrap();
        let mut envelope: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.paths.vault).unwrap()).unwrap();
        let ciphertext = envelope["ciphertext"].as_str().unwrap();
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        envelope["ciphertext"] = base64::engine::general_purpose::STANDARD
            .encode(bytes)
            .into();
        std::fs::write(&fixture.paths.vault, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(matches!(
            fixture.vault.get(&id),
            Err(VaultError::AuthenticationFailed)
        ));
    }

    #[test]
    fn existing_vault_without_key_is_locked() {
        let fixture = fixture();
        let id = CredentialRef("credential:a".into());
        fixture.vault.put(&id, "secret").unwrap();
        fixture.keys.clear();
        assert!(matches!(fixture.vault.get(&id), Err(VaultError::Locked)));
    }

    #[test]
    fn deleting_one_entry_preserves_the_other() {
        let fixture = fixture();
        let first = CredentialRef("credential:a".into());
        let second = CredentialRef("credential:b".into());
        fixture.vault.put(&first, "first").unwrap();
        fixture.vault.put(&second, "second").unwrap();
        assert!(fixture.vault.delete(&first).unwrap());
        assert!(matches!(
            fixture.vault.get(&first),
            Err(VaultError::CredentialNotFound(_))
        ));
        assert_eq!(&*fixture.vault.get(&second).unwrap(), "second");
    }

    #[test]
    fn vault_file_is_private_on_unix() {
        let fixture = fixture();
        fixture
            .vault
            .put(&CredentialRef("credential:a".into()), "secret")
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&fixture.paths.vault)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
