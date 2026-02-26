//! Telegram adapter: input guard, UI formatting, slash commands, and bot dispatcher.
//!
//! Provides inbound credential scanning, outbound HTML message formatting,
//! slash command handling, and the main teloxide-based bot event loop.

use std::sync::Arc;

use teloxide::dispatching::UpdateFilterExt;
use teloxide::prelude::*;
use teloxide::types::{InputFile, ParseMode};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::agent::approval::{ApprovalManager, ApprovalResult};
use crate::agent::{SessionRouter, TelegramOutbound};
use crate::config::{Config, RuntimePaths};
use crate::executor::Executor;
use crate::memory::MemoryEngine;
use crate::tools::registry::DynamicToolRegistry;

pub mod commands;
pub mod input_guard;
pub mod media;
pub mod ui;

// ---------------------------------------------------------------------------
// Shared state for handler injection
// ---------------------------------------------------------------------------

/// Shared dependencies injected into teloxide handlers via `dptree::deps!`.
#[derive(Clone)]
struct SharedState {
    config: Arc<Config>,
    session_router: Arc<SessionRouter>,
    approval_manager: Arc<ApprovalManager>,
    known_secrets: Vec<String>,
    executor: Arc<dyn Executor>,
    memory: Arc<MemoryEngine>,
    registry: Arc<DynamicToolRegistry>,
    paths: RuntimePaths,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the Telegram bot adapter.
///
/// Starts three concurrent tasks:
/// 1. **Inbound handler** -- receives messages, checks allowed_users, scans for
///    credentials, routes to sessions
/// 2. **Callback handler** -- processes inline keyboard callbacks for approvals
/// 3. **Outbound sender** -- sends agent responses back to Telegram
///
/// Blocks until the bot is stopped (Ctrl+C).
#[allow(clippy::too_many_arguments)]
pub async fn run_telegram(
    bot_token: &str,
    config: Arc<Config>,
    session_router: Arc<SessionRouter>,
    approval_manager: Arc<ApprovalManager>,
    mut outbound_rx: mpsc::Receiver<TelegramOutbound>,
    known_secrets: Vec<String>,
    executor: Arc<dyn Executor>,
    memory: Arc<MemoryEngine>,
    registry: Arc<DynamicToolRegistry>,
    paths: RuntimePaths,
) -> anyhow::Result<()> {
    let bot = Bot::new(bot_token);

    // Spawn outbound sender task
    let outbound_bot = bot.clone();
    let _outbound_handle = tokio::spawn(async move {
        while let Some(msg) = outbound_rx.recv().await {
            let chat_id = ChatId(msg.user_id);

            if let Some(ref text) = msg.text {
                let mut req = outbound_bot
                    .send_message(chat_id, text)
                    .parse_mode(ParseMode::Html);

                if let Some((ref approval_id, _)) = msg.approval_keyboard {
                    req = req.reply_markup(ui::approval_keyboard(approval_id));
                }

                if let Err(e) = req.await {
                    warn!(error = %e, "failed to send telegram message");
                }
            }

            if let Some(ref file_path) = msg.file_path {
                let input_file = InputFile::file(file_path);
                if let Err(e) = outbound_bot.send_document(chat_id, input_file).await {
                    warn!(error = %e, "failed to send telegram file");
                }
            }
        }
    });

    let shared = SharedState {
        config,
        session_router,
        approval_manager,
        known_secrets,
        executor,
        memory,
        registry,
        paths,
    };

    // Build dptree handler schema
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback));

    info!("telegram dispatcher starting");

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![shared])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Message handler
// ---------------------------------------------------------------------------

