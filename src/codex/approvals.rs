use serde_json::{Value, json};

use crate::permission::{PermissionDecision, PermissionRequestDraft};

const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";
const FILE_APPROVAL: &str = "item/fileChange/requestApproval";
const PERMISSIONS_APPROVAL: &str = "item/permissions/requestApproval";
const MCP_ELICITATION: &str = "mcpServer/elicitation/request";
const USER_INPUT: &str = "item/tool/requestUserInput";

pub(super) fn permission_request(message: &Value, origin: &str) -> Option<PermissionRequestDraft> {
    let method = message.get("method").and_then(Value::as_str)?;
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let reason = params
        .get("reason")
        .or_else(|| params.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let common = |kind: &str, title: String| PermissionRequestDraft {
        kind: kind.to_owned(),
        source: "codex".to_owned(),
        origin: origin.to_owned(),
        title,
        reason: reason.clone(),
        command: None,
        cwd: params.get("cwd").and_then(Value::as_str).map(str::to_owned),
        host: None,
        protocol: None,
        details: params.clone(),
        allow_accept: true,
        allow_session: true,
        allow_cancel: true,
        session_key: None,
        timeout: None,
    };
    match method {
        COMMAND_APPROVAL => {
            let network = params.get("networkApprovalContext");
            let host = network
                .and_then(|value| value.get("host"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let protocol = network
                .and_then(|value| value.get("protocol"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let mut request = common(
                if host.is_some() {
                    "networkAccess"
                } else {
                    "commandExecution"
                },
                host.as_ref()
                    .map(|host| format!("允许访问 {host}"))
                    .unwrap_or_else(|| "允许执行本地命令".to_owned()),
            );
            request.command = params
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_owned);
            request.host = host;
            request.protocol = protocol;
            Some(request)
        }
        FILE_APPROVAL => {
            let mut request = common("fileChange", "允许修改本地文件".to_owned());
            request.cwd = params
                .get("grantRoot")
                .or_else(|| params.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(request)
        }
        PERMISSIONS_APPROVAL => {
            let permissions = params.get("permissions").unwrap_or(&Value::Null);
            let network = permissions
                .pointer("/network/enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let filesystem = permissions
                .get("fileSystem")
                .is_some_and(|value| !value.is_null());
            let title = match (network, filesystem) {
                (true, true) => "允许额外的网络与文件访问",
                (true, false) => "允许额外的网络访问",
                (false, true) => "允许额外的文件访问",
                (false, false) => "确认额外权限",
            };
            Some(common("permissionGrant", title.to_owned()))
        }
        MCP_ELICITATION => {
            let mode = params.get("mode").and_then(Value::as_str).unwrap_or("form");
            let server_name = params
                .get("serverName")
                .and_then(Value::as_str)
                .unwrap_or("外部服务");
            let mut request = common("mcpElicitation", format!("{server_name} 请求进一步确认"));
            request.allow_session = false;
            request.allow_accept =
                mode == "url" || (mode == "form" && empty_confirmation_form(&params));
            request.host = params
                .get("url")
                .or_else(|| params.pointer("/_meta/origin"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some(request)
        }
        _ => None,
    }
}

pub(super) fn approval_response(message: &Value, decision: PermissionDecision) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    let params = message.get("params").unwrap_or(&Value::Null);
    match method {
        COMMAND_APPROVAL | FILE_APPROVAL => Some(json!({
            "decision": decision_name(decision)
        })),
        PERMISSIONS_APPROVAL => {
            let (permissions, scope) = match decision {
                PermissionDecision::Accept => (
                    params
                        .get("permissions")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                    "turn",
                ),
                PermissionDecision::AcceptForSession => (
                    params
                        .get("permissions")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                    "session",
                ),
                PermissionDecision::Decline | PermissionDecision::Cancel => (json!({}), "turn"),
            };
            Some(json!({
                "permissions": permissions,
                "scope": scope
            }))
        }
        MCP_ELICITATION => {
            let mode = params.get("mode").and_then(Value::as_str).unwrap_or("form");
            let (action, content) = match decision {
                PermissionDecision::Accept if mode == "form" && empty_confirmation_form(params) => {
                    ("accept", json!({}))
                }
                PermissionDecision::Accept if mode == "url" => ("accept", Value::Null),
                PermissionDecision::Decline => ("decline", Value::Null),
                PermissionDecision::Cancel => ("cancel", Value::Null),
                _ => return None,
            };
            Some(json!({"action": action, "content": content}))
        }
        _ => None,
    }
}

// Only confirmations with no input fields can be submitted without a form UI.
// Unknown schema constraints must not be silently accepted with fabricated data.
fn empty_confirmation_form(params: &Value) -> bool {
    let Some(schema) = params.get("requestedSchema").and_then(Value::as_object) else {
        return false;
    };
    schema.get("type").and_then(Value::as_str) == Some("object")
        && schema
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|fields| fields.is_empty())
        && schema
            .get("required")
            .is_none_or(|required| required.as_array().is_some_and(|items| items.is_empty()))
        && schema.keys().all(|key| {
            matches!(
                key.as_str(),
                "type"
                    | "properties"
                    | "required"
                    | "title"
                    | "description"
                    | "$schema"
                    | "additionalProperties"
            )
        })
        && schema
            .get("additionalProperties")
            .is_none_or(Value::is_boolean)
}

pub(super) fn automatic_server_request_response(message: &Value) -> Option<Value> {
    if permission_request(message, "background").is_some() {
        return approval_response(message, PermissionDecision::Decline);
    }
    if message.get("method").and_then(Value::as_str) == Some(USER_INPUT) {
        let answers = message
            .pointer("/params/questions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|question| question.get("id").and_then(Value::as_str))
            .map(|id| (id.to_owned(), json!({"answers": []})))
            .collect::<serde_json::Map<_, _>>();
        return Some(json!({"answers": answers}));
    }
    None
}

fn decision_name(decision: PermissionDecision) -> &'static str {
    match decision {
        PermissionDecision::Accept => "accept",
        PermissionDecision::AcceptForSession => "acceptForSession",
        PermissionDecision::Decline => "decline",
        PermissionDecision::Cancel => "cancel",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{approval_response, permission_request};
    use crate::permission::PermissionDecision;

    #[test]
    fn browser_origin_confirmation_can_be_explicitly_accepted() {
        let message = json!({"method": "mcpServer/elicitation/request", "params": {
            "serverName": "cua_repl", "mode": "form",
            "message": "Allow Browser use to access https://huggingface.co?",
            "requestedSchema": {"type": "object", "properties": {}},
            "_meta": {"persist": "always", "origin": "https://huggingface.co"}
        }});
        let request = permission_request(&message, "interactive").unwrap();
        assert!(request.allow_accept && request.allow_cancel);
        assert!(!request.allow_session);
        assert_eq!(request.host.as_deref(), Some("https://huggingface.co"));
        assert_eq!(
            approval_response(&message, PermissionDecision::Accept),
            Some(json!({"action":"accept", "content":{}}))
        );
        assert_eq!(
            super::automatic_server_request_response(&message),
            Some(json!({"action":"decline", "content":null}))
        );
        assert!(approval_response(&message, PermissionDecision::AcceptForSession).is_none());
    }

    #[test]
    fn forms_requiring_input_or_unknown_constraints_cannot_be_blindly_accepted() {
        for schema in [
            json!({"type":"object", "properties":{"answer":{"type":"string"}}}),
            json!({"type":"object", "properties":{}, "required":["answer"]}),
            json!({"type":"object", "properties":{}, "minProperties":1}),
            json!({"type":"object", "properties":{}, "required":null}),
            Value::Null,
        ] {
            let message = json!({"method":"mcpServer/elicitation/request", "params":{"requestedSchema":schema}});
            assert!(
                !permission_request(&message, "interactive")
                    .unwrap()
                    .allow_accept
            );
            assert!(approval_response(&message, PermissionDecision::Accept).is_none());
            assert_eq!(
                approval_response(&message, PermissionDecision::Cancel),
                Some(json!({"action":"cancel", "content":null}))
            );
        }
    }

    #[test]
    fn url_confirmation_and_implicit_form_mode_keep_distinct_payloads() {
        let url = json!({"method":"mcpServer/elicitation/request", "params":{"mode":"url", "url":"https://example.test"}});
        assert!(
            permission_request(&url, "interactive")
                .unwrap()
                .allow_accept
        );
        assert_eq!(
            approval_response(&url, PermissionDecision::Accept),
            Some(json!({"action":"accept", "content":null}))
        );
        let form = json!({"method":"mcpServer/elicitation/request", "params":{"requestedSchema":{"type":"object", "properties":{}, "required":[]}}});
        assert!(
            permission_request(&form, "interactive")
                .unwrap()
                .allow_accept
        );
        assert_eq!(
            approval_response(&form, PermissionDecision::Accept),
            Some(json!({"action":"accept", "content":{}}))
        );
    }

    #[test]
    fn network_command_becomes_a_host_specific_prompt() {
        let message = json!({
            "method": "item/commandExecution/requestApproval",
            "id": 7,
            "params": {
                "threadId": "thread",
                "turnId": "turn",
                "itemId": "item",
                "networkApprovalContext": {
                    "host": "glenzli.com",
                    "protocol": "https"
                }
            }
        });
        let request = permission_request(&message, "interactive").unwrap();
        assert_eq!(request.kind, "networkAccess");
        assert_eq!(request.host.as_deref(), Some("glenzli.com"));
        assert_eq!(request.protocol.as_deref(), Some("https"));
        assert_eq!(
            approval_response(&message, PermissionDecision::AcceptForSession).unwrap(),
            json!({"decision": "acceptForSession"})
        );
    }

    #[test]
    fn declined_permission_grants_return_an_empty_subset() {
        let message = json!({
            "method": "item/permissions/requestApproval",
            "id": 8,
            "params": {
                "permissions": {"network": {"enabled": true}}
            }
        });
        assert_eq!(
            approval_response(&message, PermissionDecision::Decline).unwrap(),
            json!({"permissions": {}, "scope": "turn"})
        );
    }
}
