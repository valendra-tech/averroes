//! macOS desktop screenshots and input automation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

use crate::tool::{Result, Tool, ToolContext, ToolError, ToolRegistry, ToolResult};

const MAX_CAPTURE_MAPPINGS: usize = 64;

pub fn register(registry: &ToolRegistry) {
    let state = Arc::new(DesktopState::default());
    registry.register(DesktopScreenshotTool::new(state.clone()));
    registry.register(DesktopInputTool::new(state));
}

#[derive(Debug, Clone, Copy, Serialize)]
struct DesktopRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl DesktopRect {
    fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy)]
struct CaptureMapping {
    bounds: DesktopRect,
    image_width: u32,
    image_height: u32,
}

impl CaptureMapping {
    fn image_to_desktop(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.bounds.x + x * self.bounds.width / f64::from(self.image_width),
            self.bounds.y + y * self.bounds.height / f64::from(self.image_height),
        )
    }
}

#[derive(Default)]
struct DesktopState {
    captures: parking_lot::Mutex<HashMap<String, CaptureMapping>>,
}

impl DesktopState {
    fn remember(&self, session_id: &str, mapping: CaptureMapping) {
        if session_id.trim().is_empty() {
            return;
        }

        let mut captures = self.captures.lock();
        if !captures.contains_key(session_id) && captures.len() >= MAX_CAPTURE_MAPPINGS {
            if let Some(oldest) = captures.keys().next().cloned() {
                captures.remove(&oldest);
            }
        }
        captures.insert(session_id.to_owned(), mapping);
    }

    fn mapping(&self, session_id: &str) -> Option<CaptureMapping> {
        self.captures.lock().get(session_id).copied()
    }
}

pub struct DesktopScreenshotTool {
    state: Arc<DesktopState>,
}

