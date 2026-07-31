use std::{io::ErrorKind, path::PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{fs, sync::RwLock};

use crate::compute::ComputeLane;

const MAX_POLICIES: usize = 50;
const MAX_ALIASES: usize = 16;
const MAX_TOPIC_CHARS: usize = 120;
const MAX_ALIAS_CHARS: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeTopicPolicy {
    pub id: String,
    pub topic: String,
    pub aliases: Vec<String>,
    pub minimum_lane: ComputeLane,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputeTopicPolicyDraft {
    #[serde(default)]
    pub id: Option<String>,
    pub topic: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub minimum_lane: ComputeLane,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComputePolicyMatch {
    pub policy: ComputeTopicPolicy,
    pub matched_alias: String,
}

#[derive(Default, Deserialize, Serialize)]
struct ComputePolicyDocument {
    #[serde(default)]
    policies: Vec<ComputeTopicPolicy>,
}

pub struct ComputePolicyStore {
    path: PathBuf,
    policies: RwLock<Vec<ComputeTopicPolicy>>,
}

impl ComputePolicyStore {
    pub async fn open(path: PathBuf) -> Result<Self> {
        let policies = match fs::read_to_string(&path).await {
            Ok(content) => {
                let document = toml::from_str::<ComputePolicyDocument>(&content)
                    .context("parse compute topic policies")?;
                validate_policies(&document.policies)?;
                document.policies
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("read compute policies {}", path.display()));
            }
        };
        let store = Self {
            path,
            policies: RwLock::new(policies),
        };
        store.persist().await?;
        Ok(store)
    }

    pub async fn snapshot(&self) -> Vec<ComputeTopicPolicy> {
        self.policies.read().await.clone()
    }

    pub async fn replace(
        &self,
        drafts: Vec<ComputeTopicPolicyDraft>,
    ) -> Result<Vec<ComputeTopicPolicy>> {
        if drafts.len() > MAX_POLICIES {
            anyhow::bail!("at most {MAX_POLICIES} compute topic policies are allowed");
        }
        let existing = self.policies.read().await.clone();
        let now = Utc::now().to_rfc3339();
        let mut policies = Vec::with_capacity(drafts.len());
        for (index, draft) in drafts.into_iter().enumerate() {
            let previous = draft
                .id
                .as_deref()
                .and_then(|id| existing.iter().find(|policy| policy.id == id));
            policies.push(policy_from_draft(draft, previous, &now, index)?);
        }
        validate_policies(&policies)?;
        *self.policies.write().await = policies;
        self.persist().await?;
        Ok(self.snapshot().await)
    }

    pub async fn upsert(&self, draft: ComputeTopicPolicyDraft) -> Result<ComputeTopicPolicy> {
        let mut policies = self.policies.write().await;
        let normalized_topic = normalize_text(&draft.topic);
        let previous_index = draft
            .id
            .as_deref()
            .and_then(|id| policies.iter().position(|policy| policy.id == id))
            .or_else(|| {
                policies
                    .iter()
                    .position(|policy| normalize_text(&policy.topic) == normalized_topic)
            });
        let now = Utc::now().to_rfc3339();
        let previous = previous_index.and_then(|index| policies.get(index));
        let policy = policy_from_draft(draft, previous, &now, policies.len())?;
        if let Some(index) = previous_index {
            policies[index] = policy.clone();
        } else {
            if policies.len() >= MAX_POLICIES {
                anyhow::bail!("at most {MAX_POLICIES} compute topic policies are allowed");
            }
            policies.push(policy.clone());
        }
        validate_policies(&policies)?;
        drop(policies);
        self.persist().await?;
        Ok(policy)
    }

    pub async fn remove(&self, id: &str) -> Result<bool> {
        let mut policies = self.policies.write().await;
        let original_len = policies.len();
        policies.retain(|policy| policy.id != id);
        let removed = policies.len() != original_len;
        drop(policies);
        if removed {
            self.persist().await?;
        }
        Ok(removed)
    }

    pub async fn match_texts<'a>(
        &self,
        texts: impl IntoIterator<Item = &'a str>,
    ) -> Option<ComputePolicyMatch> {
        let texts = texts
            .into_iter()
            .map(normalize_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        let policies = self.policies.read().await;
        policies
            .iter()
            .filter(|policy| policy.enabled)
            .filter_map(|policy| {
                policy_aliases(policy).find_map(|alias| {
                    texts
                        .iter()
                        .any(|text| contains_alias(text, &alias))
                        .then(|| ComputePolicyMatch {
                            policy: policy.clone(),
                            matched_alias: alias,
                        })
                })
            })
            .max_by_key(|matched| matched.policy.minimum_lane)
    }

    pub async fn prompt(&self) -> String {
        let policies = self.policies.read().await;
        let enabled = policies
            .iter()
            .filter(|policy| policy.enabled)
            .map(|policy| {
                let aliases = policy_aliases(policy).collect::<Vec<_>>().join(", ");
                format!(
                    "- {} [{}] => minimum {}",
                    policy.topic,
                    aliases,
                    policy.minimum_lane.as_str()
                )
            })
            .collect::<Vec<_>>();
        if enabled.is_empty() {
            return String::new();
        }
        format!(
            "User-owned compute rules:\n{}\nIf the current discussion semantically matches a \
             rule, the host lane is lower, and escalation is available, call \
             `symbiont.escalate` before any substantive answer. These are minimum-compute \
             constraints, not sticky conversation modes or claims about the topic. Re-evaluate the \
             current request each turn and do not inherit a rule merely because an older message \
             mentioned its alias.",
            enabled.join("\n")
        )
    }

    async fn persist(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create compute policy directory {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(&ComputePolicyDocument {
            policies: self.snapshot().await,
        })
        .context("encode compute topic policies")?;
        let temporary = self.path.with_extension("toml.tmp");
        fs::write(&temporary, content)
            .await
            .with_context(|| format!("write compute policies {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .await
            .with_context(|| format!("replace compute policies {}", self.path.display()))
    }
}

fn policy_from_draft(
    draft: ComputeTopicPolicyDraft,
    previous: Option<&ComputeTopicPolicy>,
    now: &str,
    index: usize,
) -> Result<ComputeTopicPolicy> {
    let topic = draft.topic.trim().to_owned();
    let aliases = deduplicate_aliases(draft.aliases);
    let id = previous
        .map(|policy| policy.id.clone())
        .or(draft.id)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("policy_{}_{}", Utc::now().timestamp_micros(), index));
    let policy = ComputeTopicPolicy {
        id,
        topic,
        aliases,
        minimum_lane: draft.minimum_lane,
        enabled: draft.enabled,
        created_at: previous
            .map(|policy| policy.created_at.clone())
            .unwrap_or_else(|| now.to_owned()),
        updated_at: now.to_owned(),
    };
    validate_policy(&policy)?;
    Ok(policy)
}

fn validate_policies(policies: &[ComputeTopicPolicy]) -> Result<()> {
    if policies.len() > MAX_POLICIES {
        anyhow::bail!("at most {MAX_POLICIES} compute topic policies are allowed");
    }
    let mut ids = std::collections::HashSet::new();
    for policy in policies {
        validate_policy(policy)?;
        if !ids.insert(&policy.id) {
            anyhow::bail!("duplicate compute topic policy id: {}", policy.id);
        }
    }
    Ok(())
}

fn validate_policy(policy: &ComputeTopicPolicy) -> Result<()> {
    let topic_chars = policy.topic.chars().count();
    if topic_chars == 0 || topic_chars > MAX_TOPIC_CHARS {
        anyhow::bail!("compute policy topic must contain 1-{MAX_TOPIC_CHARS} characters");
    }
    if policy.aliases.len() > MAX_ALIASES {
        anyhow::bail!("a compute policy can contain at most {MAX_ALIASES} aliases");
    }
    if policy
        .aliases
        .iter()
        .any(|alias| alias.is_empty() || alias.chars().count() > MAX_ALIAS_CHARS)
    {
        anyhow::bail!("compute policy aliases must contain 1-{MAX_ALIAS_CHARS} characters");
    }
    if policy.minimum_lane < ComputeLane::Investigate {
        anyhow::bail!("topic policies may require only investigate or critical lanes");
    }
    Ok(())
}

fn policy_aliases(policy: &ComputeTopicPolicy) -> impl Iterator<Item = String> + '_ {
    std::iter::once(normalize_text(&policy.topic)).chain(
        policy
            .aliases
            .iter()
            .map(|alias| normalize_text(alias))
            .filter(|alias| !alias.is_empty()),
    )
}

fn deduplicate_aliases(aliases: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    aliases
        .into_iter()
        .map(|alias| alias.trim().to_owned())
        .filter(|alias| !alias.is_empty())
        .filter(|alias| seen.insert(normalize_text(alias)))
        .collect()
}

fn normalize_text(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_alias(text: &str, alias: &str) -> bool {
    if alias
        .chars()
        .all(|character| character.is_ascii_alphanumeric())
    {
        text.split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token == alias)
    } else {
        text.contains(alias)
    }
}

fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "symbiont-compute-policy-{label}-{}-{}.toml",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn draft(topic: &str, aliases: &[&str], lane: ComputeLane) -> ComputeTopicPolicyDraft {
        ComputeTopicPolicyDraft {
            id: None,
            topic: topic.to_owned(),
            aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
            minimum_lane: lane,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn matches_ascii_aliases_without_substring_false_positives() {
        let store = ComputePolicyStore::open(path("ascii")).await.unwrap();
        store
            .upsert(draft(
                "Operator Evolution Theory",
                &["OET"],
                ComputeLane::Critical,
            ))
            .await
            .unwrap();

        assert!(store.match_texts(["继续讨论 OET 的公理环"]).await.is_some());
        assert!(store.match_texts(["poetry"]).await.is_none());
    }

    #[tokio::test]
    async fn selects_the_strongest_matching_policy() {
        let store = ComputePolicyStore::open(path("strongest")).await.unwrap();
        store
            .upsert(draft("PCP", &[], ComputeLane::Investigate))
            .await
            .unwrap();
        store
            .upsert(draft("OET", &[], ComputeLane::Critical))
            .await
            .unwrap();

        let matched = store
            .match_texts(["比较 PCP 和 OET"])
            .await
            .expect("a policy should match");
        assert_eq!(matched.policy.topic, "OET");
        assert_eq!(matched.policy.minimum_lane, ComputeLane::Critical);
    }

    #[tokio::test]
    async fn persists_replaced_policies() {
        let path = path("persist");
        let store = ComputePolicyStore::open(path.clone()).await.unwrap();
        let policies = store
            .replace(vec![draft(
                "Operator Evolution Theory",
                &["OET", "算子演化理论"],
                ComputeLane::Critical,
            )])
            .await
            .unwrap();
        drop(store);

        let reopened = ComputePolicyStore::open(path).await.unwrap();
        assert_eq!(reopened.snapshot().await, policies);
    }
}
