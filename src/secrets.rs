use std::{
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

const KEYCHAIN_SERVICE: &str = "com.glenzli.symbiont-d.ambient";

/// Where an external Provider's secret is kept. The configuration only records
/// this choice; it never serializes the secret value itself.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStore {
    #[default]
    ConfigFile,
    Keychain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    Configured,
    Missing,
    Unavailable,
}

impl CredentialStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Missing => "missing",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ConfigFileSecrets {
    #[serde(default)]
    providers: BTreeMap<String, String>,
}

/// Owns the secret material and its platform-specific storage policy. Ambient
/// topology owns Provider identities and never needs to serialize a secret.
pub struct SecretStore {
    config_path: PathBuf,
    config_file: RwLock<ConfigFileSecrets>,
    blocked_keychain_reads: RwLock<BTreeSet<String>>,
}

impl SecretStore {
    pub async fn open(config_path: PathBuf) -> Result<Self> {
        let config_file = match fs::read_to_string(&config_path).await {
            Ok(value) => toml::from_str(&value).with_context(|| {
                format!("decode local credential file {}", config_path.display())
            })?,
            Err(error) if error.kind() == ErrorKind::NotFound => ConfigFileSecrets::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read local credential file {}", config_path.display())
                });
            }
        };
        Ok(Self {
            config_path,
            config_file: RwLock::new(config_file),
            blocked_keychain_reads: RwLock::new(BTreeSet::new()),
        })
    }

    pub fn active_store(&self, requested: CredentialStore) -> CredentialStore {
        if cfg!(debug_assertions) {
            CredentialStore::ConfigFile
        } else {
            requested
        }
    }

    pub fn debug_override(&self, requested: CredentialStore) -> bool {
        cfg!(debug_assertions) && requested == CredentialStore::Keychain
    }

    pub async fn status(&self, provider_id: &str, requested: CredentialStore) -> CredentialStatus {
        match self.read(provider_id, requested).await {
            Ok(Some(_)) => CredentialStatus::Configured,
            Ok(None) => CredentialStatus::Missing,
            Err(_) => CredentialStatus::Unavailable,
        }
    }

    pub async fn read(
        &self,
        provider_id: &str,
        requested: CredentialStore,
    ) -> Result<Option<String>> {
        match self.active_store(requested) {
            CredentialStore::ConfigFile => Ok(self
                .config_file
                .read()
                .await
                .providers
                .get(provider_id)
                .cloned()
                .filter(|secret| !secret.trim().is_empty())),
            CredentialStore::Keychain => {
                if self
                    .blocked_keychain_reads
                    .read()
                    .await
                    .contains(provider_id)
                {
                    anyhow::bail!("Keychain credential requires manual reauthorization")
                }
                match keychain_read(provider_id).await {
                    Ok(value) => Ok(value),
                    Err(error) => {
                        self.blocked_keychain_reads
                            .write()
                            .await
                            .insert(provider_id.to_owned());
                        Err(error)
                    }
                }
            }
        }
    }

    pub async fn write(
        &self,
        provider_id: &str,
        requested: CredentialStore,
        secret: &str,
    ) -> Result<()> {
        if secret.trim().is_empty() {
            return Ok(());
        }
        match self.active_store(requested) {
            CredentialStore::ConfigFile => {
                let mut file = self.config_file.write().await;
                file.providers
                    .insert(provider_id.to_owned(), secret.to_owned());
                let snapshot = file.clone();
                drop(file);
                persist_config_file(&self.config_path, &snapshot).await
            }
            CredentialStore::Keychain => {
                match keychain_write(provider_id.to_owned(), secret.to_owned()).await {
                    Ok(()) => {
                        self.blocked_keychain_reads
                            .write()
                            .await
                            .remove(provider_id);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    pub async fn remove(&self, provider_id: &str, requested: CredentialStore) -> Result<()> {
        match self.active_store(requested) {
            CredentialStore::ConfigFile => {
                let mut file = self.config_file.write().await;
                if file.providers.remove(provider_id).is_none() {
                    return Ok(());
                }
                let snapshot = file.clone();
                drop(file);
                persist_config_file(&self.config_path, &snapshot).await
            }
            CredentialStore::Keychain => keychain_delete(provider_id.to_owned()).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn configuration_file_secrets_are_private_and_never_serialize_through_provider_config() {
        let path = std::env::temp_dir().join(format!(
            "symbiont-d-secrets-{}-{}.toml",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        let store = SecretStore::open(path.clone()).await.unwrap();
        store
            .write("test-provider", CredentialStore::ConfigFile, "secret-value")
            .await
            .unwrap();
        assert_eq!(
            store
                .read("test-provider", CredentialStore::ConfigFile)
                .await
                .unwrap(),
            Some("secret-value".to_owned())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        store
            .remove("test-provider", CredentialStore::ConfigFile)
            .await
            .unwrap();
        assert_eq!(
            store
                .read("test-provider", CredentialStore::ConfigFile)
                .await
                .unwrap(),
            None
        );
        std::fs::remove_file(path).unwrap();
    }
}

async fn persist_config_file(path: &PathBuf, secrets: &ConfigFileSecrets) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create credential directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("toml.tmp");
    fs::write(
        &temporary,
        toml::to_string_pretty(secrets).context("encode local credentials")?,
    )
    .await
    .with_context(|| format!("write local credential file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
            .await
            .with_context(|| format!("restrict local credential file {}", path.display()))?;
    }
    fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace local credential file {}", path.display()))
}

#[cfg(target_os = "macos")]
async fn keychain_read(provider_id: &str) -> Result<Option<String>> {
    let provider_id = provider_id.to_owned();
    tokio::task::spawn_blocking(move || {
        use security_framework::passwords::{PasswordOptions, generic_password};
        use security_framework_sys::base::errSecItemNotFound;

        match generic_password(PasswordOptions::new_generic_password(
            KEYCHAIN_SERVICE,
            &provider_id,
        )) {
            Ok(value) => String::from_utf8(value)
                .map(Some)
                .context("decode Keychain credential"),
            Err(error) if error.code() == errSecItemNotFound => Ok(None),
            Err(error) => Err(anyhow::Error::new(error)).context("read Keychain credential"),
        }
    })
    .await
    .context("join Keychain credential read")?
}

#[cfg(not(target_os = "macos"))]
async fn keychain_read(_provider_id: &str) -> Result<Option<String>> {
    anyhow::bail!("system credential store is not implemented on this platform")
}

#[cfg(target_os = "macos")]
async fn keychain_write(provider_id: String, secret: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            &provider_id,
            secret.as_bytes(),
        )
        .map_err(anyhow::Error::new)
        .context("write Keychain credential")
    })
    .await
    .context("join Keychain credential write")?
}

#[cfg(not(target_os = "macos"))]
async fn keychain_write(_provider_id: String, _secret: String) -> Result<()> {
    anyhow::bail!("system credential store is not implemented on this platform")
}

#[cfg(target_os = "macos")]
async fn keychain_delete(provider_id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        use security_framework_sys::base::errSecItemNotFound;

        match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, &provider_id)
        {
            Ok(()) => Ok(()),
            Err(error) if error.code() == errSecItemNotFound => Ok(()),
            Err(error) => Err(anyhow::Error::new(error)).context("delete Keychain credential"),
        }
    })
    .await
    .context("join Keychain credential deletion")?
}

#[cfg(not(target_os = "macos"))]
async fn keychain_delete(_provider_id: String) -> Result<()> {
    anyhow::bail!("system credential store is not implemented on this platform")
}
