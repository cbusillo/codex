use std::collections::HashSet;

use code_common::model_presets::ModelPreset;
use code_common::model_presets::ModelUpgrade;
use code_common::model_presets::ReasoningEffortPreset;
use code_common::model_presets::model_preset_available_for_auth;
use code_core::config_types::TextVerbosity as TextVerbosityConfig;
use code_core::protocol_config_types::ReasoningEffort as ProtocolReasoningEffort;
use code_login::AuthMode;
use code_protocol::openai_models::ModelInfo;
use code_protocol::openai_models::ModelVisibility;
use code_protocol::openai_models::ReasoningEffort as RemoteReasoningEffort;

const REMOTE_TEXT_VERBOSITY_ALL: &[TextVerbosityConfig] = &[
    TextVerbosityConfig::Low,
    TextVerbosityConfig::Medium,
    TextVerbosityConfig::High,
];
const REMOTE_TEXT_VERBOSITY_MEDIUM: &[TextVerbosityConfig] = &[TextVerbosityConfig::Medium];

pub(crate) fn merge_remote_models(
    remote_models: Vec<ModelInfo>,
    local_presets: Vec<ModelPreset>,
    auth_mode: Option<AuthMode>,
    supports_pro_only_models: bool,
) -> Vec<ModelPreset> {
    if remote_models.is_empty() {
        return local_presets;
    }

    let mut remote_models = remote_models;
    remote_models.sort_by(|a, b| a.priority.cmp(&b.priority));
    let mut remote_presets: Vec<ModelPreset> = remote_models.into_iter().map(model_info_to_preset).collect();

    let remote_slugs: HashSet<String> = remote_presets
        .iter()
        .map(|preset| preset.model.to_ascii_lowercase())
        .collect();

    for preset in remote_presets.iter_mut() {
        preset.is_default = false;
    }

    for mut preset in local_presets {
        if remote_slugs.contains(&preset.model.to_ascii_lowercase()) {
            continue;
        }
        preset.is_default = false;
        remote_presets.push(preset);
    }

    remote_presets.retain(|preset| {
        preset.show_in_picker
            && model_preset_available_for_auth(preset, auth_mode, supports_pro_only_models)
    });
    if let Some(default) = remote_presets.first_mut() {
        default.is_default = true;
    }

    remote_presets
}

fn model_info_to_preset(info: ModelInfo) -> ModelPreset {
    let retired_codex_model = matches!(
        info.slug.as_str(),
        "gpt-5.2" | "gpt-5.2-codex" | "gpt-5.3-codex" | "gpt-5.3-codex-spark"
    );
    let pro_only = false;
    let show_in_picker = info.visibility == ModelVisibility::List
        && !retired_codex_model
        && !info.slug.eq_ignore_ascii_case("gpt-5.1-codex");

    let supported_text_verbosity = if info.support_verbosity {
        REMOTE_TEXT_VERBOSITY_ALL
    } else {
        REMOTE_TEXT_VERBOSITY_MEDIUM
    };

    let supported_reasoning_efforts = info
        .supported_reasoning_levels
        .into_iter()
        .map(|preset| ReasoningEffortPreset {
            effort: map_reasoning_effort(preset.effort),
            description: preset.description,
        })
        .collect();

    ModelPreset {
        id: info.slug.clone(),
        model: info.slug.clone(),
        display_name: info.display_name,
        description: info.description.unwrap_or_default(),
        default_reasoning_effort: map_reasoning_effort(
            info.default_reasoning_level
                .unwrap_or(RemoteReasoningEffort::None),
        ),
        supported_reasoning_efforts,
        supported_text_verbosity,
        is_default: false,
        upgrade: info.upgrade.map(|upgrade| ModelUpgrade {
            id: upgrade.model,
            reasoning_effort_mapping: None,
            migration_config_key: info.slug,
        }),
        pro_only,
        show_in_picker,
    }
}

fn map_reasoning_effort(effort: RemoteReasoningEffort) -> ProtocolReasoningEffort {
    match effort {
        RemoteReasoningEffort::None => ProtocolReasoningEffort::Minimal,
        RemoteReasoningEffort::Minimal => ProtocolReasoningEffort::Minimal,
        RemoteReasoningEffort::Low => ProtocolReasoningEffort::Low,
        RemoteReasoningEffort::Medium => ProtocolReasoningEffort::Medium,
        RemoteReasoningEffort::High => ProtocolReasoningEffort::High,
        RemoteReasoningEffort::XHigh => ProtocolReasoningEffort::XHigh,
        RemoteReasoningEffort::Max => ProtocolReasoningEffort::Max,
        RemoteReasoningEffort::Ultra => ProtocolReasoningEffort::Ultra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_common::model_presets::builtin_model_presets;
    use code_protocol::openai_models::ApplyPatchToolType;
    use code_protocol::openai_models::ConfigShellToolType;
    use code_protocol::openai_models::TruncationPolicyConfig;
    use code_protocol::openai_models::default_input_modalities;
    use code_protocol::config_types::ReasoningSummary;

    fn remote_model(slug: &str, priority: i32) -> ModelInfo {
        ModelInfo {
            slug: slug.to_string(),
            display_name: slug.to_string(),
            description: Some(format!("{slug} from remote models")),
            default_reasoning_level: Some(RemoteReasoningEffort::Medium),
            supported_reasoning_levels: vec![code_protocol::openai_models::ReasoningEffortPreset {
                effort: RemoteReasoningEffort::Medium,
                description: "balanced".to_string(),
            }],
            shell_type: ConfigShellToolType::Default,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            availability_nux: None,
            upgrade: None,
            base_instructions: String::new(),
            model_messages: None,
            supports_reasoning_summaries: false,
            default_reasoning_summary: ReasoningSummary::Auto,
            support_verbosity: true,
            default_verbosity: None,
            apply_patch_tool_type: Some(ApplyPatchToolType::Freeform),
            web_search_tool_type: Default::default(),
            truncation_policy: TruncationPolicyConfig::tokens(100_000),
            supports_parallel_tool_calls: false,
            supports_image_detail_original: false,
            context_window: Some(100_000),
            max_context_window: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: default_input_modalities(),
            supports_search_tool: false,
            prefer_websockets: false,
            used_fallback_model_metadata: false,
        }
    }

    #[test]
    fn remote_gpt_models_are_discovered_and_prioritized_without_local_presets() {
        let merged = merge_remote_models(
            vec![remote_model("gpt-5.4", 20), remote_model("gpt-5.6", 0)],
            builtin_model_presets(Some(AuthMode::ChatGPT), true),
            Some(AuthMode::ChatGPT),
            true,
        );

        let gpt_5_6 = merged
            .iter()
            .find(|preset| preset.id == "gpt-5.6")
            .expect("new remote GPT model should be visible without a local preset");
        assert_eq!(gpt_5_6.model, "gpt-5.6");
        assert!(gpt_5_6.is_default, "highest-priority remote model should become default");

        let defaults = merged.iter().filter(|preset| preset.is_default).count();
        assert_eq!(defaults, 1, "exactly one merged model should be default");
        assert_eq!(
            merged.iter().filter(|preset| preset.model == "gpt-5.4").count(),
            1,
            "remote metadata should replace matching local presets instead of duplicating them"
        );
    }
}
