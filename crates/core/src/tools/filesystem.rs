use std::sync::Arc;

use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ledger::FileLedger;
use super::{MAX_TOOL_OUTPUT_CHARS, ToolExecutor};

/// Lines returned per call when `end_line` is omitted.
const DEFAULT_WINDOW_LINES: usize = 2000;
/// Room kept under the output cap for the paging header.
const HEADER_RESERVE_CHARS: usize = 200;

pub struct ReadFileTool {
    workspace_root: std::path::PathBuf,
    ledger: Arc<FileLedger>,
}

impl ReadFileTool {
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
impl ToolExecutor for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file from the workspace. Returns at most 2000 lines per call; \
                use start_line and end_line to read a range or to page through a larger file. \
                When only part of the file is returned, the first line of the output is a header \
                like [showing lines 1-2000 of 5000. Pass start_line to continue.]"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to workspace root"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to return, 1-based and inclusive. Defaults to 1."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line to return, inclusive. Defaults to start_line + 1999, capped at the end of the file."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let (start_line, end_line) = match parse_line_range(&call.input) {
            Ok(range) => range,
            Err(message) => return failure(call, message),
        };

        let workspace_root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let full_path = self.workspace_root.join(path);

        // Verify the path stays inside the workspace
        let canonical = match full_path.canonicalize() {
            Ok(canonical) if canonical.starts_with(&workspace_root) => canonical,
            _ => return failure(call, "Path is outside the workspace"),
        };

        let content = match tokio::fs::read_to_string(&canonical).await {
            Ok(content) => content,
            Err(e) => return failure(call, format!("Failed to read file: {e}")),
        };

        match read_window(&content, start_line, end_line) {
            Ok(output) => {
                // The whole file counts as read, even when only a range is returned.
                self.ledger.record(&canonical, &content);
                ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output,
                    error: None,
                }
            }
            Err(message) => failure(call, format!("{path}: {message}")),
        }
    }
}

fn failure(call: &ToolCall, error: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(error.into()),
    }
}

/// Parse optional `start_line` (default 1) and `end_line`.
fn parse_line_range(input: &serde_json::Value) -> Result<(usize, Option<usize>), String> {
    let start_line = optional_line(input, "start_line")?.unwrap_or(1);
    let end_line = optional_line(input, "end_line")?;
    if let Some(end_line) = end_line
        && end_line < start_line
    {
        return Err(format!(
            "end_line ({end_line}) must not be before start_line ({start_line})"
        ));
    }
    Ok((start_line, end_line))
}

/// A positive line number given as an integer, a whole-number float, or a numeric string.
fn optional_line(input: &serde_json::Value, key: &str) -> Result<Option<usize>, String> {
    let value = &input[key];
    if value.is_null() {
        return Ok(None);
    }
    let parsed = value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| n.fract() == 0.0 && *n >= 1.0)
                .map(|n| n as u64)
        })
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<u64>().ok()));
    match parsed {
        Some(line) if line >= 1 => Ok(Some(usize::try_from(line).unwrap_or(usize::MAX))),
        _ => Err(format!(
            "{key} must be a positive integer (lines are numbered from 1)"
        )),
    }
}

/// Cut `content` down to the requested lines, adding a header when it is only part of the file.
/// The window also stops before the tool output cap, so the header's last line is accurate.
/// `start_line` is at least 1 and `end_line`, when given, is not before it.
fn read_window(
    content: &str,
    start_line: usize,
    end_line: Option<usize>,
) -> Result<String, String> {
    let total = count_lines(content);
    if total == 0 && start_line == 1 {
        return Ok(String::new());
    }
    if start_line > total {
        return Err(format!(
            "start_line {start_line} is past the end of the file, which has {total} lines"
        ));
    }

    let start_offset: usize = content
        .split_inclusive('\n')
        .take(start_line - 1)
        .map(str::len)
        .sum();
    let max_lines = end_line.map_or(DEFAULT_WINDOW_LINES, |end| {
        end.saturating_sub(start_line).saturating_add(1)
    });
    let budget = MAX_TOOL_OUTPUT_CHARS - HEADER_RESERVE_CHARS;

    let mut end_offset = start_offset;
    let mut shown = 0_usize;
    let mut chars = 0_usize;
    for line in content[start_offset..]
        .split_inclusive('\n')
        .take(max_lines)
    {
        let line_chars = line.chars().count();
        // Always return at least one line, however long.
        if shown > 0 && chars + line_chars > budget {
            break;
        }
        chars += line_chars;
        end_offset += line.len();
        shown += 1;
    }

    let last_line = start_line + shown - 1;
    let window = &content[start_offset..end_offset];
    if start_line == 1 && last_line == total {
        return Ok(window.to_string());
    }
    Ok(format!(
        "[showing lines {start_line}-{last_line} of {total}. Pass start_line to continue.]\n{window}"
    ))
}

