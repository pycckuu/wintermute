//! Dynamic tool registry with file-system-based hot-reload.
//!
//! Tools are loaded from JSON schema files in a scripts directory.
//! A [`notify`] file watcher detects changes and reloads affected tools
//! automatically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::providers::ToolDefinition;

/// Upper bound for dynamic tool timeout loaded from schema files.
const MAX_DYNAMIC_TIMEOUT_SECS: u64 = 3600;

// ---------------------------------------------------------------------------
// DynamicToolSchema
// ---------------------------------------------------------------------------

/// Health metadata for a dynamic tool, updated after each invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMeta {
    /// ISO 8601 timestamp when the tool was created.
    pub created_at: String,
    /// ISO 8601 timestamp of last invocation, if any.
    pub last_used: Option<String>,
    /// Total number of invocations.
    pub invocations: u64,
    /// Running success rate (0.0–1.0).
    pub success_rate: f64,
    /// Running average execution duration in milliseconds.
    pub avg_duration_ms: u64,
    /// Last error message, if any.
    pub last_error: Option<String>,
    /// Schema version (incremented on tool updates).
    pub version: u32,
}

impl ToolMeta {
    /// Create initial metadata for a newly created tool.
    pub fn new_initial() -> Self {
        Self {
            created_at: chrono::Utc::now().to_rfc3339(),
            last_used: None,
            invocations: 0,
            success_rate: 1.0,
            avg_duration_ms: 0,
            last_error: None,
            version: 1,
        }
    }
}

/// Schema for a dynamically registered tool, loaded from a JSON file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicToolSchema {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub parameters: serde_json::Value,
    /// Maximum execution timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Health metadata, updated after each invocation.
    #[serde(default, rename = "_meta")]
    pub meta: Option<ToolMeta>,
}

/// Default timeout for dynamic tools: 120 seconds.
fn default_timeout() -> u64 {
    120
}

// ---------------------------------------------------------------------------
// DynamicToolRegistry
// ---------------------------------------------------------------------------

/// Registry of dynamically created tools, backed by JSON files on disk.
///
/// Supports hot-reload via a file system watcher: when a `.json` file is
/// created, modified, or deleted in the scripts directory, the registry
/// updates automatically.
///
/// Tracks last-used timestamps for ranked tool selection (relevance/recency).
pub struct DynamicToolRegistry {
    /// Map from tool name to its schema.
    tools: RwLock<HashMap<String, DynamicToolSchema>>,
    /// Last-used timestamp per tool name (for ranked selection).
    last_used: RwLock<HashMap<String, Instant>>,
    /// Directory containing tool scripts and schema files.
    scripts_dir: PathBuf,
    /// File watcher handle (kept alive to maintain notifications).
    _watcher: Option<RecommendedWatcher>,
}

impl std::fmt::Debug for DynamicToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = match self.tools.read() {
            Ok(tools) => tools.len(),
            Err(_) => 0,
        };
        f.debug_struct("DynamicToolRegistry")
            .field("scripts_dir", &self.scripts_dir)
            .field("tool_count", &count)
            .finish()
    }
}

