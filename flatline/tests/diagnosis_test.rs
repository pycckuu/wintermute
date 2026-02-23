//! Tests for LLM diagnosis parsing and confidence filtering.

use flatline::diagnosis::{parse_diagnosis, Diagnosis, DiagnosisConfidence};

// ---------------------------------------------------------------------------
// parse_diagnosis tests
// ---------------------------------------------------------------------------

#[test]
fn parse_valid_json() {
    let json = r#"{
        "root_cause": "Tool deploy_check has a syntax error",
        "confidence": "high",
        "recommended_action": "quarantine_tool",
        "details": "Quarantine deploy_check and notify user"
    }"#;

    let d = parse_diagnosis(json).expect("should parse valid JSON");
    assert_eq!(d.root_cause, "Tool deploy_check has a syntax error");
    assert_eq!(d.confidence, DiagnosisConfidence::High);
    assert_eq!(d.recommended_action, "quarantine_tool");
    assert_eq!(d.details, "Quarantine deploy_check and notify user");
}

#[test]
fn parse_json_embedded_in_text() {
    let text = r#"Based on my analysis, here is my diagnosis:

{
    "root_cause": "Container OOM killed",
    "confidence": "medium",
    "recommended_action": "reset_sandbox",
    "details": "The container ran out of memory during pip install"
}

I hope this helps!"#;

    let d = parse_diagnosis(text).expect("should find JSON in surrounding text");
    assert_eq!(d.root_cause, "Container OOM killed");
    assert_eq!(d.confidence, DiagnosisConfidence::Medium);
    assert_eq!(d.recommended_action, "reset_sandbox");
}

#[test]
fn parse_invalid_json_returns_none() {
    let text = "This is not JSON at all.";
    assert!(parse_diagnosis(text).is_none());
}

#[test]
fn parse_partial_json_returns_none() {
    let text = r#"{ "root_cause": "something" "#;
    // Missing closing brace and other required fields.
    assert!(parse_diagnosis(text).is_none());
}

#[test]
fn parse_missing_fields_returns_none() {
    let json = r#"{ "root_cause": "something" }"#;
    assert!(parse_diagnosis(json).is_none());
}

#[test]
fn parse_empty_string_returns_none() {
    assert!(parse_diagnosis("").is_none());
}

// ---------------------------------------------------------------------------
// DiagnosisConfidence serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn confidence_serde_roundtrip() {
    for &conf in &[
        DiagnosisConfidence::High,
        DiagnosisConfidence::Medium,
        DiagnosisConfidence::Low,
    ] {
        let json = serde_json::to_string(&conf).expect("serialize");
        let deserialized: DiagnosisConfidence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(conf, deserialized);
    }
}

#[test]
fn confidence_serializes_to_lowercase() {
    assert_eq!(
        serde_json::to_string(&DiagnosisConfidence::High).expect("serialize"),
        "\"high\""
    );
    assert_eq!(
        serde_json::to_string(&DiagnosisConfidence::Medium).expect("serialize"),
        "\"medium\""
    );
    assert_eq!(
        serde_json::to_string(&DiagnosisConfidence::Low).expect("serialize"),
        "\"low\""
    );
}

// ---------------------------------------------------------------------------
// Diagnosis struct serde roundtrip
// ---------------------------------------------------------------------------

#[test]
fn diagnosis_serde_roundtrip() {
    let d = Diagnosis {
        root_cause: "test root cause".to_owned(),
        confidence: DiagnosisConfidence::High,
        recommended_action: "report_only".to_owned(),
        details: "no action needed".to_owned(),
    };

    let json = serde_json::to_string(&d).expect("serialize");
    let deserialized: Diagnosis = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(d.root_cause, deserialized.root_cause);
    assert_eq!(d.confidence, deserialized.confidence);
    assert_eq!(d.recommended_action, deserialized.recommended_action);
    assert_eq!(d.details, deserialized.details);
}

#[test]
fn parse_with_only_braces_in_middle() {
    // Edge case: response has braces but not valid diagnosis JSON.
    let text = "The issue is {something} but I'm not sure.";
    assert!(parse_diagnosis(text).is_none());
}
