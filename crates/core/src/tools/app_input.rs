//! Acts on a running macOS app without bringing it to the front.
//!
//! `list` walks the app's accessibility tree, `press` runs `AXPress` on a
//! control, and `type_text` posts key events to the app's pid. Each action
//! records the frontmost app before and after, so a focus change shows up in
//! the tool output. See `docs/plans/macos-background-input.md`.

use std::collections::VecDeque;
use std::ffi::c_void;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use nca_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::Value;

use super::ToolExecutor;

type AxElementRef = *const c_void;
type AxError = i32;
type CgEventRef = *const c_void;

const AX_SUCCESS: AxError = 0;

const CG_WINDOW_LIST_ALL: u32 = 0;
/// `kCGWindowListOptionOnScreenOnly` | `kCGWindowListExcludeDesktopElements`.
const CG_WINDOW_LIST_ON_SCREEN: u32 = 1 | 16;
const CG_NULL_WINDOW_ID: u32 = 0;

const AX_MESSAGING_TIMEOUT_SECS: f32 = 2.0;
const MAX_ELEMENTS: usize = 1_500;
const MAX_DEPTH: usize = 12;
const DEFAULT_LIST_LIMIT: usize = 200;

#[allow(non_snake_case)]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> AxElementRef;
    fn AXUIElementSetMessagingTimeout(element: AxElementRef, timeout_seconds: f32) -> AxError;
    fn AXUIElementCopyAttributeValue(
        element: AxElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AxError;
    fn AXUIElementCopyActionNames(element: AxElementRef, names: *mut CFArrayRef) -> AxError;
    fn AXUIElementPerformAction(element: AxElementRef, action: CFStringRef) -> AxError;
}

#[allow(non_snake_case)]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        keycode: u16,
        key_down: bool,
    ) -> CgEventRef;
    fn CGEventKeyboardSetUnicodeString(event: CgEventRef, length: usize, chars: *const u16);
    fn CGEventPostToPid(pid: i32, event: CgEventRef);
    /// Reads a CGRect, laid out as four doubles: x, y, width, height.
    fn CGRectMakeWithDictionaryRepresentation(dict: CFTypeRef, rect: *mut [f64; 4]) -> bool;
}

/// Tool that acts on a macOS app in the background. Registered only on macOS.
pub struct AppInputTool;

#[async_trait::async_trait]
impl ToolExecutor for AppInputTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "app_input".into(),
            description: "Act on a running macOS app without bringing it to the front. \
                action=list shows the app's accessibility elements with their titles and values. \
                action=press runs AXPress on the control whose title matches. \
                action=open_url opens an http or https address in the app with open -g, so the app is not brought to the front. \
                action=type_text posts keystrokes to the app's process; it does not confirm delivery. \
                Every result says whether the frontmost app changed. Needs Accessibility permission."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "app": {
                        "type": "string",
                        "description": "Name of a running app with a window, e.g. Calculator"
                    },
                    "action": {
                        "type": "string",
                        "enum": ["list", "press", "type_text", "open_url"]
                    },
                    "title": {
                        "type": "string",
                        "description": "press: title, description, or identifier of the control"
                    },
                    "text": {
                        "type": "string",
                        "description": "type_text: characters to post"
                    },
                    "url": {
                        "type": "string",
                        "description": "open_url: http or https address to open in the app"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "list: most elements to return (default 200)"
                    }
                },
                "required": ["app", "action"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let input = call.input.clone();
        // Accessibility calls block on the target app, so keep them off the async runtime.
        let outcome = tokio::task::spawn_blocking(move || run(&input))
            .await
            .unwrap_or_else(|error| Err(format!("app_input stopped early: {error}")));
        match outcome {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output,
                error: None,
            },
            Err(error) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(error),
            },
        }
    }
}

