use codex_tools::AdditionalProperties;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub const GH_RUN_WAIT_TOOL_NAME: &str = "gh_run_wait";
pub const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 8;
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 1_800;
pub const MAX_TIMEOUT_SECONDS: u64 = 7_200;

pub fn create_gh_run_wait_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "run_id".to_string(),
            JsonSchema::string(Some("GitHub Actions run ID to wait for.".to_string())),
        ),
        (
            "repo".to_string(),
            JsonSchema::string(Some(
                "Repository in OWNER/REPO form. Defaults to the repository at the current working directory."
                    .to_string(),
            )),
        ),
        (
            "workflow".to_string(),
            JsonSchema::string(Some(
                "Workflow name or filename used to select the latest run when run_id is omitted."
                    .to_string(),
            )),
        ),
        (
            "branch".to_string(),
            JsonSchema::string(Some(
                "Branch used to select the latest run. Defaults to the current branch, then the repository default branch."
                    .to_string(),
            )),
        ),
        (
            "head_sha".to_string(),
            JsonSchema::string(Some(
                "Exact commit SHA required when selecting a run by workflow and branch. Prefer this after a merge or push so a newer run is not selected accidentally."
                    .to_string(),
            )),
        ),
        (
            "interval_seconds".to_string(),
            JsonSchema::integer(Some(format!(
                "Polling interval in seconds (default: {DEFAULT_POLL_INTERVAL_SECONDS})."
            ))),
        ),
        (
            "timeout_seconds".to_string(),
            JsonSchema::integer(Some(format!(
                "Overall timeout in seconds (default: {DEFAULT_TIMEOUT_SECONDS}; maximum: {MAX_TIMEOUT_SECONDS})."
            ))),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: GH_RUN_WAIT_TOOL_NAME.to_string(),
        description: "Wait for a GitHub Actions run by explicit run ID or latest workflow/branch selection, optionally constrained by commit SHA. Protected-environment waits fail immediately with exact-run pending-deployment diagnostics and are never auto-approved; other runs poll until completion or the bounded timeout."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            None,
            Some(AdditionalProperties::Boolean(false)),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "gh_run_wait_spec_tests.rs"]
mod tests;
