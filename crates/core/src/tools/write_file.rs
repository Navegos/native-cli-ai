use std::sync::Arc;

use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::ledger::FileLedger;

pub struct WriteFileTool {
    workspace_root: std::path::PathBuf,
    ledger: Arc<FileLedger>,
}

impl WriteFileTool {
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
impl ToolExecutor for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file inside the workspace".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let content = call.input["content"].as_str().unwrap_or("");
        let full_path = self.workspace_root.join(path);

        let parent = match full_path.parent() {
            Some(parent) => parent,
            None => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("Invalid write path".into()),
                };
            }
        };

        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create parent directories: {err}")),
            };
        }

        let canonical_parent = match parent.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve parent path: {err}")),
                };
            }
        };

        let workspace_root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        if !canonical_parent.starts_with(&workspace_root) {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace".into()),
            };
        }

        // Overwriting needs a fresh read of the file; new files are exempt.
        if tokio::fs::metadata(&full_path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            let existing = match tokio::fs::read_to_string(&full_path).await {
                Ok(existing) => existing,
                Err(err) => {
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read existing file: {err}")),
                    };
                }
            };
            if let Err(message) = self.ledger.check_edit(&full_path, &existing) {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(message),
                };
            }
        }

        match tokio::fs::write(&full_path, content).await {
            Ok(()) => {
                self.ledger.record(&full_path, content);
                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: format!("Wrote {}", full_path.display()),
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

    fn make_call(path: &str, content: &str) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            input: serde_json::json!({ "path": path, "content": content }),
        }
    }

    #[tokio::test]
    async fn write_file_creates_a_new_file_without_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        let result = tool.execute(&make_call("sub/new.txt", "fresh\n")).await;

        assert!(result.success, "{result:?}");
        let written = std::fs::read_to_string(dir.path().join("sub/new.txt")).unwrap();
        assert_eq!(written, "fresh\n");
    }

    #[tokio::test]
    async fn write_file_may_overwrite_what_it_wrote_earlier() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        let first = tool.execute(&make_call("a.txt", "one\n")).await;
        let second = tool.execute(&make_call("a.txt", "two\n")).await;

        assert!(first.success && second.success, "{first:?} {second:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[tokio::test]
    async fn write_file_rejects_overwriting_an_unread_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "precious\n").unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        let result = tool.execute(&make_call("a.txt", "clobbered\n")).await;

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("`a.txt` has not been read"), "{error}");
        assert!(error.contains("read_file"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "precious\n"
        );
    }

    #[tokio::test]
    async fn write_file_overwrites_after_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old\n").unwrap();
        let ledger = Arc::new(FileLedger::new(dir.path().to_path_buf()));
        ledger.record(&path, "old\n");
        let tool = WriteFileTool::with_ledger(dir.path().to_path_buf(), ledger);

        let result = tool.execute(&make_call("a.txt", "new\n")).await;

        assert!(result.success, "{result:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
    }

    #[tokio::test]
    async fn write_file_rejects_a_file_changed_since_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old\n").unwrap();
        let ledger = Arc::new(FileLedger::new(dir.path().to_path_buf()));
        ledger.record(&path, "old\n");
        std::fs::write(&path, "changed elsewhere\n").unwrap();
        let tool = WriteFileTool::with_ledger(dir.path().to_path_buf(), ledger);

        let result = tool.execute(&make_call("a.txt", "new\n")).await;

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(
            error.contains("`a.txt` changed since it was last read"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed elsewhere\n"
        );
    }

    #[tokio::test]
    async fn write_file_refuses_to_overwrite_a_non_text_file_it_cannot_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        let result = tool.execute(&make_call("blob.bin", "text\n")).await;

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("Failed to read existing file")
        );
        assert_eq!(std::fs::read(&path).unwrap(), [0xff_u8, 0xfe, 0x00, 0x01]);
    }

    #[tokio::test]
    async fn write_file_rejects_paths_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let tool = WriteFileTool::new(workspace);

        let result = tool.execute(&make_call("../escape.txt", "nope\n")).await;

        assert!(!result.success);
        assert_eq!(result.error.unwrap(), "Path is outside the workspace");
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_accepts_a_workspace_root_reached_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let tool = WriteFileTool::new(link);

        let result = tool.execute(&make_call("a.txt", "ok\n")).await;

        assert!(result.success, "{result:?}");
        assert_eq!(std::fs::read_to_string(real.join("a.txt")).unwrap(), "ok\n");
    }
}