/// What the model asked for, parsed before any system call so bad input fails fast.
#[derive(Debug, PartialEq)]
enum Request<'a> {
    List { limit: usize },
    Press { title: &'a str },
    TypeText { text: &'a str },
    OpenUrl { url: &'a str },
}

pub(super) fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, String> {
    input[key]
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("`{key}` is required."))
}

/// The `url` argument of `open_url`: an http or https address, so the tool cannot open files
/// or system settings.
fn web_url(input: &Value) -> Result<&str, String> {
    let url = required_str(input, "url")?;
    if url.starts_with("https://") || url.starts_with("http://") {
        Ok(url)
    } else {
        Err("`url` must start with http:// or https://.".into())
    }
}

fn parse(input: &Value) -> Result<(&str, Request<'_>), String> {
    let app = required_str(input, "app")?;
    let request = match required_str(input, "action")? {
        "list" => Request::List {
            limit: input["limit"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(DEFAULT_LIST_LIMIT),
        },
        "press" => Request::Press {
            title: required_str(input, "title")?,
        },
        "type_text" => Request::TypeText {
            text: required_str(input, "text")?,
        },
        "open_url" => Request::OpenUrl {
            url: web_url(input)?,
        },
        other => {
            return Err(format!(
                "Unknown action \"{other}\". Use list, press, type_text, or open_url."
            ));
        }
    };
    Ok((app, request))
}

fn run(input: &Value) -> Result<String, String> {
    let (app, request) = parse(input)?;
    let owner =
        find_app(app).ok_or_else(|| format!("No running app named \"{app}\" has a window."))?;
    let before = frontmost();
    let body = match request {
        Request::List { limit } => format_elements(&owner, &list_elements(owner.pid, limit)?),
        Request::Press { title } => {
            let element = press(owner.pid, title)?;
            format!(
                "Pressed {} \"{}\" in {} (pid {}).",
                element.role,
                element.label(),
                owner.name,
                owner.pid
            )
        }
        Request::TypeText { text } => {
            let posted = type_text(owner.pid, text)?;
            format!(
                "Posted {posted} characters to {} (pid {}). Delivery is not confirmed; check the app.",
                owner.name, owner.pid
            )
        }
        Request::OpenUrl { url } => {
            open_in_background(&owner.name, url)?;
            format!(
                "Opened {url} in {} (pid {}) without bringing it to the front.",
                owner.name, owner.pid
            )
        }
    };
    let after = frontmost();
    Ok(format!(
        "{body}\n{}",
        focus_note(before.as_ref(), after.as_ref())
    ))
}

/// Opens `url` in the named app with `open -g`, which does not bring the app to the front.
fn open_in_background(app: &str, url: &str) -> Result<(), String> {
    let output = std::process::Command::new("open")
        .args(["-g", "-a", app, url])
        .output()
        .map_err(|error| format!("could not run open: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "open could not open {url} in {app}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(super) fn focus_note(before: Option<&Owner>, after: Option<&Owner>) -> String {
    match (before, after) {
        (Some(before), Some(after)) if before.pid != after.pid => format!(
            "WARNING: frontmost app changed from {} to {}.",
            before.name, after.name
        ),
        (Some(before), Some(_)) => format!("Frontmost app unchanged: {}.", before.name),
        _ => "Frontmost app could not be determined.".into(),
    }
}

fn format_elements(owner: &Owner, elements: &[Element]) -> String {
    let mut lines = vec![format!(
        "{} elements in {} (pid {}):",
        elements.len(),
        owner.name,
        owner.pid
    )];
    lines.extend(elements.iter().map(Element::describe));
    lines.join("\n")
}

/// Virtual keycodes for the ANSI digit keys. Other characters go out with keycode 0
/// and their text attached.
fn digit_keycode(ch: char) -> Option<u16> {
    Some(match ch {
        '0' => 0x1D,
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1A,
        '8' => 0x1C,
        '9' => 0x19,
        _ => return None,
    })
}

fn ax_error_name(status: AxError) -> String {
    match status {
        -25200 => "kAXErrorFailure".into(),
        -25204 => "kAXErrorCannotComplete (the app did not answer)".into(),
        -25206 => "kAXErrorActionUnsupported".into(),
        -25211 => "kAXErrorAPIDisabled (Accessibility permission)".into(),
        _ => format!("AXError {status}"),
    }
}

fn require_trust() -> Result<(), String> {
    // SAFETY: takes no arguments and only reads this process's trust state.
    if unsafe { AXIsProcessTrusted() } != 0 {
        return Ok(());
    }
    Err("Accessibility permission is missing for the app that runs nca. Grant it in System Settings > Privacy & Security > Accessibility, then retry.".into())
}

/// A process with a window, as reported by the window server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub pid: i32,
    pub name: String,
}

/// A layer-0 window: its owning process, its window-server id, and its size in points.
struct WindowEntry {
    owner: Owner,
    number: u32,
    width: f64,
    height: f64,
}

/// Layer-0 windows in front-to-back order for the given window-list option.
fn window_entries(option: u32) -> Vec<WindowEntry> {
    // SAFETY: plain integer arguments; a non-null result is a +1 array owned below.
    let raw = unsafe { CGWindowListCopyWindowInfo(option, CG_NULL_WINDOW_ID) };
    if raw.is_null() {
        return Vec::new();
    }
    // SAFETY: `raw` is a +1 CFArray of CFDictionary, now owned by this wrapper.
    let windows = unsafe { CFArray::<CFDictionary<CFString, CFType>>::wrap_under_create_rule(raw) };
    windows
        .iter()
        .filter_map(|window| {
            if dict_i32(&window, "kCGWindowLayer")? != 0 {
                return None;
            }
            let (width, height) = window_size(&window);
            Some(WindowEntry {
                owner: Owner {
                    pid: dict_i32(&window, "kCGWindowOwnerPID")?,
                    name: dict_string(&window, "kCGWindowOwnerName")?,
                },
                number: u32::try_from(dict_i32(&window, "kCGWindowNumber")?).ok()?,
                width,
                height,
            })
        })
        .collect()
}

/// Width and height in points from a window's `kCGWindowBounds`. Zero when they are missing.
fn window_size(window: &CFDictionary<CFString, CFType>) -> (f64, f64) {
    let Some(bounds) = dict_value(window, "kCGWindowBounds") else {
        return (0.0, 0.0);
    };
    // SAFETY: `bounds` is a live CFDictionary for the call, and `rect` has CGRect's layout.
    let mut rect = [0.0_f64; 4];
    let found = unsafe { CGRectMakeWithDictionaryRepresentation(bounds.as_CFTypeRef(), &mut rect) };
    if found {
        (rect[2], rect[3])
    } else {
        (0.0, 0.0)
    }
}

fn window_owners(option: u32) -> Vec<Owner> {
    window_entries(option)
        .into_iter()
        .map(|entry| entry.owner)
        .collect()
}

/// An on-screen window, as found by `main_window_of`.
pub struct WindowRef {
    /// Window-server id, for `screencapture -l`.
    pub id: u32,
    /// Size in points.
    pub width: f64,
    pub height: f64,
}

/// The main on-screen window of the app named `name`: its largest window, so small helper
/// windows do not win. Matches by name, because an app can own several processes.
pub fn main_window_of(name: &str) -> Option<WindowRef> {
    window_entries(CG_WINDOW_LIST_ON_SCREEN)
        .into_iter()
        .filter(|entry| entry.owner.name.eq_ignore_ascii_case(name))
        .reduce(|best, next| {
            if next.width * next.height > best.width * best.height {
                next
            } else {
                best
            }
        })
        .map(|entry| WindowRef {
            id: entry.number,
            width: entry.width,
            height: entry.height,
        })
}

fn dict_value(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<CFType> {
    dict.find(CFString::new(key)).map(|value| value.clone())
}

fn dict_i32(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<i32> {
    dict_value(dict, key)?.downcast::<CFNumber>()?.to_i32()
}

fn dict_string(dict: &CFDictionary<CFString, CFType>, key: &str) -> Option<String> {
    Some(dict_value(dict, key)?.downcast::<CFString>()?.to_string())
}

/// The app with the frontmost on-screen window. A proxy for the active app.
pub fn frontmost() -> Option<Owner> {
    window_owners(CG_WINDOW_LIST_ON_SCREEN).into_iter().next()
}

/// The first window owner named `name`, ignoring case. Includes off-screen windows.
pub fn find_app(name: &str) -> Option<Owner> {
    window_owners(CG_WINDOW_LIST_ALL)
        .into_iter()
        .find(|owner| owner.name.eq_ignore_ascii_case(name))
}

/// One accessibility element, with the fields a model needs to pick a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub role: String,
    pub title: String,
    pub description: String,
    pub identifier: String,
    pub value: String,
}

impl Element {
    fn read(element: &CFType) -> Self {
        Self {
            role: string_attribute(element, "AXRole"),
            title: string_attribute(element, "AXTitle"),
            description: string_attribute(element, "AXDescription"),
            identifier: string_attribute(element, "AXIdentifier"),
            value: string_attribute(element, "AXValue"),
        }
    }

    /// The name a person reads: the title, or the description when there is no title.
    fn label(&self) -> &str {
        if self.title.is_empty() {
            &self.description
        } else {
            &self.title
        }
    }

    fn label_matches(&self, wanted: &str) -> bool {
        [&self.title, &self.description, &self.identifier]
            .iter()
            .any(|field| !field.is_empty() && field.eq_ignore_ascii_case(wanted))
    }

    fn describe(&self) -> String {
        let mut line = self.role.clone();
        for (key, field) in [
            ("title", &self.title),
            ("description", &self.description),
            ("id", &self.identifier),
            ("value", &self.value),
        ] {
            if !field.is_empty() {
                line.push_str(&format!(" {key}=\"{field}\""));
            }
        }
        line
    }
}

/// Copies one attribute. `None` for a missing attribute or an AX error.
fn copy_attribute(element: &CFType, name: &str) -> Option<CFType> {
    let attribute = CFString::new(name);
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: both pointers stay valid for the call; `value` is read only on success.
    let status = unsafe {
        AXUIElementCopyAttributeValue(
            element.as_CFTypeRef(),
            attribute.as_concrete_TypeRef(),
            &mut value,
        )
    };
    if status != AX_SUCCESS || value.is_null() {
        return None;
    }
    // SAFETY: a successful copy returns a +1 reference that this wrapper now owns.
    Some(unsafe { CFType::wrap_under_create_rule(value) })
}

fn string_attribute(element: &CFType, name: &str) -> String {
    copy_attribute(element, name)
        .and_then(|value| value.downcast::<CFString>())
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// Elements listed under `attribute`: `AXWindows` for the app, `AXChildren` below it.
fn child_elements(element: &CFType, attribute: &str) -> Vec<CFType> {
    let Some(value) = copy_attribute(element, attribute) else {
        return Vec::new();
    };
    let Some(children) = value.downcast::<CFArray>() else {
        return Vec::new();
    };
    // SAFETY: the array keeps each element alive; `wrap_under_get_rule` retains its own reference.
    children
        .iter()
        .map(|child| unsafe { CFType::wrap_under_get_rule(*child) })
        .collect()
}

fn supports_press(element: &CFType) -> bool {
    let mut names: CFArrayRef = std::ptr::null();
    // SAFETY: `names` is written only on success, and then this wrapper owns it.
    let status = unsafe { AXUIElementCopyActionNames(element.as_CFTypeRef(), &mut names) };
    if status != AX_SUCCESS || names.is_null() {
        return false;
    }
    // SAFETY: a successful call returns a +1 array of CFString.
    let names = unsafe { CFArray::<CFString>::wrap_under_create_rule(names) };
    names.iter().any(|name| name.to_string() == "AXPress")
}

/// The accessibility root for `pid`, with a messaging timeout so an unresponsive app
/// cannot hang the tool.
fn app_element(pid: i32) -> Result<CFType, String> {
    require_trust()?;
    // SAFETY: plain integer argument; a non-null result is a +1 reference owned below.
    let raw = unsafe { AXUIElementCreateApplication(pid) };
    if raw.is_null() {
        return Err(format!("Could not open accessibility for pid {pid}."));
    }
    // SAFETY: `raw` is a +1 reference, now owned by this wrapper.
    let app = unsafe { CFType::wrap_under_create_rule(raw) };
    // SAFETY: `app` stays alive for the call.
    unsafe { AXUIElementSetMessagingTimeout(raw, AX_MESSAGING_TIMEOUT_SECS) };
    Ok(app)
}

/// Breadth-first walk over the app's windows, bounded by `MAX_ELEMENTS` and `MAX_DEPTH`.
/// Each element comes with its handle so the caller can act on it.
fn walk(app: &CFType) -> Vec<(CFType, Element)> {
    let mut queue: VecDeque<(CFType, usize)> = child_elements(app, "AXWindows")
        .into_iter()
        .map(|window| (window, 1))
        .collect();
    let mut found = Vec::new();
    while let Some((element, depth)) = queue.pop_front() {
        if found.len() >= MAX_ELEMENTS {
            break;
        }
        if depth < MAX_DEPTH {
            queue.extend(
                child_elements(&element, "AXChildren")
                    .into_iter()
                    .map(|child| (child, depth + 1)),
            );
        }
        let summary = Element::read(&element);
        found.push((element, summary));
    }
    found
}

/// Lists up to `limit` accessibility elements of `pid`.
pub fn list_elements(pid: i32, limit: usize) -> Result<Vec<Element>, String> {
    let app = app_element(pid)?;
    Ok(walk(&app)
        .into_iter()
        .take(limit)
        .map(|(_, element)| element)
        .collect())
}

/// Runs `AXPress` on the first pressable element whose label matches `label`.
pub fn press(pid: i32, label: &str) -> Result<Element, String> {
    let app = app_element(pid)?;
    let label = label.trim();
    for (handle, element) in walk(&app) {
        if !element.label_matches(label) || !supports_press(&handle) {
            continue;
        }
        let action = CFString::new("AXPress");
        // SAFETY: `handle` is a live element retained by the walk; `action` outlives the call.
        let status = unsafe {
            AXUIElementPerformAction(handle.as_CFTypeRef(), action.as_concrete_TypeRef())
        };
        return if status == AX_SUCCESS {
            Ok(element)
        } else {
            Err(format!(
                "AXPress on \"{label}\" failed: {}.",
                ax_error_name(status)
            ))
        };
    }
    Err(format!(
        "No pressable control matches \"{label}\". Use action list to see the titles."
    ))
}

/// Posts `text` to `pid` as key events, one press and release per character. Returns
/// the number of characters posted.
pub fn type_text(pid: i32, text: &str) -> Result<usize, String> {
    require_trust()?;
    let mut posted = 0;
    for ch in text.chars() {
        let mut buffer = [0u16; 2];
        let units: &[u16] = ch.encode_utf16(&mut buffer);
        post_key(pid, digit_keycode(ch).unwrap_or(0), units)?;
        posted += 1;
    }
    Ok(posted)
}

/// Posts one key press and release to `pid`, carrying `units` as the typed text.
fn post_key(pid: i32, keycode: u16, units: &[u16]) -> Result<(), String> {
    for key_down in [true, false] {
        // SAFETY: a null source selects the default; a non-null result is a +1 event owned below.
        let raw = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), keycode, key_down) };
        if raw.is_null() {
            return Err("Could not create a keyboard event.".into());
        }
        // SAFETY: `raw` is a +1 reference, now owned by this wrapper until the end of the loop.
        let _event = unsafe { CFType::wrap_under_create_rule(raw) };
        // SAFETY: `units` stays valid for the call, and `raw` is a live event.
        unsafe {
            CGEventKeyboardSetUnicodeString(raw, units.len(), units.as_ptr());
            CGEventPostToPid(pid, raw);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn element(title: &str, description: &str, identifier: &str, value: &str) -> Element {
        Element {
            role: "AXButton".into(),
            title: title.into(),
            description: description.into(),
            identifier: identifier.into(),
            value: value.into(),
        }
    }

    #[tokio::test]
    async fn execute_turns_bad_input_into_a_failed_result() {
        let call = nca_common::tool::ToolCall {
            id: "call-2".into(),
            name: "app_input".into(),
            input: serde_json::json!({"action": "press"}),
        };
        let result = AppInputTool.execute(&call).await;
        assert_eq!(result.call_id, "call-2");
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("`app`"));
    }

    #[test]
    fn parse_requires_app_and_action() {
        assert!(
            parse(&json!({"action": "list"}))
                .unwrap_err()
                .contains("`app`")
        );
        assert!(
            parse(&json!({"app": "Calculator"}))
                .unwrap_err()
                .contains("`action`")
        );
    }

    #[test]
    fn parse_rejects_unknown_action_and_missing_arguments() {
        assert!(
            parse(&json!({"app": "Calculator", "action": "click"}))
                .unwrap_err()
                .contains("Unknown action")
        );
        assert!(
            parse(&json!({"app": "Calculator", "action": "press"}))
                .unwrap_err()
                .contains("`title`")
        );
        assert!(
            parse(&json!({"app": "Calculator", "action": "type_text"}))
                .unwrap_err()
                .contains("`text`")
        );
    }

    #[test]
    fn parse_reads_each_action() {
        let list = json!({"app": "Calculator", "action": "list"});
        assert_eq!(
            parse(&list).unwrap().1,
            Request::List {
                limit: DEFAULT_LIST_LIMIT
            }
        );
        let press = json!({"app": "Calculator", "action": "press", "title": "5"});
        assert_eq!(parse(&press).unwrap().1, Request::Press { title: "5" });
        let typed = json!({"app": "Calculator", "action": "type_text", "text": "53"});
        assert_eq!(parse(&typed).unwrap().1, Request::TypeText { text: "53" });
        let open = json!({"app": "Dia", "action": "open_url", "url": "https://github.com"});
        assert_eq!(
            parse(&open).unwrap().1,
            Request::OpenUrl {
                url: "https://github.com"
            }
        );
    }

    #[test]
    fn parse_rejects_non_web_urls() {
        for url in [
            "file:///etc/passwd",
            "x-apple.systempreferences:",
            "github.com",
        ] {
            let input = json!({"app": "Dia", "action": "open_url", "url": url});
            assert!(parse(&input).unwrap_err().contains("http"), "{url}");
        }
    }

    #[test]
    fn digit_keys_use_their_virtual_keycodes() {
        assert_eq!(digit_keycode('5'), Some(0x17));
        assert_eq!(digit_keycode('0'), Some(0x1D));
        assert_eq!(digit_keycode('+'), None);
        let codes: std::collections::HashSet<u16> = ('0'..='9').filter_map(digit_keycode).collect();
        assert_eq!(codes.len(), 10);
    }

    #[test]
    fn focus_note_reports_a_change_of_frontmost_app() {
        let terminal = Owner {
            pid: 1,
            name: "Terminal".into(),
        };
        let calculator = Owner {
            pid: 2,
            name: "Calculator".into(),
        };
        assert_eq!(
            focus_note(Some(&terminal), Some(&terminal)),
            "Frontmost app unchanged: Terminal."
        );
        assert_eq!(
            focus_note(Some(&terminal), Some(&calculator)),
            "WARNING: frontmost app changed from Terminal to Calculator."
        );
        assert_eq!(
            focus_note(None, Some(&calculator)),
            "Frontmost app could not be determined."
        );
    }

    #[test]
    fn element_label_matches_title_description_or_identifier() {
        let button = element("", "5", "Five", "");
        assert!(button.label_matches("5"));
        assert!(button.label_matches("five"));
        assert!(!button.label_matches("6"));
        assert_eq!(button.label(), "5");
        assert!(!element("", "", "", "").label_matches(""));
    }

    #[test]
    fn describe_lists_only_non_empty_fields() {
        let line = element("Clear", "", "AC", "0").describe();
        assert_eq!(line, "AXButton title=\"Clear\" id=\"AC\" value=\"0\"");
    }

    #[test]
    fn format_elements_puts_a_count_header_first() {
        let owner = Owner {
            pid: 7,
            name: "Calculator".into(),
        };
        let text = format_elements(&owner, &[element("5", "", "", "")]);
        assert_eq!(
            text,
            "1 elements in Calculator (pid 7):\nAXButton title=\"5\""
        );
    }

    fn wait_for_new_owner(name: &str, existing: &[i32]) -> Option<Owner> {
        for _ in 0..150 {
            let found = window_owners(CG_WINDOW_LIST_ALL)
                .into_iter()
                .find(|owner| owner.name == name && !existing.contains(&owner.pid));
            if found.is_some() {
                return found;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    fn check_front(front_pid: i32) -> Result<(), String> {
        match frontmost() {
            Some(owner) if owner.pid == front_pid => Ok(()),
            Some(owner) => Err(format!("frontmost app changed to {}", owner.name)),
            None => Err("frontmost app could not be read".into()),
        }
    }

    fn expect_display(pid: i32, expected: &str, front_pid: i32) -> Result<(), String> {
        let mut values = Vec::new();
        for _ in 0..20 {
            let elements = list_elements(pid, MAX_ELEMENTS)?;
            if elements.iter().any(|element| element.value == expected) {
                return check_front(front_pid);
            }
            values = elements
                .into_iter()
                .map(|element| element.value)
                .filter(|value| !value.is_empty())
                .collect();
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "display never showed {expected}; values seen: {values:?}"
        ))
    }

    fn exercise_calculator(calculator: &Owner) -> Result<(), String> {
        let front = frontmost().ok_or("frontmost app could not be read")?;
        if front.pid == calculator.pid {
            return Err("Calculator was frontmost before the test started".into());
        }
        press(calculator.pid, "5")?;
        expect_display(calculator.pid, "5", front.pid)?;
        type_text(calculator.pid, "3")?;
        expect_display(calculator.pid, "53", front.pid)?;
        // The same state through the tool's JSON interface, which is what the model sees.
        let report = run(&serde_json::json!({
            "app": "Calculator",
            "action": "list",
            "limit": 500
        }))?;
        eprintln!("{report}");
        if !report.contains("value=\"53\"") || !report.contains("Frontmost app unchanged") {
            return Err(format!(
                "tool output missing the display or the focus line:\n{report}"
            ));
        }
        Ok(())
    }

    /// Launches Calculator without activating it, presses 5 and types 3, and checks that
    /// the display shows 53 while the frontmost app never changes. Quits only the instance
    /// it launched. Needs Accessibility permission for the terminal that runs it.
    #[test]
    #[ignore = "launches Calculator and needs Accessibility permission"]
    fn calculator_responds_to_press_and_typing_without_taking_focus() {
        require_trust().expect("grant Accessibility permission to the app that runs cargo test");
        let existing: Vec<i32> = window_owners(CG_WINDOW_LIST_ALL)
            .into_iter()
            .filter(|owner| owner.name == "Calculator")
            .map(|owner| owner.pid)
            .collect();
        let launched = std::process::Command::new("open")
            .args(["-g", "-n", "-a", "Calculator"])
            .status()
            .expect("run open");
        assert!(launched.success(), "open -g -n -a Calculator failed");
        let calculator =
            wait_for_new_owner("Calculator", &existing).expect("a new Calculator window appeared");
        let outcome = exercise_calculator(&calculator);
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &calculator.pid.to_string()])
            .status();
        if let Err(error) = outcome {
            panic!("{error}");
        }
    }
}
