//! Egress validation — label-checked output delivery (spec 10.8, 14.6).
//!
//! Validates that outbound data does not violate the No Write Down rule
//! before delivery to a sink. Every egress event is audit-logged.

use std::sync::Arc;

use thiserror::Error;
use tracing::warn;

use crate::kernel::audit::AuditLogger;
use crate::kernel::policy::PolicyEngine;
use crate::types::SecurityLabel;

/// Egress validation errors (spec 10.8).
#[derive(Debug, Error)]
pub enum EgressError {
    /// The data label exceeds the sink's security level (No Write Down).
    #[error("egress denied: data label {data_label:?} exceeds sink label {sink_label:?} for sink '{sink}'")]
    LabelViolation {
        /// Security label of the data being sent.
        data_label: SecurityLabel,
        /// Security label of the target sink.
        sink_label: SecurityLabel,
        /// Name of the target sink.
        sink: String,
    },
    /// The sink is not registered in the policy engine.
    #[error("unknown sink: {0}")]
    UnknownSink(String),
    /// User-facing error message for label violations (spec 14.6).
    #[error("I can't send that information to this channel for privacy reasons.")]
    UserFacingDenied,
}

/// Validates egress before message delivery (spec 10.8).
///
/// Checks the No Write Down rule and logs every egress attempt
/// to the audit trail. On violation, logs the violation before
/// returning an error.
pub struct EgressValidator {
    policy: Arc<PolicyEngine>,
    audit: Arc<AuditLogger>,
}

impl EgressValidator {
    /// Create a new egress validator (spec 10.8).
    pub fn new(policy: Arc<PolicyEngine>, audit: Arc<AuditLogger>) -> Self {
        Self { policy, audit }
    }

    /// Validate and log an egress event (spec 10.8).
    ///
    /// Steps:
    /// 1. Resolve sink label via policy engine
    /// 2. Check No Write Down: data at level X cannot flow to sink below X
    /// 3. On success: audit log the egress event
    /// 4. On failure: audit log the violation, return user-facing error
    pub fn validate_and_log(
        &self,
        payload_label: SecurityLabel,
        sink: &str,
        payload_size: usize,
    ) -> Result<(), EgressError> {
        // Step 1: Resolve sink label.
        let sink_label = self
            .policy
            .sink_label(sink)
            .ok_or_else(|| EgressError::UnknownSink(sink.to_owned()))?;

        // Step 2: Check No Write Down rule (spec 4.3, 6.2).
        if let Err(_violation) = self.policy.check_write(payload_label, sink_label) {
            // Step 4: Log the violation before returning error.
            let msg = format!(
                "egress denied: data label {payload_label:?} exceeds sink label {sink_label:?} for sink '{sink}'"
            );
            warn!(%sink, ?payload_label, ?sink_label, "egress denied: No Write Down violation");

            if let Err(e) = self.audit.log_violation(&msg) {
                warn!(error = %e, "failed to audit log egress violation");
            }

            return Err(EgressError::UserFacingDenied);
        }

        // Step 3: Log successful egress (spec 6.7).
        if let Err(e) = self.audit.log_egress(sink, payload_label, payload_size) {
            warn!(error = %e, "failed to audit log egress event");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::audit::AuditLogger;
    use crate::kernel::policy::PolicyEngine;
    use std::io::{Cursor, Write};
    use std::sync::Mutex;

    // ── Test helpers ──

    /// Shared buffer for capturing audit output in tests.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Cursor::new(Vec::new()))))
        }

        fn contents(&self) -> String {
            let cursor = self.0.lock().expect("test lock");
            String::from_utf8_lossy(cursor.get_ref()).to_string()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("test lock").write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().expect("test lock").flush()
        }
    }

    fn make_validator(buf: &SharedBuf) -> EgressValidator {
        let policy = Arc::new(PolicyEngine::with_defaults());
        let audit = Arc::new(AuditLogger::from_writer(Box::new(buf.clone())));
        EgressValidator::new(policy, audit)
    }

