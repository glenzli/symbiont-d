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
            request.allow_accept = mode == "url";
            request.host = params.get("url").and_then(Value::as_str).map(str::to_owned);
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
        MCP_ELICITATION => Some(json!({
            "action": match decision {
                PermissionDecision::Accept | PermissionDecision::AcceptForSession => "accept",
                PermissionDecision::Decline => "decline",
                PermissionDecision::Cancel => "cancel",
            },
            "content": null
        })),
        _ => None,
    }
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
    use serde_json::json;

    use super::{approval_response, permission_request};
    use crate::permission::PermissionDecision;

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
