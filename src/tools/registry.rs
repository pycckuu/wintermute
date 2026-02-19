//! Dynamic tool registry with file-system-based hot-reload.
//!
//! Tools are loaded from JSON schema files in a scripts directory.
//! A [`notify`] file watcher detects changes and reloads affected tools
//! automatically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::providers::ToolDefinition;

// ---------------------------------------------------------------------------
// DynamicToolSchema
// ---------------------------------------------------------------------------

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
pub struct DynamicToolRegistry {
    /// Map from tool name to its schema.
    tools: RwLock<HashMap<String, DynamicToolSchema>>,
    /// Directory containing tool scripts and schema files.
    scripts_dir: PathBuf,
    /// File watcher handle (kept alive to maintain notifications).
    _watcher: Option<RecommendedWatcher>,
}

impl std::fmt::Debug for DynamicToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.tools.read().map(|t| t.len()).unwrap_or(0);
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
                    let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

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
            scripts_dir,
            _watcher: None,
        });

        registry.reload_all_inner()?;

        Ok(registry)
    }

    /// Get the schema for a tool by name.
    pub fn get(&self, name: &str) -> Option<DynamicToolSchema> {
        self.tools
            .read()
            .ok()
            .and_then(|map| map.get(name).cloned())
    }

    /// Return all registered tool schemas as [`ToolDefinition`]s.
    pub fn all_definitions(&self) -> Vec<ToolDefinition> {
        let map = match self.tools.read() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };

        map.values()
            .map(|schema| ToolDefinition {
                name: schema.name.clone(),
                description: schema.description.clone(),
                input_schema: schema.parameters.clone(),
            })
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

        let content = std::fs::read_to_string(&path)?;
        let schema: DynamicToolSchema = serde_json::from_str(&content)?;

        if let Ok(mut map) = self.tools.write() {
            map.insert(schema.name.clone(), schema);
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
        }

        Ok(())
    }

    /// Return the number of registered dynamic tools.
    pub fn count(&self) -> usize {
        self.tools.read().map(|m| m.len()).unwrap_or(0)
    }
}

/// Load and validate a tool schema from a JSON file.
fn load_tool_schema(path: &Path) -> anyhow::Result<DynamicToolSchema> {
    let content = std::fs::read_to_string(path)?;
    let schema: DynamicToolSchema = serde_json::from_str(&content)?;
    Ok(schema)
}
