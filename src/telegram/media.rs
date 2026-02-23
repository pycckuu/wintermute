//! Non-text message handling: download voice, photo, and document files.
//!
//! Downloads media files from Telegram to the workspace inbox directory
//! and produces a description string that is routed to the agent as a
//! regular text message. The agent can then build tools (via `create_tool`)
//! to process these files.

use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::Utc;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{Document, PhotoSize, Voice};
use tracing::debug;

/// Result of processing a non-text Telegram message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// Description string routed to the agent (e.g. `[Voice message: /path, 12s]`).
    pub text: String,
    /// Local file path where the media was saved.
    pub file_path: PathBuf,
}

/// Download a voice message and produce a description.
///
/// Saves to `{inbox_dir}/voice_{timestamp}.ogg`.
///
/// # Errors
///
/// Returns an error if the file cannot be downloaded or written.
pub async fn handle_voice(
    bot: &Bot,
    voice: &Voice,
    inbox_dir: &Path,
) -> anyhow::Result<MediaDescription> {
    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("voice_{timestamp}.ogg");
    let file_path = inbox_dir.join(&filename);

    download_telegram_file(bot, &voice.file.id, &file_path).await?;

    let text = format!(
        "[Voice message: {}, {}s]",
        file_path.display(),
        voice.duration
    );

    Ok(MediaDescription { text, file_path })
}

/// Download a photo and produce a description.
///
/// Picks the largest available size (last in the array by Telegram convention).
/// Saves to `{inbox_dir}/photo_{timestamp}.jpg`.
///
/// # Errors
///
/// Returns an error if no sizes are available or the file cannot be downloaded.
pub async fn handle_photo(
    bot: &Bot,
    photos: &[PhotoSize],
    inbox_dir: &Path,
) -> anyhow::Result<MediaDescription> {
    let photo = photos
        .last()
        .ok_or_else(|| anyhow::anyhow!("photo array is empty"))?;

    let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
    let filename = format!("photo_{timestamp}.jpg");
    let file_path = inbox_dir.join(&filename);

    download_telegram_file(bot, &photo.file.id, &file_path).await?;

    let text = format!("[Photo: {}]", file_path.display());

    Ok(MediaDescription { text, file_path })
}

/// Download a document and produce a description.
///
/// Preserves the original filename when available (sanitized against path
/// traversal). Falls back to `doc_{timestamp}` if no name is provided.
///
/// # Errors
///
/// Returns an error if the file cannot be downloaded or written.
pub async fn handle_document(
    bot: &Bot,
    document: &Document,
    inbox_dir: &Path,
) -> anyhow::Result<MediaDescription> {
    let fallback;
    let raw_name = match document.file_name.as_deref() {
        Some(name) => name,
        None => {
            fallback = format!("doc_{}", Utc::now().format("%Y%m%d_%H%M%S"));
            &fallback
        }
    };
    let filename = sanitize_filename(raw_name);
    let file_path = inbox_dir.join(&filename);

    download_telegram_file(bot, &document.file.id, &file_path).await?;

    let text = format!("[Document: {}]", file_path.display());

    Ok(MediaDescription { text, file_path })
}

/// Sanitize a filename to prevent path traversal attacks.
///
/// Replaces path separators (`/`, `\`) with underscores and strips leading
/// dots so the file stays inside the target directory. Returns a
/// timestamp-based fallback name if the result would be empty.
pub fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .replace(['/', '\\'], "_")
        .trim_start_matches('.')
        .to_owned();

    if sanitized.is_empty() {
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        format!("doc_{timestamp}")
    } else {
        sanitized
    }
}

/// Download a file from Telegram by file ID and write it to a local path.
///
/// Creates the parent directory if it does not exist.
async fn download_telegram_file(bot: &Bot, file_id: &str, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create inbox directory: {}", parent.display()))?;
    }

    let file = bot
        .get_file(file_id)
        .await
        .context("failed to get file info from Telegram")?;

    let mut dst = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("failed to create file at {}", dest.display()))?;

    bot.download_file(&file.path, &mut dst)
        .await
        .context("failed to download file from Telegram")?;

    debug!(path = %dest.display(), "media file downloaded");

    Ok(())
}
