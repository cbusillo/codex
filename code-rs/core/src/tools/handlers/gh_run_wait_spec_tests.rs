use super::*;
use codex_tools::JsonSchemaPrimitiveType;
use codex_tools::JsonSchemaType;

#[test]
fn gh_run_wait_spec_exposes_bounded_timeout_and_exact_run_fields() {
    let ToolSpec::Function(spec) = create_gh_run_wait_tool() else {
        panic!("expected function tool spec");
    };
    assert_eq!(spec.name, GH_RUN_WAIT_TOOL_NAME);
    assert!(!spec.strict);
    let properties = spec
        .parameters
        .properties
        .as_ref()
        .expect("object properties");
    for name in [
        "run_id",
        "repo",
        "workflow",
        "branch",
        "head_sha",
        "interval_seconds",
        "timeout_seconds",
    ] {
        assert!(properties.contains_key(name), "missing {name} property");
    }
    assert_eq!(
        properties["timeout_seconds"].schema_type,
        Some(JsonSchemaType::Single(JsonSchemaPrimitiveType::Integer))
    );
    assert!(
        properties["timeout_seconds"]
            .description
            .as_deref()
            .is_some_and(|description| description.contains("maximum: 7200"))
    );
}
