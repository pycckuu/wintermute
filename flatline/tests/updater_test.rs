//! Tests for the auto-update subsystem.

use std::path::PathBuf;

use flatline::updater;

fn make_wm_paths() -> wintermute::config::RuntimePaths {
    let root = PathBuf::from("/tmp/wintermute");
    wintermute::config::RuntimePaths {
        root: root.clone(),
        config_toml: root.join("config.toml"),
        agent_toml: root.join("agent.toml"),
        env_file: root.join(".env"),
        scripts_dir: root.join("scripts"),
        workspace_dir: root.join("workspace"),
        data_dir: root.join("data"),
        backups_dir: root.join("backups"),
        memory_db: root.join("data/memory.db"),
        pid_file: root.join("wintermute.pid"),
        health_json: root.join("health.json"),
        identity_md: root.join("IDENTITY.md"),
        user_md: root.join("USER.md"),
        flatline_root: root.join("flatline"),
        agents_md: root.join("AGENTS.md"),
        docs_dir: root.join("docs"),
    }
}

fn make_fl_paths() -> flatline::config::FlatlinePaths {
    flatline::config::FlatlinePaths {
        root: PathBuf::from("/tmp/flatline"),
        state_db: PathBuf::from("/tmp/flatline/state.db"),
        diagnoses_dir: PathBuf::from("/tmp/flatline/diagnoses"),
        patches_dir: PathBuf::from("/tmp/flatline/patches"),
        updates_dir: PathBuf::from("/tmp/flatline/updates"),
        pending_dir: PathBuf::from("/tmp/flatline/updates/pending"),
    }
}

fn make_health(active_sessions: usize) -> wintermute::heartbeat::health::HealthReport {
    wintermute::heartbeat::health::HealthReport {
        status: "running".to_owned(),
        uptime_secs: 1000,
        last_heartbeat: chrono::Utc::now().to_rfc3339(),
        executor: "Docker".to_owned(),
        container_healthy: true,
        active_sessions,
        memory_db_size_mb: 1.0,
        scripts_count: 5,
        dynamic_tools_count: 5,
        budget_today: wintermute::heartbeat::health::BudgetReport {
            used: 1000,
            limit: 5_000_000,
        },
        last_error: None,
    }
}

// -- parse_version_tag tests --

#[test]
fn parse_version_tag_strips_v_prefix() {
    let v = updater::parse_version_tag("v0.4.0").expect("parse");
    assert_eq!(v.major, 0);
    assert_eq!(v.minor, 4);
    assert_eq!(v.patch, 0);
}

#[test]
fn parse_version_tag_handles_bare_semver() {
    let v = updater::parse_version_tag("1.2.3").expect("parse");
    assert_eq!(v.major, 1);
    assert_eq!(v.minor, 2);
    assert_eq!(v.patch, 3);
}

#[test]
fn parse_version_tag_rejects_invalid() {
    assert!(updater::parse_version_tag("abc").is_err());
    assert!(updater::parse_version_tag("").is_err());
    assert!(updater::parse_version_tag("v").is_err());
}

// -- find_checksum tests --

#[test]
fn find_checksum_extracts_matching_digest() {
    let content = "\
abc123def456789  wintermute-0.4.0-x86_64-unknown-linux-gnu.tar.gz
fedcba987654321  flatline-0.4.0-x86_64-unknown-linux-gnu.tar.gz
111222333444555  checksums-sha256.txt
";

    let digest =
        updater::find_checksum(content, "wintermute-0.4.0-x86_64-unknown-linux-gnu.tar.gz")
            .expect("find checksum");

    assert_eq!(digest, "abc123def456789");
}

#[test]
fn find_checksum_finds_second_entry() {
    let content = "\
abc123  file-a.tar.gz
def456  file-b.tar.gz
";

    let digest = updater::find_checksum(content, "file-b.tar.gz").expect("find");
    assert_eq!(digest, "def456");
}

#[test]
fn find_checksum_returns_error_on_missing_file() {
    let content = "abc123  some-other-file.tar.gz\n";

    let result = updater::find_checksum(content, "nonexistent.tar.gz");
    assert!(result.is_err());
    assert!(result
        .expect_err("should fail")
        .to_string()
        .contains("nonexistent.tar.gz"));
}

#[test]
fn find_checksum_handles_empty_content() {
    let result = updater::find_checksum("", "file.tar.gz");
    assert!(result.is_err());
}

// -- sha256_bytes tests --

