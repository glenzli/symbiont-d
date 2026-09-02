//! PCP owns the shared evidence shape; Symbiont owns admission and attribution.
//! Apply once to authorized API results, never to MCP's already-projected items.
use anyhow::{Context, Result};
use pcp_client::model_context::{self, ContextBudget, ContextView};
use pcp_core::{QueryContextResponse, ReadPage, SearchResult};
use serde_json::{Value, json};

// The host already bounded reads/queries. Reuse the common shape without a
// second preview/content cutoff that could remove a qualification or source.
fn budget() -> ContextBudget {
    ContextBudget {
        content_chars: usize::MAX,
        preview_chars: usize::MAX,
    }
}

fn read(pages: &[ReadPage], view: ContextView) -> Result<Value> {
    let mut result = serde_json::to_value(model_context::read_context(pages, view, budget()))?;
    for (item, page) in result["items"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(pages)
    {
        // A host/model observation must not silently become a user assertion.
        item["createdBy"] = json!(page.revision.created_by);
        item["createdAt"] = json!(page.revision.created_at);
    }
    Ok(result)
}

pub(super) fn project(tool: &str, arguments: &Value, raw: &Value) -> Result<Option<Value>> {
    let value = match tool {
        "read_pages" => {
            let pages: Vec<ReadPage> = serde_json::from_value(raw["pages"].clone())
                .context("PCP read response does not match the typed API")?;
            let view = ContextView::parse(arguments["view"].as_str().unwrap_or("content"))
                .map_err(anyhow::Error::msg)?;
            read(&pages, view)?
        }
        "browse_index" | "search_pages" => {
            let result: SearchResult = serde_json::from_value(raw.clone())
                .context("PCP search response does not match the typed API")?;
            serde_json::to_value(model_context::search_context(&result, budget()))?
        }
        "semantic_search" | "match_intent" => {
            let result: QueryContextResponse = serde_json::from_value(raw.clone())
                .context("PCP query response does not match the typed API")?;
            let mut projected =
                serde_json::to_value(model_context::query_context(&result, budget()))?;
            for (item, entry) in projected["items"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(&result.entries)
            {
                if let Some(span) = &entry.source_span {
                    item["sourceSpan"] = json!(span);
                }
                if !entry.structural_relations.is_empty() {
                    item["structuralRelations"] = json!(entry.structural_relations);
                }
            }
            projected
        }
        "write_page" if raw["status"] == "review_required" => {
            let pages: Vec<ReadPage> = serde_json::from_value(raw["currentPages"].clone())
                .context("PCP retention review has invalid current Pages")?;
            // Keep exact proposal/token/source evidence. Only repeated PCP
            // envelopes change; persisted snapshots and hashes remain original.
            let mut review = raw.clone();
            review["currentPages"] = read(&pages, ContextView::Context)?["items"].take();
            review
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Value {
        json!({"page":{"pageId":"pg_1","headRevisionId":"rev_new","namespace":"symbiont-d",
            "kind":"concept","mutability":"revisioned","lifecycleStatus":"active",
            "createdAt":"2026-08-01","updatedAt":"2026-09-03"},
            "revision":{"pageId":"pg_1","revisionId":"rev_old","namespace":"symbiont-d",
            "lifecycleStatus":"active","observedAt":"2026-08-01","createdAt":"2026-09-01",
            "validFrom":"2026-08-01","validTo":"2026-09-01",
            "createdBy":{"actorId":"assistant","actorType":"model"},
            "payload":{"mediaType":"text/markdown","content":"用户尚未同意。\\mathfrak{S}，原话与限定不能省略。"},
            "facets":{"operational":"noise"},
            "sourceRefs":[{"providerId":"symbiont:transcript","locator":"message:1"}],
            "provenance":[{"operation":"ingest","actor":{"actorId":"host","actorType":"tool"},
                "timestamp":"2026-09-01","inputRevisionIds":["rev_basis"]}]},
            "validity":{"assessmentPageId":"pg_v","assessmentRevisionId":"rev_v","targetPageId":"pg_1",
                "targetRevisionId":"rev_old","standing":"disputed","rationale":"并非用户确认",
                "scope":"old","assessedAt":"2026-09-02","createdBy":{"actorId":"user","actorType":"user"}},
            "relations":[{"relationId":"rel_1","fromPageId":"pg_2","relationType":"supersedes",
                "toPageId":"pg_1","basisRevisionIds":["rev_2"],"createdAt":"2026-09-02",
                "createdBy":{"actorId":"host","actorType":"tool"}}],"history":["rev_old","rev_new"]})
    }

    #[test]
    fn shared_views_preserve_body_attribution_caveats_and_exact_old_head() {
        let mut page = page();
        let body = format!("{}但这并非已验证的结论。", "详细内容🙂".repeat(2000));
        page["revision"]["payload"]["content"] = json!(body);
        let raw = json!({"pages":[page]});
        let before = raw.clone();
        for view in ["content", "context", "full"] {
            let projected = project("read_pages", &json!({"view":view}), &raw)
                .unwrap()
                .unwrap();
            let item = &projected["items"][0];
            assert_eq!(item["content"], body);
            assert_eq!(item["createdBy"]["actorType"], "model");
            assert_eq!(item["validity"]["standing"], "disputed");
            assert_eq!(item["currentRevisionId"], "rev_new");
            assert_eq!(item["revisionId"], "rev_old");
            assert_eq!(item["observedAt"], "2026-08-01");
            assert_eq!(item["validTo"], "2026-09-01");
            assert!(item.get("facets").is_none());
            if view != "content" {
                assert_eq!(item["relations"][0]["fromPageId"], "pg_2");
            }
            assert_eq!(projected["truncated"], false);
        }
        assert_eq!(raw, before);
    }

    #[test]
    fn source_and_history_views_do_not_replay_body_or_internal_records() {
        let raw = json!({"pages":[page()]});
        let sources = project("read_pages", &json!({"view":"sources"}), &raw)
            .unwrap()
            .unwrap();
        assert_eq!(sources["items"][0]["sourceRefs"][0]["locator"], "message:1");
        assert_eq!(sources["items"][0]["basisRevisionIds"][0], "rev_basis");
        assert!(sources["items"][0].get("content").is_none());
        let history = project("read_pages", &json!({"view":"history"}), &raw)
            .unwrap()
            .unwrap();
        assert_eq!(
            history["items"][0]["history"],
            json!(["rev_old", "rev_new"])
        );
        assert!(history["items"][0].get("sourceRefs").is_none());
        assert!(history["items"][0].get("content").is_none());
    }

    #[test]
    fn preflight_uses_same_projection_without_changing_review_identity() {
        let raw = json!({"status":"review_required","created":false,"proposalId":"retain_1",
            "reviewToken":"review_1","proposal":{"content":"case"},
            "sourceEvidence":[{"role":"user","id":"msg_1","content":"source"}],"currentPages":[page()]});
        let result = project("write_page", &json!({}), &raw).unwrap().unwrap();
        assert_eq!(result["reviewToken"], raw["reviewToken"]);
        assert_eq!(result["sourceEvidence"], raw["sourceEvidence"]);
        assert_eq!(result["currentPages"][0]["revisionId"], "rev_old");
        assert_eq!(
            result["currentPages"][0]["content"],
            raw["currentPages"][0]["revision"]["payload"]["content"]
        );
        assert!(result.to_string().len() < raw.to_string().len());
        assert!(
            project(
                "write_page",
                &json!({}),
                &json!({"status":"written","created":true})
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn malformed_or_already_projected_api_results_do_not_fall_back_to_a_raw_dump() {
        assert!(project("read_pages", &json!({}), &json!({"items":[]})).is_err());
        assert!(
            project(
                "read_pages",
                &json!({"view":"invalid"}),
                &json!({"pages":[]})
            )
            .is_err()
        );
    }

    #[test]
    fn supplied_truncation_is_not_hidden() {
        let mut page = page();
        page["revision"]["payload"]["content"] = json!("old [projection truncated by host budget]");
        let projected = project("read_pages", &json!({}), &json!({"pages":[page]}))
            .unwrap()
            .unwrap();
        assert_eq!(projected["truncated"], true);
        assert_eq!(projected["items"][0]["detail"], "payload");
    }
}