impl DynamicToolRegistry {
    /// Create a new registry, loading existing tools and starting the file watcher.
    ///
    /// # Errors
    ///
    /// Returns an error if the scripts directory cannot be read or the watcher
    /// cannot be initialized.
    pub fn new(scripts_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        let tools = RwLock::new(HashMap::new());
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(evt) = event {
                    for path in evt.paths {
                        if let Err(e) = tx.send(path) {
                            warn!(error = %e, "failed to send watcher event");
                        }
                    }
                }
            })?;

        // Only watch if the directory exists.
        if scripts_dir.is_dir() {
            watcher.watch(&scripts_dir, RecursiveMode::NonRecursive)?;
        }

        let registry = Arc::new(Self {
            tools,
            last_used: RwLock::new(HashMap::new()),
            scripts_dir: scripts_dir.clone(),
            _watcher: Some(watcher),
        });

        // Load existing tools.
        registry.reload_all_inner()?;

        // Spawn background thread to process watcher events.
        let registry_for_thread = Arc::clone(&registry);
        std::thread::spawn(move || {
            while let Ok(path) = rx.recv() {
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    let file_stem = match path.file_stem().and_then(|s| s.to_str()) {
                        Some(stem) => stem,
                        None => {
                            warn!(
                                path = %path.display(),
                                "skipping dynamic tool with non-utf8 filename"
                            );
                            continue;
                        }
                    };

                    if path.exists() {
                        debug!(tool = file_stem, "reloading dynamic tool from watcher");
                        if let Err(e) = registry_for_thread.reload_tool(file_stem) {
                            warn!(tool = file_stem, error = %e, "failed to reload tool");
                        }
                    } else {
                        // File was deleted — remove from registry.
                        debug!(tool = file_stem, "removing deleted dynamic tool");
                        if let Ok(mut map) = registry_for_thread.tools.write() {
                            map.remove(file_stem);
                        }
                    }
                }
            }
        });

        let count = registry.count();
        info!(count, dir = %scripts_dir.display(), "dynamic tool registry initialised");

        Ok(registry)
    }

    /// Create a registry without a file watcher (useful for testing).
    ///
    /// # Errors
    ///
    /// Returns an error if existing tool files cannot be loaded.
    pub fn new_without_watcher(scripts_dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        let tools = RwLock::new(HashMap::new());

        let registry = Arc::new(Self {
            tools,
            last_used: RwLock::new(HashMap::new()),
            scripts_dir,
            _watcher: None,
        });

        registry.reload_all_inner()?;

        Ok(registry)
    }

    /// Get the schema for a tool by name.
    pub fn get(&self, name: &str) -> Option<DynamicToolSchema> {
        match self.tools.read() {
            Ok(map) => map.get(name).cloned(),
            Err(e) => {
                warn!(error = %e, "dynamic tool registry lock poisoned in get");
                None
            }
        }
    }

    /// Record that a dynamic tool was used (for recency-based ranking).
    pub fn record_usage(&self, name: &str) {
        if let Ok(mut last) = self.last_used.write() {
            last.insert(name.to_owned(), Instant::now());
        }
    }

    /// Return all registered tool schemas as [`ToolDefinition`]s.
    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        let map = match self.tools.read() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "dynamic tool registry lock poisoned in all_definitions");
                return Vec::new();
            }
        };

        map.values().map(schema_to_definition).collect()
    }

    /// Return dynamic tool definitions ranked by relevance and recency.
    ///
    /// When `query` is provided, tools are scored by: (1) keyword overlap with
    /// description, (2) last-used timestamp (most recent first). When `query`
    /// is empty or None, tools are ordered by recency only.
    pub fn ranked_definitions(&self, max_count: usize, query: Option<&str>) -> Vec<ToolDefinition> {
        let map = match self.tools.read() {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "dynamic tool registry lock poisoned in ranked_definitions");
                return Vec::new();
            }
        };
        let last = match self.last_used.read() {
            Ok(l) => l,
            Err(e) => {
                warn!(
                    error = %e,
                    "dynamic tool registry usage lock poisoned in ranked_definitions"
                );
                return Vec::new();
            }
        };

        let query_tokens: Vec<String> = tokenise_lowercase(query.unwrap_or(""));

        let mut scored: Vec<(f64, ToolDefinition)> = map
            .values()
            .map(|schema| {
                let def = schema_to_definition(schema);
                let score = score_tool_for_ranking(schema, &last, &query_tokens);
                (score, def)
            })
            .collect();

        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(max_count)
            .map(|(_, def)| def)
            .collect()
    }

    /// Reload a specific tool from its JSON file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    pub fn reload_tool(&self, name: &str) -> anyhow::Result<()> {
        let path = self.scripts_dir.join(format!("{name}.json"));

        if !path.exists() {
            // Remove if it was previously registered.
            if let Ok(mut map) = self.tools.write() {
                map.remove(name);
            }
            return Ok(());
        }

        let schema = load_tool_schema(&path)?;

        if let Ok(mut map) = self.tools.write() {
            map.insert(schema.name.clone(), schema);
        } else {
            warn!("dynamic tool registry lock poisoned in reload_tool");
        }

        debug!(tool = name, "dynamic tool reloaded");
        Ok(())
    }

    /// Reload all JSON tool files from the scripts directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the scripts directory cannot be read.
    pub fn reload_all(&self) -> anyhow::Result<()> {
        self.reload_all_inner()
    }

    /// Internal reload implementation.
    fn reload_all_inner(&self) -> anyhow::Result<()> {
        if !self.scripts_dir.is_dir() {
            return Ok(());
        }

        let entries = std::fs::read_dir(&self.scripts_dir)?;
        let mut loaded = HashMap::new();

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match load_tool_schema(&path) {
                Ok(schema) => {
                    loaded.insert(schema.name.clone(), schema);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skipping invalid tool schema");
                }
            }
        }

        if let Ok(mut map) = self.tools.write() {
            *map = loaded;
        } else {
            warn!("dynamic tool registry lock poisoned in reload_all");
        }

        Ok(())
    }

    /// Record an execution result, updating `_meta` and persisting to disk.
    ///
    /// Updates invocation count, success rate (running average), average
    /// duration, last-used timestamp, and last error. Writes the updated
    /// schema back to disk on a blocking thread.
    pub fn record_execution(
        &self,
        name: &str,
        success: bool,
        duration_ms: u64,
        error: Option<&str>,
    ) {
        let updated_schema = {
            let mut map = match self.tools.write() {
                Ok(m) => m,
                Err(e) => {
                    warn!(error = %e, "registry lock poisoned in record_execution");
                    return;
                }
            };

            let schema = match map.get_mut(name) {
                Some(s) => s,
                None => return,
            };

            let meta = schema.meta.get_or_insert_with(ToolMeta::new_initial);
            meta.invocations = meta.invocations.saturating_add(1);
            meta.last_used = Some(chrono::Utc::now().to_rfc3339());

            // Running average for success_rate.
            #[allow(clippy::cast_precision_loss)]
            let n = meta.invocations as f64;
            let success_val = if success { 1.0 } else { 0.0 };
            meta.success_rate = meta.success_rate + (success_val - meta.success_rate) / n;

            // Running average for duration.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            {
                let avg = meta.avg_duration_ms as f64;
                meta.avg_duration_ms = (avg + (duration_ms as f64 - avg) / n) as u64;
            }

            if !success {
                meta.last_error = error.map(|e| e.chars().take(500).collect());
            }

            schema.clone()
        };

        // Persist to disk on a bounded blocking thread.
        let path = self.scripts_dir.join(format!("{name}.json"));
        tokio::task::spawn_blocking(move || {
            if let Ok(json) = serde_json::to_string_pretty(&updated_schema) {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to persist _meta to disk"
                    );
                }
            }
        });
    }

    /// Return all tool schemas including `_meta` (for tool review / SID).
    pub fn all_schemas(&self) -> Vec<DynamicToolSchema> {
        match self.tools.read() {
            Ok(map) => map.values().cloned().collect(),
            Err(e) => {
                warn!(error = %e, "registry lock poisoned in all_schemas");
                Vec::new()
            }
        }
    }

    /// Return the number of registered dynamic tools.
    pub fn count(&self) -> usize {
        match self.tools.read() {
            Ok(map) => map.len(),
            Err(e) => {
                warn!(error = %e, "dynamic tool registry lock poisoned in count");
                0
            }
        }
    }
}

