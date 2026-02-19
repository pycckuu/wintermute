//! Tests for `src/logging.rs`.

use wintermute::logging::LoggingGuard;

#[test]
fn logging_guard_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<LoggingGuard>();
}

#[test]
fn init_production_creates_logs_dir() {
    let tmp = tempfile::tempdir().expect("should create temp dir");
    let logs_dir = tmp.path().join("logs");
    assert!(!logs_dir.exists());

    // init_production calls tracing_subscriber::registry().init() which
    // can only be called once per process. Since other tests may also
    // initialise the global subscriber, we only verify the function
    // creates the directory — we don't assert on subscriber state.
    // Use try_init pattern: call init_production, check dir exists.
    // Note: this may fail if another test already initialised the global
    // subscriber. In that case the function returns an Err from .init(),
    // but the directory should still be created.
    let _result = wintermute::logging::init_production(&logs_dir);
    assert!(logs_dir.exists(), "logs directory should be created");
}
