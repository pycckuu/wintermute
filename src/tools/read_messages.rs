//! read_messages tool: read WhatsApp message history.

use super::ToolError;

/// Read recent messages from a WhatsApp contact.
///
/// Requires WhatsApp adapter (Phase 3). Currently returns a stub error.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] if `contact` is missing, or
/// [`ToolError::ExecutionFailed`] because WhatsApp is not yet configured.
pub async fn read_messages(input: &serde_json::Value) -> Result<String, ToolError> {
    let _contact = input
        .get("contact")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: contact".to_owned()))?;

    Err(ToolError::ExecutionFailed(
        "WhatsApp not yet configured. Set up WhatsApp first.".to_owned(),
    ))
}
