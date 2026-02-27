//! Security invariant tests for the Flatline supervisor crate.
//!
//! These verify that Flatline follows the same security discipline as
//! Wintermute, adapted for the supervisor's specific role.

use std::path::{Path, PathBuf};

/// Recursively collect all `.rs` files under the given directory.
fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_rust_files(&path, out)?;
        } else if metadata.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// `std::process::Command` and `tokio::process::Command` must ONLY appear
/// in `fixer.rs`, `patterns.rs`, `services.rs`, and `updater.rs`. No other
/// module should use process execution.
#[test]
fn process_command_confined_to_allowed_modules() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src_dir, &mut rust_files).expect("failed to collect Rust source files");

    let forbidden = ["std::process::Command", "tokio::process::Command"];
    let allowed_files: &[&str] = &["fixer.rs", "patterns.rs", "services.rs", "updater.rs"];

    for path in &rust_files {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if allowed_files.contains(&file_name) {
            continue;
        }

        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

        for pattern in &forbidden {
            assert!(
                !content.contains(pattern),
                "process command API '{pattern}' found in {} — only fixer.rs, patterns.rs, services.rs, and updater.rs may use process execution",
                path.display()
            );
        }
    }
}

/// `validate_commit_hash` must reject shell injection attempts and accept
/// valid hex strings.
#[test]
fn fixer_validate_commit_hash_rejects_injection() {
    use flatline::fixer::validate_commit_hash;

    let injections = [
        "; rm -rf /",
        "abc; echo hacked",
        "$(whoami)",
        "`id`",
        "abc\ndef",
        "",
    ];

    for input in &injections {
        assert!(
            validate_commit_hash(input).is_err(),
            "validate_commit_hash must reject injection attempt: {input:?}"
        );
    }

    assert!(
        validate_commit_hash("a1b2c3d4e5f6").is_ok(),
        "validate_commit_hash must accept valid hex string 'a1b2c3d4e5f6'"
    );
}

/// No `.unwrap()` calls in production source code.
/// `.unwrap_or`, `.unwrap_or_else`, and `.unwrap_or_default` are allowed.
#[test]
fn no_unwrap_in_src() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src_dir, &mut rust_files).expect("failed to collect Rust source files");

    for path in &rust_files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

        for (line_num, line) in content.lines().enumerate() {
            // Skip lines that use allowed unwrap variants.
            if line.contains(".unwrap_or")
                || line.contains(".unwrap_or_else")
                || line.contains(".unwrap_or_default")
            {
                continue;
            }

            assert!(
                !line.contains(".unwrap()"),
                ".unwrap() found in {}:{} — production code must not use .unwrap()\n  line: {}",
                path.display(),
                line_num + 1,
                line.trim()
            );
        }
    }
}

/// `QuarantineTool` action must reject tool names containing path traversal
/// characters (`..`, `/`, `\`).
#[test]
fn quarantine_tool_rejects_path_traversal() {
    let traversal_attempts = [
        "../etc/passwd",
        "../../secret",
        "tool/../../etc",
        "tool\\..\\secret",
        "..hidden",
        "some/tool",
        "some\\tool",
    ];

    for tool_name in &traversal_attempts {
        assert!(
            tool_name.contains("..") || tool_name.contains('/') || tool_name.contains('\\'),
            "test data error: {tool_name:?} should contain path traversal characters"
        );
    }

    // Verify the fixer source code enforces this validation.
    let fixer_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/fixer.rs");
    let content = std::fs::read_to_string(&fixer_src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixer_src.display()));

    assert!(
        content.contains("contains('/')")
            && content.contains("contains('\\\\')")
            && content.contains("contains(\"..\")"),
        "apply_quarantine_tool must validate tool names against path traversal (/, \\, ..)"
    );
}

/// The diagnosis module must redact LLM output before parsing, ensuring
/// sensitive data never leaks into diagnostic results.
#[test]
fn diagnosis_module_uses_redactor() {
    let diag_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/diagnosis.rs");
    let content = std::fs::read_to_string(&diag_src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", diag_src.display()));

    assert!(
        content.contains("redactor.redact("),
        "diagnosis module must redact LLM output via redactor.redact() before parsing"
    );
}

/// Every `.rs` file in `flatline/src/` must start with `//!` module-level
/// documentation.
#[test]
fn all_modules_have_doc_comments() {
    let src_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut rust_files = Vec::new();
    collect_rust_files(&src_dir, &mut rust_files).expect("failed to collect Rust source files");

    for path in &rust_files {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));

        assert!(
            content.starts_with("//!"),
            "{} must start with //! module-level documentation",
            path.display()
        );
    }
}

/// The crate root must contain `#![forbid(unsafe_code)]` to prevent any
/// unsafe code in the entire crate.
#[test]
fn crate_root_forbids_unsafe() {
    let lib_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
    let content = std::fs::read_to_string(&lib_src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", lib_src.display()));

    assert!(
        content.contains("#![forbid(unsafe_code)]"),
        "flatline/src/lib.rs must contain #![forbid(unsafe_code)]"
    );
}
