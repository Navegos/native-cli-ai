use std::sync::Arc;

use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::ledger::FileLedger;

pub struct RenamePathTool {
    workspace_root: std::path::PathBuf,
    ledger: Arc<FileLedger>,
}

impl RenamePathTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        let ledger = Arc::new(FileLedger::new(workspace_root.clone()));
        Self::with_ledger(workspace_root, ledger)
    }

    pub fn with_ledger(workspace_root: std::path::PathBuf, ledger: Arc<FileLedger>) -> Self {
        Self {
            workspace_root,
            ledger,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for RenamePathTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "rename_path".into(),
            description: "Rename a file or directory within the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["from", "to"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        rename_impl(&self.workspace_root, call, "renamed", &self.ledger).await
    }
}

pub(super) async fn rename_impl(
    workspace_root: &std::path::PathBuf,
    call: &ToolCall,
    verb: &str,
    ledger: &FileLedger,
) -> ToolResult {
    let from = call.input["from"].as_str().unwrap_or("");
    let to = call.input["to"].as_str().unwrap_or("");
    let from_path = workspace_root.join(from);
    let to_path = workspace_root.join(to);

    let canonical_from = match from_path.canonicalize() {
        Ok(path) if path.starts_with(workspace_root) => path,
        _ => {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Source path is outside the workspace".into()),
            };
        }
    };

    if let Some(parent) = to_path.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create destination directory: {err}")),
            };
        }
        let canonical_parent = match parent.canonicalize() {
            Ok(path) if path.starts_with(workspace_root) => path,
            _ => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Destination path is outside the workspace".into()),
                };
            }
        };
        let _ = canonical_parent;
    }

    match tokio::fs::rename(&canonical_from, &to_path).await {
        Ok(()) => {
            // Exempt from read-before-edit: the old path is gone, and an
            // earlier read of an overwritten target stays valid.
            ledger.forget(&canonical_from);
            ledger.refresh_from_disk(&to_path);
            ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: format!(
                    "{verb} {} -> {}",
                    canonical_from.display(),
                    to_path.display()
                ),
                error: None,
            }
        }
        Err(err) => ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(format!("Failed to rename path: {err}")),
        },
    }
}
