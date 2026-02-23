use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse};

pub async fn execute_action(
    _pid: u32,
    _req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    Err(crate::Error::UiNotAvailable(
        "UI interaction is only supported on macOS".to_string(),
    ))
}
