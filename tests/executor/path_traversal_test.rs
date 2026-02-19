//! Path traversal protection tests for resolve_working_dir.

use std::path::{Path, PathBuf};

use wintermute::executor::direct::resolve_working_dir;

#[test]
fn relative_subdir_resolves_within_base() {
    let base = Path::new("/workspace");
    let requested = Path::new("project/src");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_ok());
    let resolved = result.expect("should resolve");
    assert!(resolved.starts_with("/workspace"));
    assert!(resolved.ends_with("project/src"));
}

#[test]
fn absolute_path_within_base_succeeds() {
    let base = Path::new("/workspace");
    let requested = Path::new("/workspace/project");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_ok());
}

#[test]
fn dotdot_traversal_blocked() {
    let base = Path::new("/workspace");
    let requested = Path::new("../../etc/passwd");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_err());
}

#[test]
fn absolute_path_outside_base_blocked() {
    let base = Path::new("/workspace");
    let requested = Path::new("/etc/passwd");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_err());
}

#[test]
fn current_dir_dot_resolves() {
    let base = Path::new("/workspace");
    let requested = Path::new("./subdir");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_ok());
    let resolved = result.expect("should resolve");
    assert_eq!(resolved, PathBuf::from("/workspace/subdir"));
}

#[test]
fn dotdot_within_base_succeeds() {
    let base = Path::new("/workspace");
    let requested = Path::new("project/../other");
    let result = resolve_working_dir(base, requested);
    assert!(result.is_ok());
    let resolved = result.expect("should resolve");
    assert_eq!(resolved, PathBuf::from("/workspace/other"));
}
