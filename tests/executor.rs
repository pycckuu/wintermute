//! Integration tests for `src/executor/`.

#[path = "executor/direct_policy_test.rs"]
mod direct_policy_test;
#[path = "executor/docker_invariants_test.rs"]
mod docker_invariants_test;
#[path = "executor/egress_test.rs"]
mod egress_test;
#[path = "executor/exec_result_test.rs"]
mod exec_result_test;
#[path = "executor/health_status_test.rs"]
mod health_status_test;
#[path = "executor/path_traversal_test.rs"]
mod path_traversal_test;
#[path = "executor/redact_result_test.rs"]
mod redact_result_test;
#[path = "executor/redactor_test.rs"]
mod redactor_test;
#[path = "executor/shell_escape_test.rs"]
mod shell_escape_test;
