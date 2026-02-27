//! Unified send_message tool: dispatches to Telegram or WhatsApp.

use std::path::Path;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::agent::TelegramOutbound;
use crate::messaging::outbound_composer::OutboundComposer;
use crate::whatsapp::client::WhatsAppClient;

use super::ToolError;

/// Send a message via Telegram or WhatsApp.
///
/// For Telegram: sends directly (same as old send_telegram).
/// For WhatsApp: requires brief_id, routes through outbound composer with
/// human-like delay, typing indicators, and read receipts.
///
/// # Errors
///
/// Returns [`ToolError::InvalidInput`] for missing fields or invalid channel,
/// or [`ToolError::ExecutionFailed`] if sending fails.
#[allow(clippy::too_many_arguments)]
pub async fn send_message(
    tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
    input: &serde_json::Value,
    workspace_dir: &Path,
    whatsapp_client: Option<&Arc<WhatsAppClient>>,
    outbound_composer: Option<&Arc<OutboundComposer>>,
    memory_pool: &SqlitePool,
) -> Result<String, ToolError> {
    let channel = input
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("telegram");

    match channel {
        "telegram" => send_telegram_direct(tx, user_id, input, workspace_dir).await,
        "whatsapp" => send_whatsapp(input, whatsapp_client, outbound_composer, memory_pool).await,
        other => Err(ToolError::InvalidInput(format!("unknown channel: {other}"))),
    }
}

/// Direct Telegram send (extracted from old send_telegram).
async fn send_telegram_direct(
    tx: &mpsc::Sender<TelegramOutbound>,
    user_id: i64,
    input: &serde_json::Value,
    workspace_dir: &Path,
) -> Result<String, ToolError> {
    let text = input
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: text".to_owned()))?;

    let file = input.get("file").and_then(|v| v.as_str());

    // Map container paths (/workspace/...) to host paths.
    let resolved_file = if let Some(f) = file {
        let relative = f.strip_prefix("/workspace/").ok_or_else(|| {
            ToolError::InvalidInput("file path must start with /workspace/".to_owned())
        })?;
        Some(workspace_dir.join(relative).to_string_lossy().into_owned())
    } else {
        None
    };

    // Validate the file exists and is within the workspace directory.
    if let Some(ref path) = resolved_file {
        let canonical = Path::new(path)
            .canonicalize()
            .map_err(|e| ToolError::InvalidInput(format!("file not accessible: {e}")))?;
        let canonical_workspace = workspace_dir
            .canonicalize()
            .map_err(|e| ToolError::ExecutionFailed(format!("workspace not accessible: {e}")))?;
        if !canonical.starts_with(&canonical_workspace) {
            return Err(ToolError::InvalidInput(
                "file path must be within the workspace directory".to_owned(),
            ));
        }
    }

    let outbound = TelegramOutbound {
        user_id,
        text: Some(text.to_owned()),
        file_path: resolved_file,
        approval_keyboard: None,
    };

    tx.try_send(outbound).map_err(|e| {
        warn!(error = %e, "telegram send failed");
        ToolError::ExecutionFailed(format!("failed to send telegram message: {e}"))
    })?;

    Ok("Message sent to Telegram".to_owned())
}

