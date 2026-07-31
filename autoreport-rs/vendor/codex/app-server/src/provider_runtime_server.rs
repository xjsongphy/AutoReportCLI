//! JSON-RPC adapter for AutoReport's provider-backed runtime.
//!
//! This is intentionally a small adapter around [`LoopManager`]. It does not
//! construct providers, load credentials, or route to any Codex service.

use crate::provider_dispatch::{ProviderRequest, parse_provider_request};
use crate::runtime_adapter::RuntimeSessionRegistry;
use autoreport_app_server_protocol::{
    JSONRPCErrorError, JSONRPCMessage, JSONRPCRequest, JSONRPCResponse, RequestId,
};
use autoreport_runtime::LoopManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// Handles the provider-runtime subset of the app-server protocol.
pub struct ProviderRuntimeServer {
    registry: Arc<RuntimeSessionRegistry>,
    manager: Arc<LoopManager>,
    autoreport_home: PathBuf,
    workspace: PathBuf,
}

impl ProviderRuntimeServer {
    pub fn new(
        registry: Arc<RuntimeSessionRegistry>,
        manager: Arc<LoopManager>,
        autoreport_home: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            manager,
            autoreport_home: autoreport_home.into(),
            workspace: workspace.into(),
        }
    }

    pub fn registry(&self) -> &Arc<RuntimeSessionRegistry> {
        &self.registry
    }

    /// Dispatch one provider-runtime request.
    ///
    /// `None` means the method is outside AutoReport's deliberately small
    /// surface. The transport drops such requests instead of advertising or
    /// emulating capabilities that this product does not provide.
    pub async fn handle_request(&self, request: JSONRPCRequest) -> Option<JSONRPCMessage> {
        let request_id = request.id.clone();
        let parsed = match parse_provider_request(request) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => return None,
            Err(err) => return Some(error(request_id, INVALID_PARAMS, err.to_string())),
        };

        Some(match self.dispatch(parsed).await {
            Ok(result) => JSONRPCMessage::Response(JSONRPCResponse {
                id: request_id,
                result,
            }),
            Err(DispatchError::InvalidParams(message)) => {
                error(request_id, INVALID_PARAMS, message)
            }
            Err(DispatchError::Internal(message)) => error(request_id, INTERNAL_ERROR, message),
        })
    }

    async fn dispatch(&self, request: ProviderRequest) -> Result<Value, DispatchError> {
        match request {
            ProviderRequest::Initialize { .. } => Ok(json!({
                "userAgent": "autoreport-app-server",
                "codexHome": self.autoreport_home,
                "platformFamily": std::env::consts::FAMILY,
                "platformOs": std::env::consts::OS,
            })),
            ProviderRequest::ThreadStart { params, .. } => {
                let workspace = params
                    .cwd
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.workspace.clone());
                if !same_workspace(&workspace, &self.workspace) {
                    return Err(DispatchError::InvalidParams(format!(
                        "thread cwd must match the app-server workspace ({})",
                        self.workspace.display()
                    )));
                }
                let thread_id = Uuid::new_v4().to_string();
                self.registry
                    .register(&thread_id, &workspace, Arc::clone(&self.manager))
                    .map_err(internal)?;
                let model = params.model.unwrap_or_default();
                let model_provider = params
                    .model_provider
                    .unwrap_or_else(|| "autoreport".to_string());
                Ok(thread_start_result(
                    &thread_id,
                    &workspace,
                    &model,
                    &model_provider,
                ))
            }
            ProviderRequest::ThreadRead { params, .. } => {
                let session = self.registry.get(&params.thread_id).map_err(internal)?;
                let history = if params.include_turns {
                    Some(session.history().await.map_err(internal)?)
                } else {
                    None
                };
                Ok(json!({
                    "thread": thread_result(&session.thread_id(), session.workspace(), history),
                }))
            }
            ProviderRequest::ThreadList { .. } => {
                let data = self
                    .registry
                    .list()
                    .map_err(internal)?
                    .into_iter()
                    .map(|info| thread_result(&info.thread_id, &info.workspace, None))
                    .collect::<Vec<_>>();
                Ok(json!({ "data": data, "nextCursor": null, "backwardsCursor": null }))
            }
            ProviderRequest::TurnStart { params, .. } => {
                let content = text_input(&params.input)?;
                self.registry
                    .submit(&params.thread_id, content)
                    .map_err(internal)?;
                Ok(json!({ "turn": { "id": Uuid::new_v4().to_string(), "items": [] } }))
            }
            ProviderRequest::TurnSteer { params, .. } => {
                let content = text_input(&params.input)?;
                self.registry
                    .steer(&params.thread_id, content)
                    .await
                    .map_err(internal)?;
                Ok(json!({ "turnId": params.expected_turn_id }))
            }
            ProviderRequest::TurnInterrupt { params, .. } => {
                self.registry
                    .interrupt(&params.thread_id)
                    .map_err(internal)?;
                Ok(json!({}))
            }
        }
    }
}

