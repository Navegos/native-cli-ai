# macOS window recording (prototype)

Status: prototype on `feat/macos-background-input` (same worktree), uncommitted.

## Goal

Let nca record one app window to a .mov while the window is not the frontmost app. Start and stop are separate tool calls, so an end-to-end test can record the steps it runs.

## Non-goals

- Audio, display or region recording, and minimized or other-Space windows. They have no on-screen window to capture.
- Headless CI recording. It needs a logged-in desktop and Screen Recording permission.
- Claude's recording behavior. I haven't checked it.

## Evidence

- Dia 1.54.2 recorded 12.0 s with `screencapture -x -V 12 -l <window id>` while T3 Code was frontmost. The first frame is the page still loading, so the recording covers the window's content, not the frontmost app.
- SIGINT stops an open-ended `screencapture -v` cleanly. A 2.85-second test file was valid H.264.
- Not tested: a fully covered window, a minimized window, and a window that moves to another Space.

## Decisions

1. Use the system `screencapture` tool. A ScreenCaptureKit binding would be much larger, and this path already works.
2. One tool, `screen_record`, with actions `start` and `stop`. `start` finds the on-screen window by app name and retries for up to 3 s. It checks Screen Recording permission with `CGPreflightScreenCaptureAccess`, which doesn't prompt. It runs an open-ended `screencapture -v -l`, and a watchdog thread sends SIGINT at the time limit: 60 s by default, 600 s at most. `stop` sends SIGINT and waits for the file to finish. Timed `-V` recordings are not used, because a SIGINT to one produced no file.
   - Recordings still running when the tool is dropped are finished, so none keeps capturing after nca exits.
3. Files go to the project's nca data folder by default: `~/.local/share/ncacli/workspaces/<id>/recordings/`, next to the project's sessions. The project folder stays clean. `[recording] dir` in the config can point elsewhere. A relative path is taken from the project root, so `.nca/recordings` keeps files in the project. An absolute path is used as-is. Filenames use only a safe slug of the app name.
4. Neither action activates an app. Both report whether the frontmost app changed, as `app_input` does.
5. Approval: the name is not on the read-only or file-edit lists in `approval.rs`, so it asks by default and is denied in Plan mode. Recordings show screen content, so they should be gated.
6. Compiled and registered on macOS only.

## Verification

- Unit tests: argument checks, the filename slug (no path traversal), unknown ids, and the approval gate.
- Live test (ignored): opens Calculator with `open -g -n` and runs three recordings of about 2 s while another app stays frontmost. One ends by its time limit with no stop call. One ends with `stop`. One is still running when the tool is dropped. Each file is checked with ffprobe, and the frontmost app must not change. The test refuses to run if Calculator is already open, and it quits only the instance it launched.

## Risks

- Screen Recording permission for the app that runs nca.
- A recording shows whatever the window shows, including account data. Keep `.nca/recordings` out of shared artifacts.
- The window lookup can miss transiently, as it did with Dia, so `start` retries.

## Results

Measured on this Mac (Darwin 27.0.0) on 2026-10-25.

- A timed `-V` recording produced no file when it was sent SIGINT from the nca test process. Open-ended `-v` recordings finish their file on SIGINT, so the tool uses those.
- Calculator's window, recorded in the background, finished three ways. The time limit ended a 1.75 s file (142 KB) with no stop call. `stop` ended a recording at 2.0 s (143 KB). Dropping the tool ended a 1.70 s file (86 KB). Lengths come from ffprobe, except the 2.0 s, which is elapsed time. The frontmost app did not change in any run.
- A frame from the first background recording shows Calculator's window content, not a blank frame.
- Bug found and fixed in this run: recording ids restarted at 1 in each session, so a later session collided with an existing file, and `screencapture` refused to save over it. Ids now include a timestamp, and `start` refuses to overwrite an existing file.
- Recordings now go to the project's data folder by default, `~/.local/share/ncacli/workspaces/<id>/recordings/`. The project folder got no files. `[recording] dir` moves them: a relative path is taken from the project root, and an absolute path is used as-is.
- Window choice: the third recording failed in three runs. The first on-screen window the lookup returned for Calculator was a small helper window, and `screencapture -l` exited with status 1 on it with no message. The tool now records the app's largest on-screen window, and the start message gives its size. With that change, all three recordings passed.
- A `.log` file is kept only when screencapture writes to it, so a successful recording leaves only the `.mov`.
- Immediate stop: a stop 0.1 s after start ended screencapture with `signal: 2 (SIGINT)` and no file, because an interrupt during startup kills it. `stop` now waits at least 2 s before interrupting. The live test covers this case and produces a 2 s file.
- Stop retries: `stop` sends SIGINT again every 3 s, up to four times, then kills screencapture. One CLI run never got a response to SIGINT, and the stop was killed after 12 s. After that kill, screen capture failed for about a minute, even for still images, until it recovered on its own. I haven't confirmed that the kill caused the stall, so the kill step needs a second look.
- CLI run: `nca run` with `screen_record` allowed completed start and stop, the reply gave the saved path, and the start result said `Frontmost app unchanged`. The run used the kenari.id endpoint, because the MiniMax key in the config is rejected with `invalid api key`.
- History: the REPL history moved to the data folder (`<data>/workspaces/<id>/history`). Startup copies `.nca/.history` once and leaves the original. The project's copy was then removed after a byte-for-byte check.
- Tests: `execute()` and the tool registry now have tests for both tools. Argument errors come back as failed results, and the macOS tools are registered.
- GitHub in Dia, through the CLI: `nca run` with `app_input` (`open_url`, `https://github.com`) and `screen_record` (`screenshot`, `delay` 5) allowed. Both succeeded, and both results said `Frontmost app unchanged`. The screenshot is 2940x1662 and shows the GitHub dashboard in Dia's window. The run used the kenari.id endpoint, as above.
- `open_url` accepts only http and https addresses. `screenshot` uses the same window lookup and permission check as recordings.
- Not measured: fully covered or minimized windows, a window moved to another Space, and recordings near the 600 s limit.
