use std::sync::Arc;

use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::ledger::FileLedger;

pub struct ApplyPatchTool {
    workspace_root: std::path::PathBuf,
    ledger: Arc<FileLedger>,
}

impl ApplyPatchTool {
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
impl ToolExecutor for ApplyPatchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "apply_patch".into(),
            description: "Apply one or more exact string replacements to a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": { "type": "string" },
                                "new_text": { "type": "string" },
                                "replace_all": { "type": "boolean" }
                            },
                            "required": ["old_text", "new_text"]
                        }
                    }
                },
                "required": ["path", "edits"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let workspace_root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let full_path = self.workspace_root.join(path);
        let canonical = match full_path.canonicalize() {
            Ok(canonical) if canonical.starts_with(&workspace_root) => canonical,
            _ => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Path is outside the workspace".into()),
                };
            }
        };

        let mut content = match tokio::fs::read_to_string(&canonical).await {
            Ok(content) => content,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file: {err}")),
                };
            }
        };

        if let Err(message) = self.ledger.check_edit(&canonical, &content) {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(message),
            };
        }

        let Some(edits) = call.input["edits"].as_array() else {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("edits must be an array".into()),
            };
        };

        for edit in edits {
            let old_text = edit["old_text"].as_str().unwrap_or("");
            let new_text = edit["new_text"].as_str().unwrap_or("");
            let replace_all = edit["replace_all"].as_bool().unwrap_or(false);

            if old_text.is_empty() {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("old_text must not be empty".into()),
                };
            }

            let occurrence_count = content.matches(old_text).count();
            if occurrence_count == 0 {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("text not found in {}", canonical.display())),
                };
            }

            if replace_all {
                content = content.replace(old_text, new_text);
            } else if occurrence_count > 1 {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "text matched {occurrence_count} occurrences in {}; use replace_all or replace_match for a precise edit",
                        canonical.display()
                    )),
                };
            } else if let Some(index) = content.find(old_text) {
                content.replace_range(index..index + old_text.len(), new_text);
            } else {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("text not found in {}", canonical.display())),
                };
            }
        }

        match tokio::fs::write(&canonical, &content).await {
            Ok(()) => {
                self.ledger.record(&canonical, &content);
                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: format!("Patched {}", canonical.display()),
                    error: None,
                }
            }
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write file: {err}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "apply_patch".into(),
            input,
        }
    }

    /// Writes `main.rs` and returns a tool whose ledger saw that content being read.
    fn tool_after_read(dir: &tempfile::TempDir, content: &str) -> ApplyPatchTool {
        let path = dir.path().join("main.rs");
        std::fs::write(&path, content).unwrap();
        let ledger = Arc::new(FileLedger::new(dir.path().to_path_buf()));
        ledger.record(&path, content);
        ApplyPatchTool::with_ledger(dir.path().to_path_buf(), ledger)
    }

    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_single_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_after_read(&dir, "alpha\nalpha\n");
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "edits": [
                    {
                        "old_text": "alpha",
                        "new_text": "beta"
                    }
                ]
            })))
            .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("replace_match"));
    }

    #[tokio::test]
    async fn apply_patch_replace_all_updates_all_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_after_read(&dir, "alpha\nalpha\n");
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "edits": [
                    {
                        "old_text": "alpha",
                        "new_text": "beta",
                        "replace_all": true
                    }
                ]
            })))
            .await;

        assert!(result.success, "{result:?}");
        let updated = std::fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert_eq!(updated, "beta\nbeta\n");
    }

    #[tokio::test]
    async fn apply_patch_requires_the_file_to_have_been_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "alpha\n").unwrap();

        let tool = ApplyPatchTool::new(dir.path().to_path_buf());
        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "edits": [{ "old_text": "alpha", "new_text": "beta" }]
            })))
            .await;

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("`main.rs` has not been read"), "{error}");
        assert!(error.contains("read_file"), "{error}");
        let untouched = std::fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert_eq!(untouched, "alpha\n");
    }

    #[tokio::test]
    async fn apply_patch_rejects_a_file_changed_since_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_after_read(&dir, "alpha\n");
        std::fs::write(dir.path().join("main.rs"), "alpha and more\n").unwrap();

        let result = tool
            .execute(&make_call(serde_json::json!({
                "path": "main.rs",
                "edits": [{ "old_text": "alpha", "new_text": "beta" }]
            })))
            .await;

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(
            error.contains("`main.rs` changed since it was last read"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn apply_patch_keeps_the_file_editable_after_a_patch() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool_after_read(&dir, "alpha beta\n");

        for (old_text, new_text) in [("alpha", "ALPHA"), ("beta", "BETA")] {
            let result = tool
                .execute(&make_call(serde_json::json!({
                    "path": "main.rs",
                    "edits": [{ "old_text": old_text, "new_text": new_text }]
                })))
                .await;
            assert!(result.success, "{result:?}");
        }

        let updated = std::fs::read_to_string(dir.path().join("main.rs")).unwrap();
        assert_eq!(updated, "ALPHA BETA\n");
    }
}