impl DesktopScreenshotTool {
    fn new(state: Arc<DesktopState>) -> Self {
        Self { state }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotParams {
    action: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    display_index: Option<usize>,
    #[serde(default)]
    window_id: Option<u32>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    application: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    include_system: bool,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for DesktopScreenshotTool {
    fn name(&self) -> &str {
        "desktop_screenshot"
    }

    fn description(&self) -> &str {
        "Inspect and capture the macOS desktop. Lists displays or windows and captures the whole desktop, one display, one isolated window, or a rectangular region as a PNG image. After a capture, desktop_input can use image coordinates directly."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["capture", "list_displays", "list_windows"],
                    "description": "Whether to take a screenshot or inspect available capture targets"
                },
                "target": {
                    "type": "string",
                    "enum": ["desktop", "display", "window", "region"],
                    "description": "Capture target. Defaults to display"
                },
                "display_index": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based display index from list_displays. Defaults to 1"
                },
                "window_id": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Window id from list_windows; required when target is window"
                },
                "x": {
                    "type": "number",
                    "description": "Global desktop X coordinate for a region"
                },
                "y": {
                    "type": "number",
                    "description": "Global desktop Y coordinate for a region"
                },
                "width": {
                    "type": "number",
                    "minimum": 1,
                    "description": "Region width in desktop points"
                },
                "height": {
                    "type": "number",
                    "minimum": 1,
                    "description": "Region height in desktop points"
                },
                "application": {
                    "type": "string",
                    "description": "Optional case-insensitive application filter for list_windows"
                },
                "title": {
                    "type": "string",
                    "description": "Optional case-insensitive title filter for list_windows"
                },
                "include_system": {
                    "type": "boolean",
                    "description": "Include menus, overlays, and other nonstandard window layers"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "description": "Maximum windows to return. Defaults to 50"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: ScreenshotParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;

        #[cfg(target_os = "macos")]
        {
            macos::execute_screenshot(&self.state, &ctx.session_id, params).await
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (ctx, params);
            Err(unsupported(self.name()))
        }
    }
}

pub struct DesktopInputTool {
    state: Arc<DesktopState>,
}

impl DesktopInputTool {
    fn new(state: Arc<DesktopState>) -> Self {
        Self { state }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputParams {
    action: String,
    #[serde(default)]
    coordinate_space: Option<String>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    end_x: Option<f64>,
    #[serde(default)]
    end_y: Option<f64>,
    #[serde(default)]
    button: Option<String>,
    #[serde(default)]
    delta_x: Option<f64>,
    #[serde(default)]
    delta_y: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    repeat: Option<u32>,
    #[serde(default)]
    interval_ms: Option<u64>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

#[async_trait]
impl Tool for DesktopInputTool {
    fn name(&self) -> &str {
        "desktop_input"
    }

    fn description(&self) -> &str {
        "Control the macOS desktop with the mouse and keyboard. Move, click, double-click, right-click, drag, scroll, type Unicode text, or press keys and shortcuts. Coordinates default to the latest desktop_screenshot image in this conversation, so screenshot pixels can be used directly."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["move", "click", "double_click", "right_click", "drag", "scroll", "type", "press"],
                    "description": "Desktop input operation"
                },
                "coordinate_space": {
                    "type": "string",
                    "enum": ["last_screenshot", "global"],
                    "description": "Interpret x/y as pixels in the latest screenshot or as global macOS desktop points. Defaults to last_screenshot when available"
                },
                "x": {
                    "type": "number",
                    "description": "Start or click X coordinate"
                },
                "y": {
                    "type": "number",
                    "description": "Start or click Y coordinate"
                },
                "end_x": {
                    "type": "number",
                    "description": "Drag destination X coordinate"
                },
                "end_y": {
                    "type": "number",
                    "description": "Drag destination Y coordinate"
                },
                "button": {
                    "type": "string",
                    "enum": ["left", "right", "middle"],
                    "description": "Mouse button. Defaults to left"
                },
                "delta_x": {
                    "type": "number",
                    "description": "Horizontal scroll pixels; positive scrolls right"
                },
                "delta_y": {
                    "type": "number",
                    "description": "Vertical scroll pixels; positive scrolls down"
                },
                "text": {
                    "type": "string",
                    "description": "Unicode text to type into the focused control"
                },
                "key": {
                    "type": "string",
                    "description": "Key or shortcut, for example Enter, ArrowDown, Cmd+L, or Cmd+Shift+P"
                },
                "repeat": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "Number of key presses. Defaults to 1"
                },
                "interval_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 1000,
                    "description": "Delay between typed characters. Defaults to 0"
                },
                "duration_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 5000,
                    "description": "Drag duration. Defaults to 250ms"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: &ToolContext, params: &Value) -> Result<ToolResult> {
        let params: InputParams =
            serde_json::from_value(params.clone()).map_err(|error| ToolError::InvalidParams {
                tool: self.name().into(),
                message: error.to_string(),
            })?;

        #[cfg(target_os = "macos")]
        {
            let state = self.state.clone();
            let session_id = ctx.session_id.clone();
            tokio::task::spawn_blocking(move || macos::execute_input(&state, &session_id, params))
                .await
                .map_err(|error| ToolError::Execution {
                    tool: self.name().into(),
                    message: format!("Desktop input worker failed: {error}"),
                })?
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (ctx, params);
            Err(unsupported(self.name()))
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn unsupported(tool: &str) -> ToolError {
    ToolError::Execution {
        tool: tool.into(),
        message: "Desktop capture and input are currently available only on macOS".into(),
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use base64::Engine as _;
    use core_foundation::{
        array::CFArray,
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::{CFDictionary, CFDictionaryRef},
        number::CFNumber,
        string::{CFString, CFStringRef},
    };
    use core_graphics::{
        access::ScreenCaptureAccess,
        display::CGDisplay,
        event::{
            CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
            EventField, KeyCode, ScrollEventUnit,
        },
        event_source::{CGEventSource, CGEventSourceStateID},
        geometry::{CGPoint, CGRect},
        window::{
            kCGNullWindowID, kCGWindowAlpha, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
            kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
            kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID, CGWindowListCopyWindowInfo,
        },
    };
    use std::{thread, time::Duration};
    use tokio::process::Command;
    use uuid::Uuid;

    const MAX_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;
    const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(60);
    const ACCESSIBILITY_SETTINGS_URL: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

    #[derive(Debug, Clone, Serialize)]
    struct DisplayInfo {
        index: usize,
        id: u32,
        main: bool,
        bounds: DesktopRect,
        pixel_width: u64,
        pixel_height: u64,
        scale_x: f64,
        scale_y: f64,
    }

    #[derive(Debug, Clone, Serialize)]
    struct WindowInfo {
        id: u32,
        pid: i32,
        application: String,
        title: Option<String>,
        bounds: DesktopRect,
        layer: i32,
        onscreen: bool,
        z_order: usize,
    }

    struct CaptureSpec {
        arguments: Vec<String>,
        bounds: DesktopRect,
        label: String,
        details: Value,
    }

    pub(super) async fn execute_screenshot(
        state: &DesktopState,
        session_id: &str,
        params: ScreenshotParams,
    ) -> Result<ToolResult> {
        let action = params.action.trim().to_ascii_lowercase();
        match action.as_str() {
            "list_displays" => list_displays_result(),
            "list_windows" => list_windows_result(&params),
            "capture" => capture(state, session_id, &params).await,
            _ => Err(invalid(format!(
                "Unknown desktop_screenshot action '{}'. Use capture, list_displays, or list_windows",
                params.action
            ))),
        }
    }

    fn list_displays_result() -> Result<ToolResult> {
        let displays = displays()?;
        let content = serde_json::to_string_pretty(&displays).map_err(serialization_error)?;
        Ok(ToolResult::ok(content).with_metadata(json!({
            "action": "list_displays",
            "count": displays.len()
        })))
    }

    fn list_windows_result(params: &ScreenshotParams) -> Result<ToolResult> {
        let application = normalized_filter(params.application.as_deref());
        let title = normalized_filter(params.title.as_deref());
        let limit = params.limit.unwrap_or(50).clamp(1, 200);
        let windows = windows(params.include_system)?
            .into_iter()
            .filter(|window| {
                application
                    .as_ref()
                    .is_none_or(|filter| window.application.to_ascii_lowercase().contains(filter))
                    && title.as_ref().is_none_or(|filter| {
                        window
                            .title
                            .as_deref()
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .contains(filter)
                    })
            })
            .take(limit)
            .collect::<Vec<_>>();
        let content = serde_json::to_string_pretty(&windows).map_err(serialization_error)?;
        Ok(ToolResult::ok(content).with_metadata(json!({
            "action": "list_windows",
            "count": windows.len(),
            "limit": limit
        })))
    }

    async fn capture(
        state: &DesktopState,
        session_id: &str,
        params: &ScreenshotParams,
    ) -> Result<ToolResult> {
        let access = ScreenCaptureAccess;
        if !access.preflight() && !access.request() {
            return Err(execution(
                "Screen Recording permission is required. Enable Averroes in System Settings > Privacy & Security > Screen Recording, then retry the capture",
            ));
        }

        let spec = capture_spec(params)?;
        let path =
            std::env::temp_dir().join(format!("averroes-desktop-capture-{}.png", Uuid::new_v4()));
        let mut command = Command::new("/usr/sbin/screencapture");
        command
            .kill_on_drop(true)
            .arg("-x")
            .arg("-t")
            .arg("png")
            .args(&spec.arguments)
            .arg(&path);

        let output = match tokio::time::timeout(SCREENSHOT_TIMEOUT, command.output()).await {
            Ok(result) => result.map_err(|error| {
                execution(format!("Failed to start macOS screen capture: {error}"))
            })?,
            Err(_) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(execution("Screen capture timed out after 60 seconds"));
            }
        };
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&path).await;
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(execution(if stderr.is_empty() {
                "macOS could not capture that target. Check Screen Recording permission and confirm the display, window, or region still exists".into()
            } else {
                format!("macOS screen capture failed: {stderr}")
            }));
        }

        let bytes_result = tokio::fs::read(&path).await;
        let _ = tokio::fs::remove_file(&path).await;
        let bytes = bytes_result
            .map_err(|error| execution(format!("Failed to read captured PNG: {error}")))?;
        if bytes.len() > MAX_SCREENSHOT_BYTES {
            return Err(execution(format!(
                "Screenshot is {} MiB; the limit is {} MiB. Capture one display, window, or a smaller region",
                bytes.len().div_ceil(1024 * 1024),
                MAX_SCREENSHOT_BYTES / 1024 / 1024
            )));
        }
        let (image_width, image_height) = png_dimensions(&bytes).ok_or_else(|| {
            execution("macOS returned an invalid or empty PNG. Check Screen Recording permission")
        })?;
        let mapping = CaptureMapping {
            bounds: spec.bounds,
            image_width,
            image_height,
        };
        state.remember(session_id, mapping);

        Ok(ToolResult::ok(format!(
            "Captured {} as a {}x{} PNG. desktop_input now defaults to coordinates from this image.",
            spec.label, image_width, image_height
        ))
        .with_image(
            "image/png",
            base64::engine::general_purpose::STANDARD.encode(bytes),
        )
        .with_metadata(json!({
            "action": "capture",
            "target": spec.details,
            "media_type": "image/png",
            "image_width": image_width,
            "image_height": image_height,
            "desktop_bounds": spec.bounds,
            "coordinate_space": "last_screenshot"
        })))
    }

    fn capture_spec(params: &ScreenshotParams) -> Result<CaptureSpec> {
        let target = params
            .target
            .as_deref()
            .unwrap_or("display")
            .trim()
            .to_ascii_lowercase();
        match target.as_str() {
            "desktop" => {
                let displays = displays()?;
                let bounds = union_bounds(displays.iter().map(|display| display.bounds))
                    .ok_or_else(|| execution("No active displays were found"))?;
                Ok(CaptureSpec {
                    arguments: vec![region_argument(bounds)],
                    bounds,
                    label: "the complete desktop".into(),
                    details: json!({ "kind": "desktop" }),
                })
            }
            "display" => {
                let index = params.display_index.unwrap_or(1);
                let display = displays()?
                    .into_iter()
                    .find(|display| display.index == index)
                    .ok_or_else(|| invalid(format!("Display index {index} does not exist")))?;
                Ok(CaptureSpec {
                    arguments: vec![format!("-D{index}")],
                    bounds: display.bounds,
                    label: format!("display {index}"),
                    details: json!({ "kind": "display", "display_index": index, "display_id": display.id }),
                })
            }
            "window" => {
                let id = params
                    .window_id
                    .ok_or_else(|| invalid("window_id is required when target is window"))?;
                let window = windows(true)?
                    .into_iter()
                    .find(|window| window.id == id)
                    .ok_or_else(|| invalid(format!("Window id {id} is not currently visible")))?;
                let description = window
                    .title
                    .as_deref()
                    .filter(|title| !title.is_empty())
                    .map(|title| format!("{} — {title}", window.application))
                    .unwrap_or_else(|| window.application.clone());
                Ok(CaptureSpec {
                    arguments: vec!["-o".into(), format!("-l{id}")],
                    bounds: window.bounds,
                    label: format!("window {id} ({description})"),
                    details: json!({
                        "kind": "window",
                        "window_id": id,
                        "application": window.application,
                        "title": window.title
                    }),
                })
            }
            "region" => {
                let bounds = DesktopRect {
                    x: required_finite(params.x, "x", target.as_str())?,
                    y: required_finite(params.y, "y", target.as_str())?,
                    width: required_positive(params.width, "width", target.as_str())?,
                    height: required_positive(params.height, "height", target.as_str())?,
                };
                Ok(CaptureSpec {
                    arguments: vec![region_argument(bounds)],
                    bounds,
                    label: "the selected desktop region".into(),
                    details: json!({ "kind": "region" }),
                })
            }
            _ => Err(invalid(format!(
                "Unknown capture target '{target}'. Use desktop, display, window, or region"
            ))),
        }
    }

    fn displays() -> Result<Vec<DisplayInfo>> {
        let ids = CGDisplay::active_displays().map_err(|code| {
            execution(format!(
                "Could not enumerate active displays (CoreGraphics error {code})"
            ))
        })?;
        Ok(ids
            .into_iter()
            .enumerate()
            .map(|(offset, id)| {
                let display = CGDisplay::new(id);
                let rect = display.bounds();
                let bounds = desktop_rect(rect);
                let pixel_width = display.pixels_wide();
                let pixel_height = display.pixels_high();
                DisplayInfo {
                    index: offset + 1,
                    id,
                    main: display.is_main(),
                    bounds,
                    pixel_width,
                    pixel_height,
                    scale_x: pixel_width as f64 / bounds.width,
                    scale_y: pixel_height as f64 / bounds.height,
                }
            })
            .collect())
    }

    fn windows(include_system: bool) -> Result<Vec<WindowInfo>> {
        let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
        let raw = unsafe { CGWindowListCopyWindowInfo(options, kCGNullWindowID) };
        if raw.is_null() {
            return Err(execution("Could not enumerate desktop windows"));
        }
        let list: CFArray<CFDictionary<CFString, CFType>> =
            unsafe { CFArray::wrap_under_create_rule(raw) };

        let number_key = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
        let pid_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
        let owner_key = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerName) };
        let title_key = unsafe { CFString::wrap_under_get_rule(kCGWindowName) };
        let bounds_key = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
        let layer_key = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
        let alpha_key = unsafe { CFString::wrap_under_get_rule(kCGWindowAlpha) };
        let onscreen_key = unsafe { CFString::wrap_under_get_rule(kCGWindowIsOnscreen) };

