use std::{collections::BTreeMap, io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::{
    ambient_api::AmbientSnapshot, drive_input::DriveInputSnapshot, mail_input::MailInputSnapshot,
    sensing::InputRoleSnapshot,
};

pub const INPUT_ROLE_AVATARS: [&str; 8] = [
    "moon-window",
    "courier",
    "prism",
    "firefly",
    "tide",
    "seed",
    "star-map",
    "echo",
];
const SYMBIONT_DISSENT_AVATAR: &str = "symbiont-dissent";
const SYMBIONT_ATTACKER_ID: &str = "symbiont_attacker";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputRoleAppearance {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct InputRoleDocument {
    #[serde(default)]
    roles: BTreeMap<String, InputRoleAppearance>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRoleAppearanceUpdate {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub avatar: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRoleSettingsUpdate {
    pub roles: Vec<InputRoleAppearanceUpdate>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRoleSettingsSnapshot {
    pub roles: Vec<InputRoleDescriptor>,
    pub avatar_options: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRoleDescriptor {
    pub id: String,
    pub kind: &'static str,
    pub source: String,
    pub default_name: String,
    pub name: String,
    pub custom_name: bool,
    pub avatar: String,
}

pub struct InputRoleStore {
    path: PathBuf,
    document: RwLock<InputRoleDocument>,
}

impl InputRoleStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let mut document = match fs::read_to_string(&path).await {
            Ok(value) => toml::from_str(&value).unwrap_or_else(|error| {
                tracing::warn!(%error, path = %path.display(), "input role appearance configuration is invalid; using defaults");
                InputRoleDocument::default()
            }),
            Err(error) if error.kind() == ErrorKind::NotFound => InputRoleDocument::default(),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("read input role appearance configuration {}", path.display())
                });
            }
        };
        // The dissent role is a visible posture of Symbiont rather than an
        // independent external speaker. Drop legacy appearance overrides so
        // its name and avatar always follow the primary identity.
        document.roles.remove(SYMBIONT_ATTACKER_ID);
        let store = Self {
            path,
            document: RwLock::new(document),
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn snapshot(
        &self,
        ambient: &AmbientSnapshot,
        drive: &DriveInputSnapshot,
        mail: &MailInputSnapshot,
        attacker_enabled: bool,
        symbiont_display_name: &str,
    ) -> InputRoleSettingsSnapshot {
        let document = self.document.read().await;
        let mut roles = Vec::new();
        if ambient.luna.config.enabled {
            roles.push(descriptor(
                &document,
                "ambient_luna",
                "built_in",
                "Codex · Luna",
                "Luna · 广域输入",
            ));
        }
        let enabled_providers = ambient
            .providers
            .iter()
            .filter(|provider| provider.enabled)
            .map(|provider| provider.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        for channel in ambient.channels.iter().filter(|channel| {
            channel.config.enabled
                && enabled_providers.contains(channel.config.provider_id.as_str())
        }) {
            roles.push(descriptor(
                &document,
                &format!("ambient_{}", channel.config.id),
                "provider",
                &format!("{} · {}", channel.config.provider_id, channel.config.model),
                &channel.config.name,
            ));
        }
        if drive.config.enabled {
            roles.push(descriptor(
                &document,
                "drive_digests",
                "document_feed",
                "Google Drive",
                &drive.config.name,
            ));
        }
        if mail.config.enabled {
            roles.push(descriptor(
                &document,
                "mail_inbox",
                "mailbox",
                "IMAP · 私有收件箱",
                &mail.config.name,
            ));
        }
        if attacker_enabled {
            roles.push(descriptor(
                &document,
                SYMBIONT_ATTACKER_ID,
                "reviewer",
                "Codex · 异议审阅",
                &format!("{symbiont_display_name} · 异议"),
            ));
        }
        InputRoleSettingsSnapshot {
            roles,
            avatar_options: INPUT_ROLE_AVATARS.into_iter().collect(),
        }
    }

    pub async fn update(&self, update: InputRoleSettingsUpdate) -> Result<()> {
        let mut roles = BTreeMap::new();
        for role in update.roles {
            if role.id == SYMBIONT_ATTACKER_ID {
                continue;
            }
            validate_role(&role)?;
            roles.insert(
                role.id,
                InputRoleAppearance {
                    name: role.name.map(|name| name.trim().to_owned()),
                    avatar: Some(role.avatar),
                },
            );
        }
        // Keep hidden roles so a temporary disable does not reset their identity.
        let mut document = self.document.write().await;
        document.roles.remove(SYMBIONT_ATTACKER_ID);
        document.roles.extend(roles);
        drop(document);
        self.persist().await
    }

    pub async fn apply(&self, actor: &mut InputRoleSnapshot, symbiont_display_name: &str) {
        if actor.id == SYMBIONT_ATTACKER_ID {
            actor.name = format!("{symbiont_display_name} · 异议");
            actor.avatar_seed = SYMBIONT_DISSENT_AVATAR.to_owned();
            return;
        }
        let document = self.document.read().await;
        let appearance = document.roles.get(&actor.id);
        if let Some(name) = appearance
            .and_then(|appearance| appearance.name.as_deref())
            .filter(|name| custom_name(&actor.id, name))
        {
            actor.name = name.to_owned();
        }
        actor.avatar_seed = appearance
            .and_then(|appearance| appearance.avatar.as_deref())
            .and_then(normalize_avatar)
            .map(str::to_owned)
            .unwrap_or_else(|| default_avatar(&actor.id).to_owned());
    }

    async fn persist(&self) -> Result<()> {
        let content = {
            let document = self.document.read().await;
            toml::to_string_pretty(&*document).context("encode input role appearances")?
        };
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await.with_context(|| {
                format!(
                    "create input role appearance directory {}",
                    parent.display()
                )
            })?;
        }
        let temporary = self.path.with_extension("toml.tmp");
        fs::write(&temporary, content).await.with_context(|| {
            format!(
                "write input role appearance configuration {}",
                temporary.display()
            )
        })?;
        fs::rename(&temporary, &self.path).await.with_context(|| {
            format!(
                "replace input role appearance configuration {}",
                self.path.display()
            )
        })
    }
}

fn descriptor(
    document: &InputRoleDocument,
    id: &str,
    kind: &'static str,
    source: &str,
    default_name: &str,
) -> InputRoleDescriptor {
    if id == SYMBIONT_ATTACKER_ID {
        return InputRoleDescriptor {
            id: id.to_owned(),
            kind,
            source: source.to_owned(),
            default_name: default_name.to_owned(),
            name: default_name.to_owned(),
            custom_name: false,
            avatar: SYMBIONT_DISSENT_AVATAR.to_owned(),
        };
    }
    let appearance = document.roles.get(id);
    let custom_name = appearance
        .and_then(|appearance| appearance.name.as_deref())
        .is_some_and(|name| custom_name(id, name));
    InputRoleDescriptor {
        id: id.to_owned(),
        kind,
        source: source.to_owned(),
        default_name: default_name.to_owned(),
        name: if custom_name {
            appearance
                .and_then(|appearance| appearance.name.clone())
                .unwrap_or_else(|| default_name.to_owned())
        } else {
            default_name.to_owned()
        },
        custom_name,
        avatar: appearance
            .and_then(|appearance| appearance.avatar.as_deref())
            .and_then(normalize_avatar)
            .map(str::to_owned)
            .unwrap_or_else(|| default_avatar(id).to_owned()),
    }
}

fn custom_name(id: &str, name: &str) -> bool {
    !name.trim().is_empty() && id != SYMBIONT_ATTACKER_ID
}

fn default_avatar(id: &str) -> &'static str {
    if id == SYMBIONT_ATTACKER_ID {
        return SYMBIONT_DISSENT_AVATAR;
    }
    let hash = id.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    INPUT_ROLE_AVATARS[hash % INPUT_ROLE_AVATARS.len()]
}

fn normalize_avatar(avatar: &str) -> Option<&'static str> {
    if avatar == SYMBIONT_DISSENT_AVATAR {
        return Some(SYMBIONT_DISSENT_AVATAR);
    }
    INPUT_ROLE_AVATARS
        .iter()
        .copied()
        .find(|candidate| *candidate == avatar)
        .or_else(|| match avatar {
            "orbit" => Some("moon-window"),
            "ripple" => Some("tide"),
            "spark" => Some("firefly"),
            "comet" => Some("courier"),
            "moss" => Some("seed"),
            "dawn" => Some("star-map"),
            _ => None,
        })
}

fn validate_role(role: &InputRoleAppearanceUpdate) -> Result<()> {
    if role.id.trim().is_empty() || role.id.chars().count() > 160 {
        anyhow::bail!("input role id is invalid");
    }
    if role.name.as_ref().is_some_and(|name| {
        name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control)
    }) {
        anyhow::bail!("input role name must contain between 1 and 80 characters");
    }
    if normalize_avatar(&role.avatar).is_none() {
        anyhow::bail!("input role avatar is unknown");
    }
    if role.avatar == SYMBIONT_DISSENT_AVATAR {
        anyhow::bail!("the Symbiont dissent avatar is not a configurable input role avatar");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn default_avatar_is_stable_and_drawn_from_the_visible_pool() {
        let first = default_avatar("ambient_luna");
        assert_eq!(first, default_avatar("ambient_luna"));
        assert!(INPUT_ROLE_AVATARS.contains(&first));
        assert_eq!(default_avatar("symbiont_attacker"), SYMBIONT_DISSENT_AVATAR);
    }

    #[test]
    fn rejects_unknown_avatar_values() {
        assert!(
            validate_role(&InputRoleAppearanceUpdate {
                id: "ambient_luna".to_owned(),
                name: Some("Luna".to_owned()),
                avatar: "remote-url".to_owned(),
            })
            .is_err()
        );
    }

    #[test]
    fn keeps_the_derived_avatar_out_of_configurable_roles() {
        assert!(
            validate_role(&InputRoleAppearanceUpdate {
                id: "ambient_luna".to_owned(),
                name: None,
                avatar: SYMBIONT_DISSENT_AVATAR.to_owned(),
            })
            .is_err()
        );
    }

    #[test]
    fn maps_legacy_avatars_to_the_new_illustrated_pool() {
        assert_eq!(normalize_avatar("orbit"), Some("moon-window"));
        assert_eq!(normalize_avatar("moss"), Some("seed"));
    }

    #[tokio::test]
    async fn nickname_updates_are_persisted() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("symbiont-input-roles-{nonce}.toml"));
        let store = InputRoleStore::open(path.clone())
            .await
            .expect("open store");
        store
            .update(InputRoleSettingsUpdate {
                roles: vec![InputRoleAppearanceUpdate {
                    id: "mail_inbox".to_owned(),
                    name: Some("星图信使".to_owned()),
                    avatar: "courier".to_owned(),
                }],
            })
            .await
            .expect("save nickname");
        let persisted = fs::read_to_string(&path)
            .await
            .expect("read saved settings");
        assert!(persisted.contains("星图信使"));
        assert!(persisted.contains("courier"));
        let _ = fs::remove_file(path).await;
    }

    #[test]
    fn attacker_default_name_follows_the_symbiont_nickname() {
        let document = InputRoleDocument::default();
        let role = descriptor(
            &document,
            "symbiont_attacker",
            "reviewer",
            "Codex · 异议审阅",
            "小伴 · 异议",
        );
        assert_eq!(role.name, "小伴 · 异议");
    }

    #[test]
    fn legacy_attacker_name_does_not_freeze_the_derived_default() {
        let mut document = InputRoleDocument::default();
        document.roles.insert(
            "symbiont_attacker".to_owned(),
            InputRoleAppearance {
                name: Some("逆向审视".to_owned()),
                avatar: None,
            },
        );
        let role = descriptor(
            &document,
            "symbiont_attacker",
            "reviewer",
            "Codex · 异议审阅",
            "小伴 · 异议",
        );
        assert_eq!(role.name, "小伴 · 异议");
    }

    #[test]
    fn attacker_appearance_is_always_derived() {
        let mut document = InputRoleDocument::default();
        document.roles.insert(
            "symbiont_attacker".to_owned(),
            InputRoleAppearance {
                name: Some("边界检查员".to_owned()),
                avatar: None,
            },
        );
        let role = descriptor(
            &document,
            "symbiont_attacker",
            "reviewer",
            "Codex · 异议审阅",
            "小伴 · 异议",
        );
        assert_eq!(role.name, "小伴 · 异议");
        assert!(!role.custom_name);
        assert_eq!(role.avatar, SYMBIONT_DISSENT_AVATAR);
    }
}