#[test]
fn sha256_bytes_computes_correct_digest() {
    // SHA256 of empty string is well-known.
    let digest = updater::sha256_bytes(b"");
    assert_eq!(
        digest,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha256_bytes_known_value() {
    // SHA256 of "hello" is well-known.
    let digest = updater::sha256_bytes(b"hello");
    assert_eq!(
        digest,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

// -- sha256_file tests --

#[tokio::test]
async fn sha256_file_computes_correct_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("test.bin");
    tokio::fs::write(&file_path, b"hello world")
        .await
        .expect("write");

    let digest = updater::sha256_file(&file_path).await.expect("hash");
    assert_eq!(
        digest,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}

#[tokio::test]
async fn sha256_file_errors_on_missing() {
    let result = updater::sha256_file(std::path::Path::new("/nonexistent/file")).await;
    assert!(result.is_err());
}

// -- validate_asset_name tests --

#[test]
fn validate_asset_name_accepts_clean_names() {
    assert!(
        updater::validate_asset_name("wintermute-0.4.0-x86_64-unknown-linux-gnu.tar.gz").is_ok()
    );
    assert!(updater::validate_asset_name("flatline-0.4.0-aarch64-apple-darwin.tar.gz").is_ok());
    assert!(updater::validate_asset_name("checksums-sha256.txt").is_ok());
    assert!(updater::validate_asset_name("migrate-0.4-to-0.5.sh").is_ok());
}

#[test]
fn validate_asset_name_rejects_path_traversal() {
    assert!(updater::validate_asset_name("../etc/passwd").is_err());
    assert!(updater::validate_asset_name("foo/../bar").is_err());
    assert!(updater::validate_asset_name("..").is_err());
}

#[test]
fn validate_asset_name_rejects_path_separators() {
    assert!(updater::validate_asset_name("/etc/passwd").is_err());
    assert!(updater::validate_asset_name("foo/bar").is_err());
    assert!(updater::validate_asset_name("foo\\bar").is_err());
}

#[test]
fn validate_asset_name_rejects_control_chars() {
    assert!(updater::validate_asset_name("file\x00name").is_err());
    assert!(updater::validate_asset_name("file\nname").is_err());
}

#[test]
fn validate_asset_name_rejects_too_long() {
    let long_name = "a".repeat(257);
    assert!(updater::validate_asset_name(&long_name).is_err());
}

// -- UpdateStatus tests --

#[test]
fn update_status_as_str() {
    assert_eq!(updater::UpdateStatus::Pending.as_str(), "pending");
    assert_eq!(updater::UpdateStatus::Downloading.as_str(), "downloading");
    assert_eq!(updater::UpdateStatus::Applying.as_str(), "applying");
    assert_eq!(updater::UpdateStatus::Healthy.as_str(), "healthy");
    assert_eq!(updater::UpdateStatus::RolledBack.as_str(), "rolled_back");
    assert_eq!(updater::UpdateStatus::Failed.as_str(), "failed");
    assert_eq!(updater::UpdateStatus::Skipped.as_str(), "skipped");
    assert_eq!(updater::UpdateStatus::Pinned.as_str(), "pinned");
}

#[test]
fn update_status_serde_roundtrip() {
    let status = updater::UpdateStatus::RolledBack;
    let json = serde_json::to_string(&status).expect("serialize");
    assert_eq!(json, "\"rolled_back\"");

    let parsed: updater::UpdateStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(parsed, status);
}

// -- is_check_time tests --

#[test]
fn is_check_time_rejects_invalid_format() {
    assert!(!updater::is_check_time("invalid", 300));
    assert!(!updater::is_check_time("", 300));
    assert!(!updater::is_check_time("25:00", 300));
}

// -- is_idle tests --

#[test]
fn is_idle_true_when_no_sessions() {
    let health = make_health(0);
    let config = flatline::config::UpdateConfig::default();
    let updater_inst = updater::Updater::new(config, make_fl_paths(), make_wm_paths());
    assert!(updater_inst.is_idle(&health));
}

#[test]
fn is_idle_false_when_active_sessions() {
    let health = make_health(2);
    let config = flatline::config::UpdateConfig::default();
    let updater_inst = updater::Updater::new(config, make_fl_paths(), make_wm_paths());
    assert!(!updater_inst.is_idle(&health));
}

// -- install_dir tests --

#[test]
fn install_dir_returns_bin_subdirectory() {
    let dir = updater::install_dir().expect("install_dir");
    assert!(
        dir.ends_with("bin"),
        "install_dir should end with 'bin', got: {}",
        dir.display()
    );
    let parent_name = dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str());
    assert_eq!(parent_name, Some(".wintermute"));
}

// -- extract_dist_archive tests --

#[test]
fn extract_dist_archive_errors_on_missing_file() {
    let result = updater::extract_dist_archive(std::path::Path::new("/nonexistent/archive.tar.gz"));
    assert!(result.is_err());
}

#[test]
fn extract_dist_archive_extracts_all_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("test-dist.tar.gz");

    // Create a minimal tar.gz archive with the expected dist structure.
    let file = std::fs::File::create(&archive_path).expect("create archive");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    // Add a directory entry.
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "wintermute-1.0.0-test/", &[] as &[u8])
        .expect("add dir");

    // Add a fake binary file.
    let binary_content = b"fake-binary-content";
    let mut header = tar::Header::new_gnu();
    header.set_size(binary_content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            "wintermute-1.0.0-test/wintermute",
            &binary_content[..],
        )
        .expect("add binary");

    // Add a service file inside a subdirectory.
    let service_content = b"[Unit]\nDescription=Test\n";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "wintermute-1.0.0-test/systemd/", &[] as &[u8])
        .expect("add systemd dir");

    let mut header = tar::Header::new_gnu();
    header.set_size(service_content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            "wintermute-1.0.0-test/systemd/wintermute.service",
            &service_content[..],
        )
        .expect("add service");

    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip");

    // Extract and verify.
    let extracted = updater::extract_dist_archive(&archive_path).expect("extract");

    assert!(extracted.exists(), "extracted directory should exist");
    assert!(
        extracted.join("wintermute").exists(),
        "wintermute binary should exist"
    );
    assert!(
        extracted.join("systemd/wintermute.service").exists(),
        "service file should exist"
    );

    let content = std::fs::read(extracted.join("wintermute")).expect("read binary");
    assert_eq!(content, binary_content);
}