        Ok(list
            .iter()
            .enumerate()
            .filter_map(|(z_order, dictionary)| {
                let id = dictionary_number(&dictionary, &number_key)?.to_i64()? as u32;
                let pid = dictionary_number(&dictionary, &pid_key)?.to_i32()?;
                let application = dictionary_string(&dictionary, &owner_key)?;
                let title = dictionary_string(&dictionary, &title_key)
                    .filter(|title| !title.trim().is_empty());
                let layer = dictionary_number(&dictionary, &layer_key)?.to_i32()?;
                let alpha = dictionary_number(&dictionary, &alpha_key)
                    .and_then(|number| number.to_f64())
                    .unwrap_or(1.0);
                let onscreen = dictionary
                    .find(&onscreen_key)
                    .and_then(|value| value.downcast::<CFBoolean>())
                    .map(bool::from)
                    .unwrap_or(true);
                let bounds_dictionary = dictionary.find(&bounds_key)?.downcast::<CFDictionary>()?;
                let bounds = desktop_rect(CGRect::from_dict_representation(&bounds_dictionary)?);
                if !onscreen
                    || alpha <= 0.0
                    || bounds.width < 2.0
                    || bounds.height < 2.0
                    || (!include_system && layer != 0)
                {
                    return None;
                }
                Some(WindowInfo {
                    id,
                    pid,
                    application,
                    title,
                    bounds,
                    layer,
                    onscreen,
                    z_order,
                })
            })
            .collect())
    }

