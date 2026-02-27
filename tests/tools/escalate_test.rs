//! Tests for the escalate tool.

use wintermute::tools::escalate::escalate_tool_definition;

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

#[test]
fn escalate_tool_definition_has_required_fields() {
    let def = escalate_tool_definition();
    assert_eq!(def.name, "escalate");
    assert!(!def.description.is_empty());

    let schema = &def.input_schema;
    let props = schema.get("properties").expect("should have properties");
    assert!(
        props.get("question").is_some(),
        "should have question field"
    );
    assert!(props.get("context").is_some(), "should have context field");

    let required = schema
        .get("required")
        .expect("should have required")
        .as_array()
        .expect("required should be array");
    assert!(required.iter().any(|v| v.as_str() == Some("question")));
}

#[test]
fn escalate_tool_definition_question_is_required_context_optional() {
    let def = escalate_tool_definition();
    let required: Vec<&str> = def.input_schema["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();

    assert!(required.contains(&"question"));
    assert!(!required.contains(&"context"), "context should be optional");
}