/// Helper: build a tar.gz containing a single file entry with an arbitrary
/// (potentially malicious) path set directly in the raw header bytes.
/// The `tar` crate validates paths in `append_data`, so we must bypass it
/// by writing the name field directly into the GNU header.
fn build_tar_gz_with_raw_path(archive_path: &std::path::Path, raw_path: &str, content: &[u8]) {
    let file = std::fs::File::create(archive_path).expect("create archive");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);

    // Write path directly into the raw name field, bypassing validation.
    let name_bytes = raw_path.as_bytes();
    let gnu = header.as_gnu_mut().expect("gnu header");
    let dest = &mut gnu.name[..name_bytes.len()];
    dest.copy_from_slice(name_bytes);
    // Zero the rest of the name field.
    for byte in &mut gnu.name[name_bytes.len()..] {
        *byte = 0;
    }
    header.set_cksum();

    builder.append(&header, content).expect("append raw entry");

    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip");
}

#[test]
fn extract_dist_archive_rejects_path_traversal_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("traversal.tar.gz");

    build_tar_gz_with_raw_path(&archive_path, "legit/../../etc/passwd", b"malicious");

    let result = updater::extract_dist_archive(&archive_path);
    assert!(result.is_err(), "should reject path traversal");
    assert!(
        result
            .expect_err("should fail")
            .to_string()
            .contains("unsafe path"),
        "error should mention unsafe path"
    );
}

#[test]
fn extract_dist_archive_rejects_absolute_path_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("absolute.tar.gz");

    build_tar_gz_with_raw_path(&archive_path, "/etc/passwd", b"malicious");

    let result = updater::extract_dist_archive(&archive_path);
    assert!(result.is_err(), "should reject absolute path");
    assert!(
        result
            .expect_err("should fail")
            .to_string()
            .contains("unsafe path"),
        "error should mention unsafe path"
    );
}

#[test]
fn extract_dist_archive_errors_on_empty_archive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive_path = dir.path().join("empty.tar.gz");

    let file = std::fs::File::create(&archive_path).expect("create archive");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let builder = tar::Builder::new(encoder);

    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip");

    let result = updater::extract_dist_archive(&archive_path);
    assert!(result.is_err(), "should reject empty archive");
    assert!(
        result
            .expect_err("should fail")
            .to_string()
            .contains("empty"),
        "error should mention empty archive"
    );
}

// -- backup_binary tests --

#[tokio::test]
async fn backup_binary_creates_prev_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let updates_dir = dir.path().join("updates");
    let pending_dir = updates_dir.join("pending");
    std::fs::create_dir_all(&pending_dir).expect("create dirs");

    // Create a fake binary to back up.
    let bin_content = b"fake-binary";
    let bin_path = dir.path().join("wintermute");
    std::fs::write(&bin_path, bin_content).expect("write binary");

    let fl_paths = flatline::config::FlatlinePaths {
        root: dir.path().to_path_buf(),
        state_db: dir.path().join("state.db"),
        diagnoses_dir: dir.path().join("diagnoses"),
        patches_dir: dir.path().join("patches"),
        updates_dir: updates_dir.clone(),
        pending_dir,
    };

    let config = flatline::config::UpdateConfig::default();
    let wm_paths = make_wm_paths();

    // Run from the directory containing the fake binary so resolve_binary_path finds it.
    let original_dir = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("chdir");

    let updater_inst = updater::Updater::new(config, fl_paths, wm_paths);
    let result = updater_inst.backup_binary("wintermute").await;

    // Restore working directory before asserting.
    std::env::set_current_dir(original_dir).expect("restore cwd");

    result.expect("backup should succeed");

    let prev_path = updates_dir.join("wintermute.prev");
    assert!(prev_path.exists(), "wintermute.prev should exist");
    let prev_content = std::fs::read(&prev_path).expect("read prev");
    assert_eq!(prev_content, bin_content);
}