    fn dictionary_number(
        dictionary: &CFDictionary<CFString, CFType>,
        key: &CFString,
    ) -> Option<CFNumber> {
        dictionary.find(key).and_then(|value| value.downcast())
    }

    fn dictionary_string(
        dictionary: &CFDictionary<CFString, CFType>,
        key: &CFString,
    ) -> Option<String> {
        dictionary
            .find(key)
            .and_then(|value| value.downcast::<CFString>())
            .map(|value| value.to_string())
    }

    fn desktop_rect(rect: CGRect) -> DesktopRect {
        DesktopRect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }

    fn union_bounds(rectangles: impl IntoIterator<Item = DesktopRect>) -> Option<DesktopRect> {
        rectangles.into_iter().fold(None, |union, rect| {
            Some(match union {
                None => rect,
                Some(union) => {
                    let left = union.x.min(rect.x);
                    let top = union.y.min(rect.y);
                    let right = (union.x + union.width).max(rect.x + rect.width);
                    let bottom = (union.y + union.height).max(rect.y + rect.height);
                    DesktopRect {
                        x: left,
                        y: top,
                        width: right - left,
                        height: bottom - top,
                    }
                }
            })
        })
    }

    fn region_argument(bounds: DesktopRect) -> String {
        format!(
            "-R{},{},{},{}",
            bounds.x.round() as i64,
            bounds.y.round() as i64,
            bounds.width.round() as u64,
            bounds.height.round() as u64
        )
    }

    fn normalized_filter(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
    }

    fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
        const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
        if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
            return None;
        }
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        (width > 0 && height > 0).then_some((width, height))
    }

    pub(super) fn execute_input(
        state: &DesktopState,
        session_id: &str,
        params: InputParams,
    ) -> Result<ToolResult> {
        ensure_input_access()?;
        let action = params.action.trim().to_ascii_lowercase();
        let source = event_source()?;
        match action.as_str() {
            "move" => {
                let point = required_point(state, session_id, &params, params.x, params.y, "move")?;
                move_mouse(&source, point)?;
                Ok(input_result("move", json!({ "x": point.x, "y": point.y })))
            }
            "click" | "double_click" | "right_click" => {
                let point = optional_point(state, session_id, &params, params.x, params.y)?
                    .unwrap_or(current_pointer(&source)?);
                let button = if action == "right_click" {
                    CGMouseButton::Right
                } else {
                    mouse_button(params.button.as_deref())?
                };
                let count = if action == "double_click" { 2 } else { 1 };
                click_mouse(&source, point, button, count)?;
                Ok(input_result(
                    &action,
                    json!({ "x": point.x, "y": point.y, "click_count": count }),
                ))
            }
            "drag" => {
                let start = required_point(state, session_id, &params, params.x, params.y, "drag")?;
                let end = required_point(
                    state,
                    session_id,
                    &params,
                    params.end_x,
                    params.end_y,
                    "drag destination",
                )?;
                let duration_ms = params.duration_ms.unwrap_or(250).min(5_000);
                drag_mouse(
                    &source,
                    start,
                    end,
                    mouse_button(params.button.as_deref())?,
                    duration_ms,
                )?;
                Ok(input_result(
                    "drag",
                    json!({
                        "start": { "x": start.x, "y": start.y },
                        "end": { "x": end.x, "y": end.y },
                        "duration_ms": duration_ms
                    }),
                ))
            }
            "scroll" => {
                if let Some(point) = optional_point(
                    state,
                    session_id,
                    &params,
                    params.x,
                    params.y,
                )? {
                    move_mouse(&source, point)?;
                }
                let delta_x = params.delta_x.unwrap_or(0.0);
                let delta_y = params.delta_y.unwrap_or(600.0);
                scroll(&source, delta_x, delta_y)?;
                Ok(input_result(
                    "scroll",
                    json!({ "delta_x": delta_x, "delta_y": delta_y }),
                ))
            }
            "type" => {
                let text = params
                    .text
                    .as_deref()
                    .ok_or_else(|| invalid("text is required for the type action"))?;
                if text.is_empty() {
                    return Err(invalid("text cannot be empty for the type action"));
                }
                let interval_ms = params.interval_ms.unwrap_or(0).min(1_000);
                type_text(&source, text, interval_ms)?;
                Ok(input_result(
                    "type",
                    json!({ "characters": text.chars().count(), "interval_ms": interval_ms }),
                ))
            }
            "press" => {
                let shortcut = params
                    .key
                    .as_deref()
                    .ok_or_else(|| invalid("key is required for the press action"))?;
                let repeat = params.repeat.unwrap_or(1).clamp(1, 50);
                press_key(&source, shortcut, repeat)?;
                Ok(input_result(
                    "press",
                    json!({ "key": shortcut, "repeat": repeat }),
                ))
            }
            _ => Err(invalid(format!(
                "Unknown desktop_input action '{}'. Use move, click, double_click, right_click, drag, scroll, type, or press",
                params.action
            ))),
        }
    }

    fn ensure_input_access() -> Result<()> {
        if unsafe { CGPreflightPostEventAccess() } {
            Ok(())
        } else {
            request_accessibility_prompt();
            if unsafe { CGRequestPostEventAccess() } || unsafe { CGPreflightPostEventAccess() } {
                return Ok(());
            }
            open_accessibility_settings();
            Err(execution(
                "Accessibility permission is required for desktop input. macOS requires you to enable Averroes manually in the Accessibility settings that were opened, then retry",
            ))
        }
    }

    fn request_accessibility_prompt() {
        let prompt_key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) };
        let prompt_value = CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(prompt_key, prompt_value)]);
        // The prompt is asynchronous, so a false return here is expected. The
        // post-event preflight above remains the authority for tool execution.
        let _ = unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) };
    }

    fn open_accessibility_settings() {
        if let Err(error) = std::process::Command::new("/usr/bin/open")
            .arg(ACCESSIBILITY_SETTINGS_URL)
            .spawn()
        {
            crate::observability::diagnostics::record(
                crate::observability::diagnostics::DiagnosticLevel::Warning,
                "desktop.accessibility",
                format!("Could not open the macOS Accessibility settings: {error}"),
            );
        }
    }

    fn event_source() -> Result<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| execution("Could not create a macOS input event source"))
    }

    fn required_point(
        state: &DesktopState,
        session_id: &str,
        params: &InputParams,
        x: Option<f64>,
        y: Option<f64>,
        action: &str,
    ) -> Result<CGPoint> {
        optional_point(state, session_id, params, x, y)?
            .ok_or_else(|| invalid(format!("x and y are required for the {action} action")))
    }

    fn optional_point(
        state: &DesktopState,
        session_id: &str,
        params: &InputParams,
        x: Option<f64>,
        y: Option<f64>,
    ) -> Result<Option<CGPoint>> {
        let (x, y) = match (x, y) {
            (None, None) => return Ok(None),
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => (x, y),
            (Some(_), Some(_)) => return Err(invalid("Desktop coordinates must be finite")),
            _ => return Err(invalid("x and y must be supplied together")),
        };

        let mapping = state.mapping(session_id);
        let coordinate_space = params
            .coordinate_space
            .as_deref()
            .unwrap_or(if mapping.is_some() {
                "last_screenshot"
            } else {
                "global"
            })
            .trim()
            .to_ascii_lowercase();
        let (x, y) = match coordinate_space.as_str() {
            "global" => (x, y),
            "last_screenshot" => mapping
                .ok_or_else(|| {
                    invalid(
                        "No screenshot coordinate map exists for this conversation. Capture first or use coordinate_space='global'",
                    )
                })?
                .image_to_desktop(x, y),
            _ => {
                return Err(invalid(format!(
                    "Unknown coordinate_space '{coordinate_space}'. Use last_screenshot or global"
                )))
            }
        };
        validate_desktop_point(x, y)?;
        Ok(Some(CGPoint::new(x, y)))
    }

    fn validate_desktop_point(x: f64, y: f64) -> Result<()> {
        if displays()?
            .iter()
            .any(|display| display.bounds.contains(x, y))
        {
            Ok(())
        } else {
            Err(invalid(format!(
                "Mapped point ({x:.1}, {y:.1}) is outside every active display"
            )))
        }
    }

    fn current_pointer(source: &CGEventSource) -> Result<CGPoint> {
        CGEvent::new(source.clone())
            .map(|event| event.location())
            .map_err(|_| execution("Could not read the current mouse position"))
    }

    fn move_mouse(source: &CGEventSource, point: CGPoint) -> Result<()> {
        post_mouse_event(
            source,
            CGEventType::MouseMoved,
            point,
            CGMouseButton::Left,
            0,
        )
    }

    fn click_mouse(
        source: &CGEventSource,
        point: CGPoint,
        button: CGMouseButton,
        count: i64,
    ) -> Result<()> {
        move_mouse(source, point)?;
        let (down, up, _) = mouse_event_types(button);
        for click_index in 1..=count {
            post_mouse_event(source, down, point, button, click_index)?;
            thread::sleep(Duration::from_millis(18));
            post_mouse_event(source, up, point, button, click_index)?;
            if click_index < count {
                thread::sleep(Duration::from_millis(70));
            }
        }
        Ok(())
    }

    fn drag_mouse(
        source: &CGEventSource,
        start: CGPoint,
        end: CGPoint,
        button: CGMouseButton,
        duration_ms: u64,
    ) -> Result<()> {
        move_mouse(source, start)?;
        let (down, up, dragged) = mouse_event_types(button);
        post_mouse_event(source, down, start, button, 1)?;
        let steps = (duration_ms / 16).clamp(2, 120);
        let delay = Duration::from_millis((duration_ms / steps).max(1));
        for step in 1..=steps {
            let progress = step as f64 / steps as f64;
            let point = CGPoint::new(
                start.x + (end.x - start.x) * progress,
                start.y + (end.y - start.y) * progress,
            );
            post_mouse_event(source, dragged, point, button, 1)?;
            thread::sleep(delay);
        }
        post_mouse_event(source, up, end, button, 1)
    }

    fn post_mouse_event(
        source: &CGEventSource,
        event_type: CGEventType,
        point: CGPoint,
        button: CGMouseButton,
        click_state: i64,
    ) -> Result<()> {
        let event = CGEvent::new_mouse_event(source.clone(), event_type, point, button)
            .map_err(|_| execution("Could not create a macOS mouse event"))?;
        if click_state > 0 {
            event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_state);
        }
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn mouse_event_types(button: CGMouseButton) -> (CGEventType, CGEventType, CGEventType) {
        match button {
            CGMouseButton::Left => (
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
                CGEventType::LeftMouseDragged,
            ),
            CGMouseButton::Right => (
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
                CGEventType::RightMouseDragged,
            ),
            CGMouseButton::Center => (
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
                CGEventType::OtherMouseDragged,
            ),
        }
    }

    fn mouse_button(value: Option<&str>) -> Result<CGMouseButton> {
        match value.unwrap_or("left").trim().to_ascii_lowercase().as_str() {
            "left" => Ok(CGMouseButton::Left),
            "right" => Ok(CGMouseButton::Right),
            "middle" | "center" => Ok(CGMouseButton::Center),
            value => Err(invalid(format!(
                "Unknown mouse button '{value}'. Use left, right, or middle"
            ))),
        }
    }

    fn scroll(source: &CGEventSource, delta_x: f64, delta_y: f64) -> Result<()> {
        if !delta_x.is_finite() || !delta_y.is_finite() {
            return Err(invalid("Scroll deltas must be finite"));
        }
        let horizontal = bounded_i32(-delta_x);
        let vertical = bounded_i32(-delta_y);
        let event = CGEvent::new_scroll_event(
            source.clone(),
            ScrollEventUnit::PIXEL,
            2,
            vertical,
            horizontal,
            0,
        )
        .map_err(|_| execution("Could not create a macOS scroll event"))?;
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    fn bounded_i32(value: f64) -> i32 {
        value.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32
    }

    fn type_text(source: &CGEventSource, text: &str, interval_ms: u64) -> Result<()> {
        let chunks = unicode_chunks(text, if interval_ms > 0 { 1 } else { 20 });
        for (index, chunk) in chunks.iter().enumerate() {
            let down = CGEvent::new_keyboard_event(source.clone(), 0, true)
                .map_err(|_| execution("Could not create a macOS keyboard event"))?;
            down.set_string(chunk);
            down.post(CGEventTapLocation::HID);
            let up = CGEvent::new_keyboard_event(source.clone(), 0, false)
                .map_err(|_| execution("Could not create a macOS keyboard event"))?;
            up.set_string(chunk);
            up.post(CGEventTapLocation::HID);
            if interval_ms > 0 && index + 1 < chunks.len() {
                thread::sleep(Duration::from_millis(interval_ms));
            }
        }
        Ok(())
    }

    fn unicode_chunks(text: &str, max_utf16_units: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut chunk = String::new();
        let mut units = 0;
        for character in text.chars() {
            let character_units = character.len_utf16();
            if units + character_units > max_utf16_units && !chunk.is_empty() {
                chunks.push(std::mem::take(&mut chunk));
                units = 0;
            }
            chunk.push(character);
            units += character_units;
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        chunks
    }

    fn press_key(source: &CGEventSource, shortcut: &str, repeat: u32) -> Result<()> {
        let (keycode, flags) = parse_shortcut(shortcut)?;
        for index in 0..repeat {
            let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
                .map_err(|_| execution("Could not create a macOS keyboard event"))?;
            down.set_flags(flags);
            down.post(CGEventTapLocation::HID);
            thread::sleep(Duration::from_millis(12));
            let up = CGEvent::new_keyboard_event(source.clone(), keycode, false)
                .map_err(|_| execution("Could not create a macOS keyboard event"))?;
            up.set_flags(flags);
            up.post(CGEventTapLocation::HID);
            if index + 1 < repeat {
                thread::sleep(Duration::from_millis(35));
            }
        }
        Ok(())
    }

    fn parse_shortcut(shortcut: &str) -> Result<(CGKeyCode, CGEventFlags)> {
        let mut parts = shortcut
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let key = parts
            .pop()
            .filter(|key| !key.is_empty())
            .ok_or_else(|| invalid("key cannot be empty"))?;
        let mut flags = CGEventFlags::CGEventFlagNull;
        for modifier in parts {
            flags |= match modifier.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "meta" => CGEventFlags::CGEventFlagCommand,
                "ctrl" | "control" => CGEventFlags::CGEventFlagControl,
                "alt" | "option" => CGEventFlags::CGEventFlagAlternate,
                "shift" => CGEventFlags::CGEventFlagShift,
                "fn" | "function" => CGEventFlags::CGEventFlagSecondaryFn,
                _ => return Err(invalid(format!("Unknown shortcut modifier '{modifier}'"))),
            };
        }
        let keycode = key_code(key)
            .ok_or_else(|| invalid(format!("Unknown key '{key}' in shortcut '{shortcut}'")))?;
        Ok((keycode, flags))
    }

    fn key_code(key: &str) -> Option<CGKeyCode> {
        let normalized = key.trim().to_ascii_lowercase().replace(['_', ' '], "");
        Some(match normalized.as_str() {
            "enter" | "return" => KeyCode::RETURN,
            "tab" => KeyCode::TAB,
            "space" | "spacebar" => KeyCode::SPACE,
            "backspace" | "delete" => KeyCode::DELETE,
            "forwarddelete" => KeyCode::FORWARD_DELETE,
            "escape" | "esc" => KeyCode::ESCAPE,
            "command" | "cmd" | "meta" => KeyCode::COMMAND,
            "shift" => KeyCode::SHIFT,
            "capslock" => KeyCode::CAPS_LOCK,
            "option" | "alt" => KeyCode::OPTION,
            "control" | "ctrl" => KeyCode::CONTROL,
            "function" | "fn" => KeyCode::FUNCTION,
            "volumeup" => KeyCode::VOLUME_UP,
            "volumedown" => KeyCode::VOLUME_DOWN,
            "mute" => KeyCode::MUTE,
            "home" => KeyCode::HOME,
            "end" => KeyCode::END,
            "pageup" => KeyCode::PAGE_UP,
            "pagedown" => KeyCode::PAGE_DOWN,
            "left" | "arrowleft" | "leftarrow" => KeyCode::LEFT_ARROW,
            "right" | "arrowright" | "rightarrow" => KeyCode::RIGHT_ARROW,
            "up" | "arrowup" | "uparrow" => KeyCode::UP_ARROW,
            "down" | "arrowdown" | "downarrow" => KeyCode::DOWN_ARROW,
            "f1" => KeyCode::F1,
            "f2" => KeyCode::F2,
            "f3" => KeyCode::F3,
            "f4" => KeyCode::F4,
            "f5" => KeyCode::F5,
            "f6" => KeyCode::F6,
            "f7" => KeyCode::F7,
            "f8" => KeyCode::F8,
            "f9" => KeyCode::F9,
            "f10" => KeyCode::F10,
            "f11" => KeyCode::F11,
            "f12" => KeyCode::F12,
            "f13" => KeyCode::F13,
            "f14" => KeyCode::F14,
            "f15" => KeyCode::F15,
            "f16" => KeyCode::F16,
            "f17" => KeyCode::F17,
            "f18" => KeyCode::F18,
            "f19" => KeyCode::F19,
            "f20" => KeyCode::F20,
            "a" => 0,
            "s" => 1,
            "d" => 2,
            "f" => 3,
            "h" => 4,
            "g" => 5,
            "z" => 6,
            "x" => 7,
            "c" => 8,
            "v" => 9,
            "b" => 11,
            "q" => 12,
            "w" => 13,
            "e" => 14,
            "r" => 15,
            "y" => 16,
            "t" => 17,
            "1" => 18,
            "2" => 19,
            "3" => 20,
            "4" => 21,
            "6" => 22,
            "5" => 23,
            "=" | "equal" => 24,
            "9" => 25,
            "7" => 26,
            "-" | "minus" => 27,
            "8" => 28,
            "0" => 29,
            "]" | "rightbracket" => 30,
            "o" => 31,
            "u" => 32,
            "[" | "leftbracket" => 33,
            "i" => 34,
            "p" => 35,
            "l" => 37,
            "j" => 38,
            "'" | "quote" => 39,
            "k" => 40,
            ";" | "semicolon" => 41,
            "\\" | "backslash" => 42,
            "," | "comma" => 43,
            "/" | "slash" => 44,
            "n" => 45,
            "m" => 46,
            "." | "period" => 47,
            "`" | "backtick" => 50,
            _ => return None,
        })
    }

    fn input_result(action: &str, details: Value) -> ToolResult {
        ToolResult::ok(format!("Desktop {action} completed")).with_metadata(json!({
            "action": action,
            "details": details
        }))
    }

    fn required_finite(value: Option<f64>, name: &str, target: &str) -> Result<f64> {
        value.filter(|value| value.is_finite()).ok_or_else(|| {
            invalid(format!(
                "{name} is required and must be finite for {target}"
            ))
        })
    }

    fn required_positive(value: Option<f64>, name: &str, target: &str) -> Result<f64> {
        value
            .filter(|value| value.is_finite() && *value >= 1.0)
            .ok_or_else(|| {
                invalid(format!(
                    "{name} is required and must be at least one point for {target}"
                ))
            })
    }

    fn serialization_error(error: serde_json::Error) -> ToolError {
        execution(format!("Could not serialize desktop information: {error}"))
    }

    fn invalid(message: impl Into<String>) -> ToolError {
        ToolError::InvalidParams {
            tool: "desktop".into(),
            message: message.into(),
        }
    }

    fn execution(message: impl Into<String>) -> ToolError {
        ToolError::Execution {
            tool: "desktop".into(),
            message: message.into(),
        }
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: CFStringRef;
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reads_png_dimensions() {
            let mut png = Vec::from(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice());
            png.extend_from_slice(&1920_u32.to_be_bytes());
            png.extend_from_slice(&1080_u32.to_be_bytes());
            assert_eq!(png_dimensions(&png), Some((1920, 1080)));
        }

        #[test]
        fn maps_retina_capture_coordinates_to_desktop_points() {
            let mapping = CaptureMapping {
                bounds: DesktopRect {
                    x: -1440.0,
                    y: 0.0,
                    width: 1440.0,
                    height: 900.0,
                },
                image_width: 2880,
                image_height: 1800,
            };
            assert_eq!(mapping.image_to_desktop(1440.0, 900.0), (-720.0, 450.0));
        }

        #[test]
        fn parses_common_shortcuts() {
            let (key, flags) = parse_shortcut("Cmd+Shift+P").unwrap();
            assert_eq!(key, 35);
            assert!(flags.contains(CGEventFlags::CGEventFlagCommand));
            assert!(flags.contains(CGEventFlags::CGEventFlagShift));
        }

        #[test]
        fn unicode_chunks_do_not_split_surrogate_pairs() {
            assert_eq!(
                unicode_chunks("1234567890123456789🙂x", 20),
                vec!["1234567890123456789", "🙂x"]
            );
            assert_eq!(unicode_chunks("a🙂b", 1), vec!["a", "🙂", "b"]);
        }

        #[test]
        fn unions_displays_with_negative_origins() {
            let union = union_bounds([
                DesktopRect {
                    x: -1920.0,
                    y: 0.0,
                    width: 1920.0,
                    height: 1080.0,
                },
                DesktopRect {
                    x: 0.0,
                    y: -200.0,
                    width: 2560.0,
                    height: 1440.0,
                },
            ])
            .unwrap();
            assert_eq!(union.x, -1920.0);
            assert_eq!(union.y, -200.0);
            assert_eq!(union.width, 4480.0);
            assert_eq!(union.height, 1440.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_requires_confirmation_but_screenshot_is_read_only() {
        let state = Arc::new(DesktopState::default());
        let screenshot = DesktopScreenshotTool::new(state.clone());
        let input = DesktopInputTool::new(state);

        assert!(screenshot.is_read_only());
        assert!(!screenshot.requires_confirmation());
        assert!(input.requires_confirmation());
        assert!(!input.is_read_only());
    }
}
