# Agent Harness and Workflow Hardening

Status: implemented on `dev` and verified (uncommitted working tree). Verification and results are in the Results section. Scope: `nca-core` agent loop, provider stream adapters, built-in tools, and the built-in system prompt. Patterns borrowed from open-source coding agents where they fit (see Sources).

## Evidence: baseline against kenari.id

Baseline binary: unmodified `nca` from `dev` (commit `1a4e57b`). Provider: custom OpenAI-compatible endpoint at `https://kenari.id` (`POST /v1/chat/completions`). Each scenario ran in a throwaway workspace and `NCA_HOME`.

| Scenario | `minimax-m3-1-flash` | `deepseek-v4-1-flash` | `muse-spark-1-3-contributor:free` |
|---|---|---|---|
| math (no tools) | pass | pass | pass |
| read `SECRET=` from a file | pass | pass | **fail**: `Rate limited, retry after 1000ms` |
| edit one value in a file | pass (read, edit, re-read) | pass | **fail**: rate limited |
| count lines of a 5000-line file | pass (used `execute_bash`) | pass | **fail**: rate limited |

Other observations:

- The free muse model returned `finish_reason: length` with empty content when `max_tokens` was 300. Empty output must fail loudly with guidance, not retry the same request.
- DeepSeek returns `reasoning_content` and `reasoning` on tool-call turns. kenari did not reject the follow-up request whether or not that field was echoed back. DeepSeek's thinking mode expects it, so nca should replay it.
- nca assumed a 32k context window for these 1M-context models, so auto-summarize triggers far too early. Follow-up item, not in this change.

Code-level gaps found while reading the harness:

1. `provider/openai_compat.rs`: stream read errors are sent as assistant text (`TextDelta("[custom stream error ...]")`), so a broken stream looks like a finished answer.
2. `provider/openai_compat.rs`: tool calls whose JSON arguments fail to parse are dropped silently. The model never learns its call was discarded.
3. `provider/openai_compat.rs`: `finish_reason` is ignored except for `tool_calls`. `length` cannot be distinguished from `stop`.
4. `provider/openai_compat.rs`: `reasoning_content` is discarded and never replayed.
5. `core/src/agent.rs`: any `ProviderError` from `chat()` aborts the turn (`?`). There is no retry or backoff for rate limits, 5xx, or dropped connections.
6. `core/src/agent.rs`: the empty-response retry replays the identical request, which cannot fix a `length` cutoff.
7. `core/src/agent.rs`: turn budget exhaustion is a hard error with no summary.
8. `core/src/agent.rs`: loop detection counts only consecutive failures. Repeating the same successful call is never caught.
9. `core/src/tools/mod.rs`: tool output is never capped. A large file goes into context whole.
10. `core/src/tools/filesystem.rs`: `read_file` has no line range, so a model cannot page through a large file.
11. Edit tools accept writes to files the model never read in this session, so a stale or blind edit goes through.
12. `core/src/harness.rs`: the built-in prompt hard-codes this repo's rules ("Rust-native only", "MiniMax is the primary provider path") for every workspace. Those rules already live in this repo's `AGENTS.md`.

## Decisions

1. **Generic built-in prompt.** Repo-specific rules move out of the built-in prompt. They stay in `AGENTS.md`, which this repo already loads. This is a deliberate product change and can be reverted.
2. **Typed stream failures.** Stream errors become `StreamChunk::Failed`. They never become assistant text.
3. **Invalid tool calls go back to the model.** Unparseable tool arguments produce an error tool result, so the model can retry the call with valid JSON. An empty argument string means `{}`.
4. **Reasoning is captured and replayed.** `reasoning_content` is stored on the assistant `Message` and echoed back on OpenAI-compatible requests. It is never shown in the transcript.
5. **Length cutoffs fail immediately.** `finish_reason: length` with no visible output fails once, with an actionable message. It is not retried.
6. **Retries for transient errors.** 429, 408, 5xx, and network failures before any output retry with exponential backoff and jitter. Up to 4 attempts. `Retry-After` is honored up to 30s. Each retry emits a `provider_retry` checkpoint so the TUI shows it as a soft "↻" line. Cancellation interrupts the wait.
7. **Doom-loop control.** Three identical consecutive tool calls add a corrective note to the next request, sent to the model but not kept in history. Six stop the turn with an explicit error.
8. **Budget wrap-up.** When the turn budget runs out, one final call is made with tools disabled. It asks for a summary of what was done and what remains.
9. **Verification gate.** If a turn edited files and then tried to finish without any `run_validation` run, the loop adds one reminder and continues. It fires at most once per turn.
10. **Bounded tool output.** Tool output is capped at 30,000 characters, keeping the head and tail with an explicit marker. `read_file` gains `start_line` and `end_line`, and returns at most 2,000 lines when no range is given.
11. **Read before edit.** An existing file must have been read in this session and must be unchanged since. This applies to `edit_file`, `apply_patch`, and `write_file` on existing paths. New files are exempt. `replace_match` validates its own target text and is exempt. Enforced with a per-session content-hash ledger.

## Out of scope

- Context-window lookup from a provider's `/v1/models` for custom endpoints (follow-up).
- Replaying Anthropic thinking blocks, which need signatures.
- Changes to MCP tool handling.

## Verification

- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings`.
- `cargo test --workspace`.
- Live: rerun the baseline matrix against kenari.id, plus a forced-full-read scenario and a `max_tokens=64` length scenario. Expected: the free muse model passes the read, edit, and count scenarios after retries. The length scenario fails once with guidance.

Local build note: until `genai` was removed, on this Mac the Command Line Tools default SDK broke the `aws-lc-sys` link check, and builds needed `SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk`. `aws-lc-sys` is gone from the lockfile now, and builds work without it.

## Results

Verified on 2026-10-25 with the same kenari.id matrix as the baseline. Workspace tests, clippy with `-D warnings`, and `cargo fmt --check` all pass.

| Scenario | `minimax-m3-1-flash` | `deepseek-v4-1-flash` | `muse-spark-1-3-contributor:free` |
|---|---|---|---|
| math (no tools) | pass | pass | pass |
| read `SECRET=` from a file | pass | pass | pass (after 2 retries) |
| edit one value, re-read | pass (verification nudge fired) | pass (nudge fired) | pass (after 2 retries) |
| count lines of a 5000-line file | pass | pass | pass (after 2 retries) |
| page to the last line (new: needs `start_line`) | pass | pass | not completed: the harness's 170 s limit ran out during rate-limit waits |
| `max_tokens=8` (length cutoff) | not run | not run | answered in full, so the cutoff did not reproduce live; covered by a unit test |

Changes behind the results:

- Retries honor `Retry-After` (capped at 30 s) and back off exponentially for 5xx and dropped connections. The free muse tier returns rate limits of 17 to 57 seconds, so three retries recover most turns.
- Read, edit, and count pass on muse after retries. The baseline failed all three on the first 429.
- The verification gate fired on every edit run. Each model then finished with a clean answer.

## Issues from GitHub (madebyaris/native-cli-ai)

- **#64** Windows doubled every keystroke ("hhaalloo"). crossterm reports a key release as well as a press, and the TUI handled both. Release events are now ignored (`tui/terminal.rs::is_actionable_key`). Covered by a unit test.
- **#63** A custom provider could not be cancelled while stalled. The turn only checked the cancel flag between stream chunks. Cancel now also interrupts a pending request (`agent.rs::open_stream`), and provider HTTP clients have a 30 s connect timeout and a 300 s idle-read timeout (`provider.rs::client_builder`). Covered by a test with a provider that never answers.
- **#25** Streams that broke mid-answer looked like finished answers. Mid-stream `error` events (OpenAI-style and Anthropic-style) and read failures are now typed `Failed` chunks. A failure after visible output is an error, not a silent truncation.
- **#58** Mouse capture cannot be turned off, so the terminal cannot select text. Added `[ui] mouse_capture` (default `true`). Turning it off needs keyboard scrolling, so PageUp and PageDown now scroll the transcript.
- **#66** No way to name a session. Added `/rename <name>`. The name is saved with the session and shown by `nca sessions`. The TUI session picker still lists IDs only.
- Multi-byte characters split across network chunks were decoded per chunk and could be corrupted. Both stream adapters now split on the newline byte before decoding.
- Found while testing: session IDs were `session-<micros>-<counter>`, so two processes started in the same microsecond shared a socket path, and one of six parallel runs failed at startup with `File exists`. The process ID is now part of both session ID generators.
- nca assumed a 32k context window for kenari's 1M models. The OpenAI-style catalog parser now reads `context_length` (OpenRouter's and kenari's field) as well as `context_window`, and skips entries without an `id`.

Not addressed in this change, with the reason:
- #65 Bedrock, #20 Alibaba provider, #47 custom tools, #48 tree-sitter edit tools, #21 per-subagent models: each is a feature with its own design, so they stay open.
- #55, #53, #52, #22: the maintainer has already answered them in the issue threads.

## Sources

Checked against primary text where noted. Items without a note rest on issue or PR text, or on third-party captures.

- **Aider** `aider/coders/base_coder.py`: lint after edits, with reflection capped at 3 per request. Verified. Decision 9 is a one-shot version of this.
- **OpenAI Codex** `codex-rs/core/gpt_5_codex_prompt.md`: never revert changes the agent did not make, stop when the worktree drifts, and require approval for destructive git. Fetched, quotes not byte-checked. Decision 1 and the operating principles use these rules.
- **opencode** doom-loop guard: three identical calls trigger a check. Taken from issue and PR text. Its code path was not verified. Decision 7 adds a nudge and a hard stop at six, not an approval prompt.
- **Claude Code** hooks docs: exit code 2 blocks, and silence does not approve. Verified. Relevant to nca hooks, which are a follow-up.
- **vLLM** MiniMax M2 parser: the model emits no opening `<think>` token, so everything before `</think>` is reasoning. Verified in docs. Not implemented; see follow-ups.
- **Cline**: plan and act are separated by tool class. Third-party prompt capture from July 2025, not the current source. nca already enforces this in the permission gate (`approval.rs`, plan mode denies every non-read-only tool), so nothing new is needed.
- **DeepSeek** thinking-mode docs: `reasoning_content` must come back on tool-call turns. Decision 4 implements this. kenari did not enforce it in probes, but DeepSeek's own API expects it.

## Follow-ups (not in this change)

- Route the third identical tool call through approval, as opencode does, instead of only nudging.
- Run `run_validation` automatically after successful edits, as Aider does, which needs project check detection.
- Split MiniMax reasoning at `</think>` if a live model emits inline think tags. kenari's `minimax-m3-1-flash` did not.
- Context window from the provider's `/v1/models` for custom endpoints. nca assumed 32k for models that advertise 1M.
- Claude Code hook exit-code contract (exit 2 blocks, stderr goes to the model) and a Stop hook that gates completion.
- Return post-edit snippets from edit tools, as Cline does, so the model sees the file state it created.