/// Send a message via WhatsApp through the outbound composer pipeline.
///
/// Full flow:
/// 1. Parse brief_id and text from input
/// 2. Load the brief from SQLite
/// 3. Load conversation history for context
/// 4. Compose message via OutboundComposer (restricted context)
/// 5. If blocked by redactor, return error
/// 6. Send read receipt (mark_read)
/// 7. Calculate human-like delay
/// 8. Send typing indicator
/// 9. Wait for the delay
/// 10. Send text via WhatsAppClient
/// 11. Log to audit trail
/// 12. Return success
async fn send_whatsapp(
    input: &serde_json::Value,
    whatsapp_client: Option<&Arc<WhatsAppClient>>,
    outbound_composer: Option<&Arc<OutboundComposer>>,
    memory_pool: &SqlitePool,
) -> Result<String, ToolError> {
    let wa_client = whatsapp_client.ok_or_else(|| {
        ToolError::ExecutionFailed(
            "WhatsApp client not configured. Enable WhatsApp in config.toml first.".to_owned(),
        )
    })?;

    let composer = outbound_composer.ok_or_else(|| {
        ToolError::ExecutionFailed(
            "Outbound composer not configured. WhatsApp messaging requires brief support."
                .to_owned(),
        )
    })?;

    let brief_id = input
        .get("brief_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ToolError::InvalidInput(
                "WhatsApp messages require a brief_id. Create a brief first with manage_brief."
                    .to_owned(),
            )
        })?;

    let agent_intent = input
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidInput("missing required field: text".to_owned()))?;

    let incoming_text = input.get("incoming_text").and_then(|v| v.as_str());

    // Step 1: Load the brief
    let brief = crate::messaging::brief::load_brief(memory_pool, brief_id)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to load brief: {e}")))?;

    // Step 2: Resolve the contact's WhatsApp JID
    let jid = resolve_jid_for_brief(&brief, memory_pool)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("failed to resolve contact JID: {e}")))?;

    // Step 3: Load conversation history for multi-turn context
    let history =
        crate::messaging::outbound_composer::load_conversation_history(memory_pool, brief_id)
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to load conversation history: {e}"))
            })?;

    // Step 4: Compose message via OutboundComposer (restricted context)
    let composed = composer
        .compose(&brief, &history, incoming_text, agent_intent)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("composition failed: {e}")))?;

    // Step 5: If blocked by redactor, return error
    if composed.blocked {
        let warning_summary: Vec<String> = composed
            .warnings
            .iter()
            .map(|w| w.category.clone())
            .collect();
        let warnings_json = serde_json::to_string(&warning_summary).unwrap_or_default();

        // Log the blocked message to audit trail
        if let Err(e) = crate::messaging::audit::log_outbound(
            memory_pool,
            Some(brief_id),
            &brief.session_id,
            "whatsapp",
            &jid,
            &composed.text,
            "outbound",
            Some(&warnings_json),
            true,
        )
        .await
        {
            warn!(error = %e, "failed to log blocked outbound message");
        }

        return Err(ToolError::ExecutionFailed(format!(
            "Message blocked by privacy redactor: {}",
            warning_summary.join("; ")
        )));
    }

    // Step 6: Send read receipt
    if let Err(e) = wa_client.mark_read(&jid).await {
        debug!(error = %e, "read receipt failed (non-critical)");
    }

    // Step 7: Calculate human-like delay
    let incoming_len = incoming_text.map_or(0, str::len);
    let delay_ms =
        crate::messaging::outbound_composer::human_like_delay_ms(incoming_len, composed.text.len());

    // Step 8: Send typing indicator
    if let Err(e) = wa_client.send_typing(&jid).await {
        debug!(error = %e, "typing indicator failed (non-critical)");
    }

    // Step 9: Wait for human-like delay
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

    // Step 10: Send the message
    wa_client
        .send_text(&jid, &composed.text)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("WhatsApp send failed: {e}")))?;

    // Step 11: Log to audit trail
    let warning_summary: Option<String> = if composed.warnings.is_empty() {
        None
    } else {
        let summaries: Vec<String> = composed
            .warnings
            .iter()
            .map(|w| w.category.clone())
            .collect();
        serde_json::to_string(&summaries).ok()
    };

    if let Err(e) = crate::messaging::audit::log_outbound(
        memory_pool,
        Some(brief_id),
        &brief.session_id,
        "whatsapp",
        &jid,
        &composed.text,
        "outbound",
        warning_summary.as_deref(),
        false,
    )
    .await
    {
        warn!(error = %e, "failed to log outbound message to audit trail");
    }

    info!(
        brief_id,
        jid = %jid,
        delay_ms,
        "WhatsApp message sent with human-like timing"
    );

    Ok(format!(
        "Message sent to WhatsApp contact (brief: {brief_id}, delay: {delay_ms}ms)"
    ))
}

/// Resolve the WhatsApp JID for a brief's linked contact.
async fn resolve_jid_for_brief(
    brief: &crate::messaging::brief::TaskBrief,
    db: &SqlitePool,
) -> Result<String, String> {
    let contact_id = brief
        .contact_id
        .ok_or_else(|| "brief has no linked contact".to_owned())?;

    let contact = crate::messaging::contacts::load_contact(db, contact_id)
        .await
        .map_err(|e| format!("contact lookup failed: {e}"))?;

    contact
        .whatsapp_jid
        .ok_or_else(|| format!("contact '{}' has no WhatsApp JID", contact.name))
}
