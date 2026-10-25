# macOS background input (prototype)

Status: prototype on `feat/macos-background-input`, branched from `main` at 750fc2a. Uncommitted.

## Goal

Let nca act on a running macOS app, pressing a control or typing text, without bringing that app to the front. Each action reports whether the frontmost app changed, so a focus steal shows up in the tool output.

## Non-goals

- Mouse clicks at coordinates. No pid-targeted mouse path is verified yet.
- Private SkyLight calls (`SLEventPostToPid`, `SLPSPostEventRecordTo`). desktopctl and trycua use them. They are private APIs and can change with any macOS update. Revisit only if the public routes fail.
- Windows and Linux. Each needs its own plan.
- Committing, pull requests, or installing to `/usr/local/bin`.

## Evidence

- desktopctl (MIT) logs its `SLEventPostToPid` dispatch as `sent_unverified` ([desktopctl](https://github.com/yaroshevych/desktopctl)). Sending an event does not prove delivery, so this plan checks effects directly.
- trycua's window-internals notes say Chromium's renderer accepts events that arrive through SkyLight and rejects ones that don't. Expect public events to fail on Chromium and Electron. That is a known limit, not a bug in this code.
- Public routes, not yet measured here: `AXUIElementPerformAction(kAXPressAction)` is the macOS analog of UIA `Invoke`. `CGEventPostToPid` posts a key event to one process.

## Decisions

1. Public APIs only: Accessibility (`AXPress`, `AXValue` reads) and `CGEventPostToPid`. The lockfile already has `core-foundation` 0.10.1, so this adds an edge, not a new package.
2. One tool, `app_input`, with actions `list`, `press`, and `type_text`. Its name is not on the read-only or file-edit lists in `approval.rs`, so Default and AcceptEdits ask, and Plan and DontAsk deny. No approval code changes.
3. Output says "posted" for typing, not "typed". The tool cannot confirm delivery.
4. Compiled and registered on macOS only.
5. Bounded walk: at most 1,500 elements, depth 12, and a 2 s messaging timeout on the app element.
6. The live test is `#[ignore]`. It launches Calculator with `open -g -n`, which does not bring it to the front, targets only that pid, and quits it afterward.

## Verification

- Unit tests: `cargo test -p nca-core app_input`.
- `cargo fmt --all -- --check` and `cargo clippy --workspace -- -D warnings`.
- Live: `cargo test -p nca-core calculator_responds -- --ignored --nocapture`. Pass means `press 5` shows `5`, `type_text 3` shows `53`, and the frontmost app never changes. It needs Accessibility permission for the terminal that runs it.
- Not verified: Linux and Windows builds. The module is compiled out there.

## Results

Measured on this Mac (Darwin 27.0.0) on 2026-10-25. Calculator was launched with `open -g -n` and quit by pid afterward.

- `press 5` through AXPress: the display showed `5`, and the frontmost app did not change.
- `type_text 3` through `CGEventPostToPid`: the display showed `53`, and the frontmost app did not change.
- The tool's JSON path (`list`) reported `value="53"` and `Frontmost app unchanged`.
- No Calculator process was left running.
- `cargo test --workspace` passes. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` are clean.
- Dia 1.54.2 (`company.thebrowser.dia`): `open -g -a Dia` opened facebook.com in a new tab, and the frontmost app did not change. Nothing was typed or clicked in Dia. A 12-second `screencapture -v -l` recording of the window covered the page load, and a `screencapture -l` screenshot saved the loaded page.
- `screencapture -v` stops cleanly on SIGINT. A 2.85-second test file was valid H.264, so a test can end a recording on demand.

Not measured: `app_input` press and typing in Dia or any other Chromium app, mouse clicks, Safari, TextEdit, Electron, and apps with two instances under the same name. `find_app` takes the first match in window order.

## Risks and open questions

- Accessibility permission. Without it the live test cannot pass, and the tool returns an error that names the setting.
- Calculator is one app and one control. `AXPress` did not bring it to the front in that run. Other apps may behave differently.
- Frontmost detection uses the first on-screen layer-0 window. It is a proxy, not an API for the active app.
- Electron and Chromium apps may need the private path.

## Follow-ups (not in this change)

- Mouse clicks.
- Private SkyLight path, only if the public routes fail.
- Windows: UIA with `AutoSetFocus` off, plus the `EnableWindow` shield for XAML and Chromium hosts ([Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation2-get_autosetfocus)).
- Linux: AT-SPI `DoAction`, after a focus test. Wayland needs the portal consent flow.