    // ── Tests ──

    /// Sensitive data to sink:telegram:owner (Regulated level) should pass.
    ///
    /// telegram:owner is at Regulated level, which is above Sensitive,
    /// so No Write Down is not violated.
    #[test]
    fn test_egress_allowed_sensitive_to_owner() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result =
            validator.validate_and_log(SecurityLabel::Sensitive, "sink:telegram:owner", 256);

        assert!(result.is_ok(), "sensitive data should egress to owner sink");
    }

    /// Regulated data to whatsapp:reply_to_sender (Public level) should fail.
    ///
    /// Regression test 7: `regulated:health` data cannot egress to WhatsApp.
    #[test]
    fn test_egress_denied_regulated_to_whatsapp() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result = validator.validate_and_log(
            SecurityLabel::Regulated,
            "sink:whatsapp:reply_to_sender",
            512,
        );

        assert!(result.is_err());
        let err = result.expect_err("should be denied");
        assert!(
            matches!(err, EgressError::UserFacingDenied),
            "expected UserFacingDenied, got: {err}"
        );
    }

    /// Sensitive data to a public sink (github:public) should fail.
    #[test]
    fn test_egress_denied_sensitive_to_public() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result =
            validator.validate_and_log(SecurityLabel::Sensitive, "sink:github:public", 128);

        assert!(result.is_err());
        let err = result.expect_err("should be denied");
        assert!(
            matches!(err, EgressError::UserFacingDenied),
            "expected UserFacingDenied, got: {err}"
        );
    }

    /// Unknown sink name should return UnknownSink error.
    #[test]
    fn test_egress_unknown_sink() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result = validator.validate_and_log(SecurityLabel::Public, "sink:unknown:channel", 64);

        assert!(result.is_err());
        let err = result.expect_err("should be unknown sink");
        assert!(
            matches!(err, EgressError::UnknownSink(ref s) if s == "sink:unknown:channel"),
            "expected UnknownSink, got: {err}"
        );
    }

    /// Verify audit log_egress is called on successful egress.
    #[test]
    fn test_egress_audit_logged() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        validator
            .validate_and_log(SecurityLabel::Sensitive, "sink:telegram:owner", 256)
            .expect("should succeed");

        let audit_output = buf.contents();
        assert!(
            !audit_output.is_empty(),
            "audit log should have entries after egress"
        );
        assert!(
            audit_output.contains("egress"),
            "audit log should contain egress event"
        );
        assert!(
            audit_output.contains("sink:telegram:owner"),
            "audit log should contain sink name"
        );
    }

    /// Verify audit log_violation is called on egress denial.
    #[test]
    fn test_egress_violation_logged() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let _result = validator.validate_and_log(
            SecurityLabel::Regulated,
            "sink:whatsapp:reply_to_sender",
            512,
        );

        let audit_output = buf.contents();
        assert!(
            !audit_output.is_empty(),
            "audit log should have entries after violation"
        );
        assert!(
            audit_output.contains("policy_violation"),
            "audit log should contain policy_violation event"
        );
        assert!(
            audit_output.contains("egress denied"),
            "audit log should contain denial reason"
        );
    }

    /// Public data to a public sink should pass (labels equal).
    #[test]
    fn test_egress_allowed_public_to_public() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result = validator.validate_and_log(SecurityLabel::Public, "sink:github:public", 100);

        assert!(result.is_ok(), "public data should egress to public sink");
    }

    /// Internal data to a Sensitive sink should pass (writing up).
    #[test]
    fn test_egress_allowed_internal_to_sensitive_sink() {
        let buf = SharedBuf::new();
        let validator = make_validator(&buf);

        let result = validator.validate_and_log(SecurityLabel::Internal, "sink:notion:digest", 200);

        assert!(
            result.is_ok(),
            "internal data should egress to sensitive sink (notion)"
        );
    }
}
