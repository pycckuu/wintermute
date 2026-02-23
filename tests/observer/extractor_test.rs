//! Tests for `src/observer/extractor.rs` — extraction parsing and filtering.

use wintermute::observer::extractor::{parse_extractions, Extraction, ExtractionKind};

#[test]
fn parse_valid_json_array() {
    let json = r#"[
        {"kind": "fact", "content": "User prefers dark mode", "confidence": 0.8},
        {"kind": "procedure", "content": "Deploy with cargo build --release", "confidence": 0.9}
    ]"#;

    let result = parse_extractions(json).expect("should parse");
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].kind, ExtractionKind::Fact);
    assert_eq!(result[0].content, "User prefers dark mode");
    assert!((result[0].confidence - 0.8).abs() < f64::EPSILON);
    assert_eq!(result[1].kind, ExtractionKind::Procedure);
}

#[test]
fn parse_extractions_with_surrounding_text() {
    let text = r#"Here are the extractions:
    [{"kind": "preference", "content": "Uses vim keybindings", "confidence": 0.7}]
    That's all I found."#;

    let result = parse_extractions(text).expect("should parse");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].kind, ExtractionKind::Preference);
}

#[test]
fn parse_extractions_filters_low_confidence() {
    let json = r#"[
        {"kind": "fact", "content": "Maybe likes tea", "confidence": 0.3},
        {"kind": "fact", "content": "Definitely likes coffee", "confidence": 0.9}
    ]"#;

    let result = parse_extractions(json).expect("should parse");
    assert_eq!(
        result.len(),
        1,
        "low confidence extraction should be filtered"
    );
    assert_eq!(result[0].content, "Definitely likes coffee");
}

#[test]
fn parse_extractions_filters_empty_content() {
    let json = r#"[
        {"kind": "fact", "content": "", "confidence": 0.9},
        {"kind": "fact", "content": "Valid fact", "confidence": 0.8}
    ]"#;

    let result = parse_extractions(json).expect("should parse");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].content, "Valid fact");
}

#[test]
fn parse_empty_array() {
    let json = "[]";
    let result = parse_extractions(json).expect("should parse");
    assert!(result.is_empty());
}

#[test]
fn parse_invalid_json_returns_empty() {
    let garbage = "not json at all";
    let result = parse_extractions(garbage).expect("should not error");
    assert!(result.is_empty());
}

#[test]
fn parse_partial_json_returns_empty() {
    let partial = r#"[{"kind": "fact", "content": "incomplete"#;
    let result = parse_extractions(partial).expect("should not error");
    assert!(result.is_empty());
}

#[test]
fn extraction_serialization_roundtrip() {
    let extraction = Extraction {
        kind: ExtractionKind::Fact,
        content: "test content".to_owned(),
        confidence: 0.85,
    };

    let json = serde_json::to_string(&extraction).expect("should serialize");
    let deserialized: Extraction = serde_json::from_str(&json).expect("should deserialize");

    assert_eq!(deserialized.kind, extraction.kind);
    assert_eq!(deserialized.content, extraction.content);
    assert!((deserialized.confidence - extraction.confidence).abs() < f64::EPSILON);
}