/// Handle an incoming Telegram message.
///
/// Checks allowed_users, dispatches slash commands, and routes
/// regular text to the session router after credential scanning.
async fn handle_message(bot: Bot, msg: Message, state: SharedState) -> ResponseResult<()> {
    let user_id = match msg.from {
        Some(ref user) => {
            let uid_u64 = user.id.0;
            // teloxide uses u64 for user IDs; our config stores i64.
            i64::try_from(uid_u64).unwrap_or(0)
        }
        None => return Ok(()),
    };

    debug!(user_id, "telegram message received");

    // Check if user is in allowed_users
    if !state
        .config
        .channels
        .telegram
        .allowed_users
        .contains(&user_id)
    {
        warn!(
            user_id,
            allowed = ?state.config.channels.telegram.allowed_users,
            "message dropped: user not in allowed_users"
        );
        return Ok(());
    }

    let text = if let Some(t) = msg.text() {
        t.to_owned()
    } else {
        // Handle non-text messages: voice, photo, document.
        let inbox_dir = state.paths.workspace_dir.join("inbox");
        let media_result = if let Some(voice) = msg.voice() {
            media::handle_voice(&bot, voice, &inbox_dir).await
        } else if let Some(photos) = msg.photo() {
            media::handle_photo(&bot, photos, &inbox_dir).await
        } else if let Some(document) = msg.document() {
            media::handle_document(&bot, document, &inbox_dir).await
        } else {
            debug!(user_id, "unsupported message type, ignoring");
            return Ok(());
        };

        match media_result {
            Ok(desc) => desc.text,
            Err(e) => {
                warn!(error = %e, "failed to handle media message");
                bot.send_message(
                    msg.chat.id,
                    "Failed to download the file. Please try again.",
                )
                .await?;
                return Ok(());
            }
        }
    };

    // Handle slash commands
    if text.starts_with('/') {
        let reply = dispatch_command(&text, &state, user_id).await;
        bot.send_message(msg.chat.id, reply)
            .parse_mode(ParseMode::Html)
            .await?;
        return Ok(());
    }

    // Scan message for credentials
    match input_guard::scan_message(&text, &state.known_secrets) {
        input_guard::GuardAction::Blocked => {
            bot.send_message(
                msg.chat.id,
                "That looks like a credential. Add it to your .env file instead.",
            )
            .await?;
        }
        input_guard::GuardAction::Redacted(redacted) => {
            if let Err(e) = state.session_router.route_message(user_id, redacted).await {
                warn!(error = %e, "failed to route redacted message to session");
            }
        }
        input_guard::GuardAction::Pass(clean) => {
            if let Err(e) = state.session_router.route_message(user_id, clean).await {
                warn!(error = %e, "failed to route message to session");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Command dispatcher
// ---------------------------------------------------------------------------

/// Parse and dispatch a slash command, returning the HTML response.
async fn dispatch_command(text: &str, state: &SharedState, user_id: i64) -> String {
    // Strip the leading "/" and split into command and args
    let without_slash = &text[1..];
    // Handle bot-mention suffixes like "/help@wintermute_bot"
    let (full_command, args) = match without_slash.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (without_slash, ""),
    };
    // Strip @bot_name suffix if present
    let command = full_command.split('@').next().unwrap_or(full_command);

    match command {
        "help" | "start" => commands::handle_help(),
        "reset" | "new" => {
            let had_session = state.session_router.remove_session(user_id).await;
            commands::handle_reset(had_session)
        }
        "status" => {
            let session_count = state.session_router.session_count().await;
            commands::handle_status(&*state.executor, &state.memory, session_count).await
        }
        "budget" => {
            // We don't have per-session budget info from this context,
            // so show daily-level summary with zeros for session values.
            commands::handle_budget(0, 0, 0, 0)
        }
        "memory" => commands::handle_memory(&state.memory).await,
        "memory_pending" => commands::handle_memory_pending(&state.memory).await,
        "memory_undo" => commands::handle_memory_undo(&state.memory).await,
        "tools" => {
            if args.is_empty() {
                commands::handle_tools(&state.registry)
            } else {
                commands::handle_tools_detail(&state.registry, args)
            }
        }
        "sandbox" => commands::handle_sandbox(&*state.executor).await,
        "backup" => {
            commands::handle_backup_trigger(
                &state.paths.scripts_dir,
                &state.memory,
                &state.paths.backups_dir,
            )
            .await
        }
        _ => format!("Unknown command: /{}", ui::escape_html(command)),
    }
}

// ---------------------------------------------------------------------------
// Callback query handler
// ---------------------------------------------------------------------------

/// Handle inline keyboard callback queries for approval responses.
async fn handle_callback(bot: Bot, query: CallbackQuery, state: SharedState) -> ResponseResult<()> {
    let user_id = {
        let uid_u64 = query.from.id.0;
        i64::try_from(uid_u64).unwrap_or(0)
    };

    let data = match query.data {
        Some(ref d) => d.as_str(),
        None => {
            bot.answer_callback_query(&query.id).await?;
            return Ok(());
        }
    };

    // Parse callback data: "a:{id}" for approve, "d:{id}" for deny
    let (approved, approval_id) = if let Some(id) = data.strip_prefix("a:") {
        (true, id)
    } else if let Some(id) = data.strip_prefix("d:") {
        (false, id)
    } else {
        bot.answer_callback_query(&query.id)
            .text("Unknown action")
            .await?;
        return Ok(());
    };

    let result = state
        .approval_manager
        .resolve(approval_id, approved, user_id);

    let answer_text = match &result {
        ApprovalResult::Approved { tool_name, .. } => format!("Approved: {tool_name}"),
        ApprovalResult::Denied { tool_name, .. } => format!("Denied: {tool_name}"),
        ApprovalResult::Expired => "Approval expired.".to_owned(),
        ApprovalResult::NotFound => "Approval not found.".to_owned(),
        ApprovalResult::WrongUser => "You are not authorized for this approval.".to_owned(),
    };

    // Route the result to the session
    if let Err(e) = state.session_router.route_approval(result).await {
        warn!(error = %e, "failed to route approval result");
    }

    bot.answer_callback_query(&query.id)
        .text(answer_text)
        .await?;

    Ok(())
}