#[derive(Debug)]
enum DispatchError {
    InvalidParams(String),
    Internal(String),
}

fn internal(error: impl std::fmt::Display) -> DispatchError {
    DispatchError::Internal(error.to_string())
}

fn same_workspace(left: &Path, right: &Path) -> bool {
    left == right
        || match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
}

fn error(id: RequestId, code: i64, message: impl Into<String>) -> JSONRPCMessage {
    JSONRPCMessage::Error(autoreport_app_server_protocol::JSONRPCError {
        id,
        error: JSONRPCErrorError {
            code,
            message: message.into(),
            data: None,
        },
    })
}

fn text_input(
    input: &[autoreport_app_server_protocol::UserInput],
) -> Result<String, DispatchError> {
    let mut texts = Vec::with_capacity(input.len());
    for item in input {
        match item {
            autoreport_app_server_protocol::UserInput::Text { text, .. } => {
                if !text.trim().is_empty() {
                    texts.push(text.clone());
                }
            }
            _ => {
                return Err(DispatchError::InvalidParams(
                    "AutoReport provider runtime accepts text input only".to_string(),
                ));
            }
        }
    }
    if texts.is_empty() {
        return Err(DispatchError::InvalidParams(
            "turn input cannot be empty".to_string(),
        ));
    }
    Ok(texts.join("\n"))
}

fn thread_start_result(id: &str, workspace: &Path, model: &str, provider: &str) -> Value {
    json!({
        "thread": thread_result(id, workspace, None),
        "model": model,
        "modelProvider": provider,
        "serviceTier": null,
        "cwd": workspace,
        "runtimeWorkspaceRoots": [],
        "instructionSources": [],
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": { "type": "dangerFullAccess" },
        "activePermissionProfile": null,
        "reasoningEffort": null,
        "multiAgentMode": "explicitRequestOnly"
    })
}

fn thread_result(
    id: &str,
    workspace: &Path,
    history: Option<Vec<autoreport_rollout::ResponseItem>>,
) -> Value {
    json!({
        "id": id,
        "extra": null,
        "sessionId": id,
        "forkedFromId": null,
        "parentThreadId": null,
        "preview": "",
        "ephemeral": false,
        "isPinned": false,
        "historyMode": "legacy",
        "modelProvider": "autoreport",
        "createdAt": 0,
        "updatedAt": 0,
        "recencyAt": null,
        "status": "idle",
        "path": null,
        "cwd": workspace,
        "cliVersion": env!("CARGO_PKG_VERSION"),
        "source": "appServer",
        "canAcceptDirectInput": true,
        "threadSource": null,
        "agentNickname": null,
        "agentRole": null,
        "gitInfo": null,
        "name": null,
        "turns": history.unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::text_input;
    use autoreport_app_server_protocol::UserInput;

    #[test]
    fn text_input_rejects_non_text_items() {
        assert!(
            text_input(&[UserInput::Image {
                url: "x".into(),
                detail: None
            }])
            .is_err()
        );
    }
}