/// Number of lines, counting a final line that lacks a trailing newline.
fn count_lines(content: &str) -> usize {
    let newlines = content.bytes().filter(|&byte| byte == b'\n').count();
    newlines + usize::from(!content.is_empty() && !content.ends_with('\n'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "read_file".into(),
            input,
        }
    }

    fn numbered_lines(count: usize) -> String {
        (1..=count).map(|n| format!("line {n}\n")).collect()
    }

    async fn read(dir: &tempfile::TempDir, input: serde_json::Value) -> ToolResult {
        ReadFileTool::new(dir.path().to_path_buf())
            .execute(&make_call(input))
            .await
    }

    #[tokio::test]
    async fn small_file_is_returned_whole_without_a_header() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let result = read(&dir, serde_json::json!({ "path": "a.txt" })).await;

        assert!(result.success, "{result:?}");
        assert_eq!(result.output, "one\ntwo\nthree\n");
    }

    #[tokio::test]
    async fn whole_file_is_unchanged_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let content = "crlf\r\nno newline at the end";
        std::fs::write(dir.path().join("a.txt"), content).unwrap();

        let result = read(&dir, serde_json::json!({ "path": "a.txt" })).await;

        assert_eq!(result.output, content);
    }

    #[tokio::test]
    async fn empty_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();

        let result = read(&dir, serde_json::json!({ "path": "empty.txt" })).await;

        assert!(result.success, "{result:?}");
        assert_eq!(result.output, "");
    }

    #[tokio::test]
    async fn ranged_read_returns_the_lines_with_a_header() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();

        let result = read(
            &dir,
            serde_json::json!({ "path": "a.txt", "start_line": 2, "end_line": 3 }),
        )
        .await;

        assert!(result.success, "{result:?}");
        assert_eq!(
            result.output,
            "[showing lines 2-3 of 5. Pass start_line to continue.]\ntwo\nthree\n"
        );
    }

    #[tokio::test]
    async fn start_line_alone_reads_to_the_end_of_a_small_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree").unwrap();

        let result = read(
            &dir,
            serde_json::json!({ "path": "a.txt", "start_line": 2 }),
        )
        .await;

        assert_eq!(
            result.output,
            "[showing lines 2-3 of 3. Pass start_line to continue.]\ntwo\nthree"
        );
    }

    #[tokio::test]
    async fn end_line_past_the_end_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

        let result = read(
            &dir,
            serde_json::json!({ "path": "a.txt", "end_line": 500 }),
        )
        .await;

        assert_eq!(result.output, "one\ntwo\n");
    }

    #[tokio::test]
    async fn long_file_is_windowed_to_2000_lines_with_a_header() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big.txt"), numbered_lines(5000)).unwrap();

        let first = read(&dir, serde_json::json!({ "path": "big.txt" })).await;

        assert!(first.success, "{first:?}");
        let (header, body) = first.output.split_once('\n').unwrap();
        assert_eq!(
            header,
            "[showing lines 1-2000 of 5000. Pass start_line to continue.]"
        );
        assert_eq!(body.lines().count(), 2000);
        assert!(body.starts_with("line 1\n"));
        assert!(body.ends_with("line 2000\n"));

        let next = read(
            &dir,
            serde_json::json!({ "path": "big.txt", "start_line": 2001 }),
        )
        .await;
        let (header, body) = next.output.split_once('\n').unwrap();
        assert_eq!(
            header,
            "[showing lines 2001-4000 of 5000. Pass start_line to continue.]"
        );
        assert!(body.starts_with("line 2001\n"));
    }

    #[tokio::test]
    async fn window_stops_before_the_output_cap() {
        let dir = tempfile::tempdir().unwrap();
        // 1000 lines of 100 chars each is far over the 30k cap.
        let content: String = (1..=1000).map(|n| format!("{n:0>99}\n")).collect();
        std::fs::write(dir.path().join("wide.txt"), content).unwrap();

        let result = read(&dir, serde_json::json!({ "path": "wide.txt" })).await;

        assert!(result.success, "{result:?}");
        assert!(result.output.chars().count() < MAX_TOOL_OUTPUT_CHARS);
        let (header, body) = result.output.split_once('\n').unwrap();
        let shown = body.lines().count();
        assert!(shown < 1000 && shown > 200, "shown {shown}");
        assert_eq!(
            header,
            format!("[showing lines 1-{shown} of 1000. Pass start_line to continue.]")
        );
    }

    #[tokio::test]
    async fn one_oversized_line_is_still_returned() {
        let dir = tempfile::tempdir().unwrap();
        let content = format!("{}\nsecond\n", "x".repeat(MAX_TOOL_OUTPUT_CHARS * 2));
        std::fs::write(dir.path().join("long.txt"), content).unwrap();

        let result = read(&dir, serde_json::json!({ "path": "long.txt" })).await;

        assert!(result.success, "{result:?}");
        assert!(
            result
                .output
                .starts_with("[showing lines 1-1 of 2. Pass start_line to continue.]\nxxx")
        );
    }

    #[tokio::test]
    async fn start_line_past_the_end_reports_the_line_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();

        let result = read(
            &dir,
            serde_json::json!({ "path": "a.txt", "start_line": 4 }),
        )
        .await;

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("start_line 4"), "{error}");
        assert!(error.contains("has 3 lines"), "{error}");
    }

    #[tokio::test]
    async fn start_line_past_the_end_of_an_empty_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();

        let result = read(
            &dir,
            serde_json::json!({ "path": "empty.txt", "start_line": 2 }),
        )
        .await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("has 0 lines"));
    }

    #[tokio::test]
    async fn invalid_ranges_are_rejected_with_guidance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();

        for input in [
            serde_json::json!({ "path": "a.txt", "start_line": 0 }),
            serde_json::json!({ "path": "a.txt", "start_line": -3 }),
            serde_json::json!({ "path": "a.txt", "end_line": "soon" }),
            serde_json::json!({ "path": "a.txt", "start_line": 2, "end_line": 1 }),
        ] {
            let result = read(&dir, input.clone()).await;
            assert!(!result.success, "{input}");
            assert!(result.error.unwrap().contains("_line"), "{input}");
        }
    }

    #[tokio::test]
    async fn numeric_strings_and_whole_floats_are_accepted_as_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();

        for (start, end) in [
            (serde_json::json!("2"), serde_json::json!("2")),
            (serde_json::json!(2.0), serde_json::json!(2.0)),
        ] {
            let result = read(
                &dir,
                serde_json::json!({ "path": "a.txt", "start_line": start, "end_line": end }),
            )
            .await;

            assert!(result.success, "{result:?}");
            assert!(result.output.ends_with("\ntwo\n"), "{result:?}");
        }
    }

    #[tokio::test]
    async fn paths_outside_the_workspace_are_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let result = read(&dir, serde_json::json!({ "path": "../elsewhere.txt" })).await;

        assert!(!result.success);
        assert_eq!(result.error.unwrap(), "Path is outside the workspace");
    }

    #[tokio::test]
    async fn successful_reads_record_the_whole_file_in_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let ledger = Arc::new(FileLedger::new(dir.path().to_path_buf()));
        let tool = ReadFileTool::with_ledger(dir.path().to_path_buf(), ledger.clone());

        let rejected = tool
            .execute(&make_call(
                serde_json::json!({ "path": "a.txt", "start_line": 9 }),
            ))
            .await;
        assert!(!rejected.success);
        assert!(ledger.check_edit(&path, "one\ntwo\nthree\n").is_err());

        let ranged = tool
            .execute(&make_call(
                serde_json::json!({ "path": "a.txt", "start_line": 2, "end_line": 2 }),
            ))
            .await;
        assert!(ranged.success, "{ranged:?}");
        assert!(ledger.check_edit(&path, "one\ntwo\nthree\n").is_ok());
    }

    #[test]
    fn definition_describes_the_paging_parameters() {
        let definition = ReadFileTool::new(std::path::PathBuf::from(".")).definition();

        assert_eq!(definition.name, "read_file");
        assert!(definition.description.contains("start_line"));
        assert!(definition.description.contains("end_line"));
        assert!(
            definition
                .description
                .contains("[showing lines 1-2000 of 5000. Pass start_line to continue.]")
        );
        let properties = &definition.parameters["properties"];
        assert_eq!(properties["start_line"]["type"], "integer");
        assert_eq!(properties["end_line"]["type"], "integer");
        assert_eq!(
            definition.parameters["required"],
            serde_json::json!(["path"])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_root_reached_through_a_symlink_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.txt"), "hello\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let result = ReadFileTool::new(link)
            .execute(&make_call(serde_json::json!({ "path": "a.txt" })))
            .await;

        assert!(result.success, "{result:?}");
        assert_eq!(result.output, "hello\n");
    }

    #[test]
    fn count_lines_matches_editor_numbering() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("\n"), 1);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("a\r\nb\r\n"), 2);
    }
}