/// Split text into lowercase alphanumeric tokens.
fn tokenise_lowercase(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Convert a schema to a tool definition.
fn schema_to_definition(schema: &DynamicToolSchema) -> ToolDefinition {
    ToolDefinition {
        name: schema.name.clone(),
        description: schema.description.clone(),
        input_schema: schema.parameters.clone(),
    }
}

/// Score a tool for ranking: relevance (query overlap) + recency.
fn score_tool_for_ranking(
    schema: &DynamicToolSchema,
    last_used: &HashMap<String, Instant>,
    query_tokens: &[String],
) -> f64 {
    let recency = last_used
        .get(&schema.name)
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(f64::MAX);
    let recency_score = 1.0 / (1.0 + recency / 3600.0);

    let relevance = if query_tokens.is_empty() {
        0.0
    } else {
        let desc_tokens = tokenise_lowercase(&schema.description);
        let matches = query_tokens
            .iter()
            .filter(|q| desc_tokens.iter().any(|d| d.contains(q.as_str())))
            .count();
        f64::from(u32::try_from(matches).unwrap_or(0))
            / f64::from(u32::try_from(query_tokens.len()).unwrap_or(1))
    };

    relevance * 2.0 + recency_score
}

/// Load and validate a tool schema from a JSON file.
fn load_tool_schema(path: &Path) -> anyhow::Result<DynamicToolSchema> {
    let content = std::fs::read_to_string(path)?;
    let schema: DynamicToolSchema = serde_json::from_str(&content)?;
    if schema.timeout_secs == 0 || schema.timeout_secs > MAX_DYNAMIC_TIMEOUT_SECS {
        anyhow::bail!(
            "invalid timeout_secs {} in {}; expected 1..={MAX_DYNAMIC_TIMEOUT_SECS}",
            schema.timeout_secs,
            path.display()
        );
    }
    Ok(schema)
}
