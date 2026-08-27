use crate::agent_defaults::model_guide_markdown;
use crate::config_types::ReasoningEffort as ReasoningEffortConfig;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::TextVerbosity as TextVerbosityConfig;
use crate::context_ledger::ContextLedger;
use crate::context_ledger::ContextPersistence;
use crate::context_ledger::ContextSourceKind;
use crate::context_ledger::response_item_bytes;
use crate::environment_context::EnvironmentContext;
use crate::error::Result;
use crate::model_family::ModelFamily;
use crate::openai_tools::OpenAiTool;
use crate::protocol::RateLimitSnapshotEvent;
use crate::protocol::TokenUsage;
use crate::truncate::truncate_middle;
use crate::user_instructions::UserInstructions;
use code_protocol::models::ContentItem;
use code_protocol::models::FunctionCallOutputContentItem;
use code_protocol::models::ResponseItem;
use futures::Stream;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::ops::Deref;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Additional prompt for Code. Can not edit Codex instructions.
const PROMPT_CODER_TEMPLATE: &str = include_str!("../prompt_coder.md");
static BASE_MODEL_DESCRIPTIONS: Lazy<String> = Lazy::new(|| model_guide_markdown());
static DEFAULT_DEVELOPER_PROMPT: Lazy<String> = Lazy::new(|| {
    PROMPT_CODER_TEMPLATE.replace("{MODEL_DESCRIPTIONS}", &BASE_MODEL_DESCRIPTIONS)
});

/// wraps environment context message in a tag for the model to parse more easily.
const ENVIRONMENT_CONTEXT_START: &str = "<environment_context>\n\n";
const ENVIRONMENT_CONTEXT_END: &str = "\n\n</environment_context>";

/// Review thread system prompt. Edit `core/src/review_prompt.md` to customize.
#[allow(dead_code)]
pub const REVIEW_PROMPT: &str = include_str!("../review_prompt.md");

/// API request payload for a single model turn
#[derive(Debug, Clone)]
pub struct Prompt {
    /// Conversation context input items.
    pub input: Vec<ResponseItem>,

    /// Insertion point for request-only context within `input`. Items before
    /// this index are stable history; items at and after it are the live turn.
    pub volatile_context_insert_at: Option<usize>,

    /// Request-only context items to render at `volatile_context_insert_at`.
    pub volatile_context_items: Vec<ResponseItem>,

    /// Whether to store response on server side (disable_response_storage = !store).
    pub store: bool,

    /// Model instructions that are appended to the base instructions.
    pub user_instructions: Option<String>,

    /// A list of key-value pairs that will be added as a developer message
    /// for the model to use
    pub(crate) environment_context: Option<EnvironmentContext>,

    /// Tools available to the model, including additional tools sourced from
    /// external MCP servers.
    pub(crate) tools: Vec<OpenAiTool>,

    /// Status items to be added at the end of the input
    /// These are generated fresh for each request (screenshots, system status)
    pub status_items: Vec<ResponseItem>,

    /// Optional override for the built-in BASE_INSTRUCTIONS.
    pub base_instructions_override: Option<String>,

    /// Whether to prepend the default developer instructions block.
    pub include_additional_instructions: bool,

    /// Additional developer messages to insert immediately after the default
    /// fork instructions but before any environment or user context.
    pub prepend_developer_messages: Vec<String>,

    /// Optional `text.format` for structured outputs (used by side-channel requests).
    pub text_format: Option<TextFormat>,

    /// Optional per-request model slug override.
    pub model_override: Option<String>,

    /// Optional per-request model family override matching `model_override`.
    pub model_family_override: Option<ModelFamily>,
    /// Optional the output schema for the model's response.
    pub output_schema: Option<Value>,
    /// Optional tag used to route debug logs into helper-specific directories.
    pub log_tag: Option<String>,
    /// Optional override for session/conversation identifiers used for caching.
    pub session_id_override: Option<Uuid>,

    /// Optional override for the model guide placeholder in the developer prompt.
    pub model_descriptions: Option<String>,
}

impl Default for Prompt {
    fn default() -> Self {
        Self {
            input: Vec::new(),
            volatile_context_insert_at: None,
            volatile_context_items: Vec::new(),
            store: false,
            user_instructions: None,
            environment_context: None,
            tools: Vec::new(),
            status_items: Vec::new(),
            base_instructions_override: None,
            include_additional_instructions: true,
            prepend_developer_messages: Vec::new(),
            text_format: None,
            model_override: None,
            model_family_override: None,
            output_schema: None,
            log_tag: None,
            session_id_override: None,
            model_descriptions: None,
        }
    }
}

impl Prompt {
    pub(crate) fn get_full_instructions<'a>(&'a self, model: &'a ModelFamily) -> Cow<'a, str> {
        let effective_model = self.model_family_override.as_ref().unwrap_or(model);
        Cow::Borrowed(
            self.base_instructions_override
                .as_deref()
                .unwrap_or(effective_model.base_instructions.deref()),
        )
    }

    pub fn set_log_tag<S: Into<String>>(&mut self, tag: S) {
        self.log_tag = Some(tag.into());
    }

    fn additional_instructions(&self) -> Cow<'_, str> {
        if let Some(custom) = &self.model_descriptions {
            Cow::Owned(PROMPT_CODER_TEMPLATE.replace("{MODEL_DESCRIPTIONS}", custom))
        } else {
            Cow::Borrowed(DEFAULT_DEVELOPER_PROMPT.deref())
        }
    }

    fn get_formatted_user_instructions(&self) -> Option<ResponseItem> {
        let instructions = self.user_instructions.as_ref()?;
        let directory = self
            .environment_context
            .as_ref()
            .and_then(|ctx| ctx.cwd.as_ref())
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_default();
        Some(
            UserInstructions {
                directory,
                text: instructions.clone(),
            }
            .into(),
        )
    }

    fn get_formatted_environment_context(&self) -> Option<String> {
        self.environment_context.as_ref().map(|ec| {
            let ec_str = serde_json::to_string_pretty(ec).unwrap_or_else(|_| format!("{:?}", ec));
            format!("{ENVIRONMENT_CONTEXT_START}{ec_str}{ENVIRONMENT_CONTEXT_END}")
        })
    }

    pub(crate) fn get_formatted_input(&self) -> Vec<ResponseItem> {
        self.get_formatted_input_with_ledger().0
    }

    pub(crate) fn get_formatted_input_with_ledger(&self) -> (Vec<ResponseItem>, ContextLedger) {
        let mut input_with_instructions = Vec::with_capacity(
            self.input.len() + self.volatile_context_items.len() + self.status_items.len() + 3,
        );
        let mut ledger = ContextLedger::default();
        let mut seen_tool_outputs = std::collections::HashSet::new();
        let volatile_context_insert_at = self
            .volatile_context_insert_at
            .unwrap_or(self.input.len())
            .min(self.input.len());
        if self.include_additional_instructions {
            let developer_text = self.additional_instructions().into_owned();
            ledger.push(
                ContextSourceKind::DeveloperInstructions,
                ContextPersistence::Contextual,
                "default developer instructions",
                1,
                developer_text.len(),
                Some("developer:default".to_string()),
            );
            input_with_instructions.push(ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: vec![ContentItem::InputText { text: developer_text }], end_turn: None, phase: None});
            if let Some(ui) = self.get_formatted_user_instructions() {
                let has_user_instructions = self.input.iter().any(|item| {
                    matches!(item, ResponseItem::Message { role, content, .. }
                        if role == "user" && UserInstructions::is_user_instructions(content))
                });
                if !has_user_instructions {
                    add_response_item_to_ledger(&mut ledger, &ui);
                    input_with_instructions.push(ui);
                }
            }
        }
        add_input_items_to_prompt(
            &mut input_with_instructions,
            &mut ledger,
            &mut seen_tool_outputs,
            self.input[..volatile_context_insert_at].iter(),
        );
        add_input_items_to_prompt(
            &mut input_with_instructions,
            &mut ledger,
            &mut seen_tool_outputs,
            self.volatile_context_items.iter(),
        );
        if self.include_additional_instructions {
            if let Some(ec) = self.get_formatted_environment_context() {
                let has_environment_context = self.input.iter().any(|item| {
                    matches!(item, ResponseItem::Message { role, content, .. }
                        if role == "user"
                            && content.iter().any(|c| matches!(c,
                                ContentItem::InputText { text } if text.contains(ENVIRONMENT_CONTEXT_START.trim())
                            )))
                });
                if !has_environment_context {
                    ledger.push(
                        ContextSourceKind::EnvironmentContext,
                        ContextPersistence::GeneratedPerAttempt,
                        "environment context",
                        1,
                        ec.len(),
                        Some("environment_context".to_string()),
                    );
                    input_with_instructions.push(ResponseItem::Message {
                        id: None,
                        role: "user".to_string(),
                        content: vec![ContentItem::InputText { text: ec }], end_turn: None, phase: None});
                }
            }
            for message in &self.prepend_developer_messages {
                let trimmed = message.trim();
                if trimmed.is_empty() {
                    continue;
                }
                ledger.push(
                    classify_prepend_developer_message(trimmed),
                    ContextPersistence::RequestOnly,
                    "prepended developer message",
                    1,
                    trimmed.len(),
                    duplicate_key_for_prepend_developer_message(trimmed),
                );
                input_with_instructions.push(ResponseItem::Message {
                    id: None,
                    role: "developer".to_string(),
                    content: vec![ContentItem::InputText {
                        text: trimmed.to_string(),
                    }], end_turn: None, phase: None});
            }
        }
        add_input_items_to_prompt(
            &mut input_with_instructions,
            &mut ledger,
            &mut seen_tool_outputs,
            self.input[volatile_context_insert_at..].iter(),
        );

        // Add status items at the end so they're fresh for each request
        for item in &self.status_items {
            ledger.push(
                classify_status_item(item),
                ContextPersistence::GeneratedPerAttempt,
                "status item",
                1,
                response_item_bytes(item),
                duplicate_key_for_status_item(item),
            );
            input_with_instructions.push(item.clone());
        }

        // Limit screenshots to maximum 5 (keep first and last 4)
        limit_screenshots_in_input(&mut input_with_instructions);

        (input_with_instructions, ledger)
    }

    pub(crate) fn context_ledger_for_request(
        &self,
        model: &ModelFamily,
        input: &[ResponseItem],
        tools_json: &[serde_json::Value],
    ) -> ContextLedger {
        let mut ledger = self.context_ledger_for_formatted_input(input);
        let full_instructions = self.get_full_instructions(model);
        ledger.push(
            ContextSourceKind::BaseInstructions,
            ContextPersistence::Contextual,
            "base instructions",
            1,
            full_instructions.len(),
            Some("base_instructions".to_string()),
        );

        let tools_bytes = tools_json
            .iter()
            .map(|tool| serde_json::to_string(tool).map(|s| s.len()).unwrap_or(0))
            .sum::<usize>();
        ledger.push(
            ContextSourceKind::ToolSchema,
            ContextPersistence::Contextual,
            "tool schemas",
            tools_json.len(),
            tools_bytes,
            Some("tool_schemas".to_string()),
        );
        ledger
    }

    fn context_ledger_for_formatted_input(&self, input: &[ResponseItem]) -> ContextLedger {
        let mut ledger = ContextLedger::default();
        for item in input {
            if self.status_items.iter().any(|status_item| status_item == item) {
                ledger.push(
                    classify_status_item(item),
                    ContextPersistence::GeneratedPerAttempt,
                    "status item",
                    1,
                    response_item_bytes(item),
                    duplicate_key_for_status_item(item),
                );
                continue;
            }
            add_response_item_to_ledger(&mut ledger, item);
        }
        ledger
    }

    pub fn set_tools(&mut self, tools: Vec<OpenAiTool>) {
        self.tools = tools;
    }

    /// Creates a formatted user instructions message from a string
    #[allow(dead_code)]
    pub(crate) fn format_user_instructions_message(ui: &str) -> ResponseItem {
        UserInstructions {
            directory: String::new(),
            text: ui.to_string(),
        }
        .into()
    }
}

fn add_input_items_to_prompt<'a, I>(
    input_with_instructions: &mut Vec<ResponseItem>,
    ledger: &mut ContextLedger,
    seen_tool_outputs: &mut std::collections::HashSet<String>,
    items: I,
) where
    I: IntoIterator<Item = &'a ResponseItem>,
{
    // Deduplicate function call outputs before adding to input.
    for item in items {
        if classify_input_item(item) == ContextSourceKind::ToolOutput
            && let Some(duplicate_key) = duplicate_key_for_input_item(item)
            && !seen_tool_outputs.insert(duplicate_key.clone())
        {
            tracing::debug!("Filtering duplicate tool output from input: {duplicate_key}");
            continue;
        }
        add_response_item_to_ledger(ledger, item);
        input_with_instructions.push(item.clone());
    }
}

fn classify_prepend_developer_message(text: &str) -> ContextSourceKind {
    if text.contains("<auto_review_ledger") {
        ContextSourceKind::AutoReviewLedger
    } else if text.contains("<memory") || text.to_ascii_lowercase().contains("memory") {
        ContextSourceKind::MemoryInstructions
    } else {
        ContextSourceKind::DeveloperInstructions
    }
}

fn duplicate_key_for_prepend_developer_message(text: &str) -> Option<String> {
    if classify_prepend_developer_message(text) == ContextSourceKind::AutoReviewLedger {
        Some("developer:prepend:auto_review_ledger".to_string())
    } else {
        Some(format!("developer:prepend:{:016x}", stable_hash(text)))
    }
}

fn add_response_item_to_ledger(ledger: &mut ContextLedger, item: &ResponseItem) {
    if let ResponseItem::Message { role, content, .. } = item {
        if role == "user" && UserInstructions::is_user_instructions(content) {
            add_user_instructions_to_ledger(ledger, content);
            return;
        }
    }

    ledger.push(
        classify_input_item(item),
        persistence_for_input_item(item),
        label_for_input_item(item),
        1,
        response_item_bytes(item),
        duplicate_key_for_input_item(item),
    );
}

fn add_user_instructions_to_ledger(ledger: &mut ContextLedger, content: &[ContentItem]) {
    let text = content_text(content);
    let skills_marker = "## Skills";
    if let Some((project_docs, skills_manifest)) = text.split_once(skills_marker) {
        ledger.push(
            ContextSourceKind::UserInstructions,
            ContextPersistence::Contextual,
            "user/project instructions",
            1,
            project_docs.len(),
            Some("user_instructions".to_string()),
        );
        ledger.push(
            ContextSourceKind::SkillsManifest,
            ContextPersistence::Contextual,
            "skills manifest",
            1,
            skills_marker.len() + skills_manifest.len(),
            Some("skills_manifest".to_string()),
        );
    } else {
        ledger.push(
            ContextSourceKind::UserInstructions,
            ContextPersistence::Contextual,
            "user/project instructions",
            1,
            text.len(),
            Some("user_instructions".to_string()),
        );
    }
}

fn classify_input_item(item: &ResponseItem) -> ContextSourceKind {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            classify_user_message(content)
        }
        ResponseItem::Message { role, .. } if role == "developer" => {
            ContextSourceKind::DeveloperInstructions
        }
        ResponseItem::Message { .. } => ContextSourceKind::ConversationHistory,
        ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. } => ContextSourceKind::ToolOutput,
        ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. } => ContextSourceKind::ConversationHistory,
        ResponseItem::Reasoning { .. }
        | ResponseItem::GhostSnapshot { .. }
        | ResponseItem::CompactionSummary { .. }
        | ResponseItem::ContextCompaction { .. } => ContextSourceKind::ConversationHistory,
        ResponseItem::Other => ContextSourceKind::Unknown,
    }
}

fn classify_user_message(content: &[ContentItem]) -> ContextSourceKind {
    let text = content_text(content);
    if text.starts_with("<skill>\n") {
        ContextSourceKind::ExplicitSkill
    } else if text.contains(ENVIRONMENT_CONTEXT_START.trim()) {
        ContextSourceKind::EnvironmentContext
    } else if UserInstructions::is_user_instructions(content) {
        if text.contains("### Available skills") {
            ContextSourceKind::SkillsManifest
        } else {
            ContextSourceKind::UserInstructions
        }
    } else if content
        .iter()
        .any(|item| matches!(item, ContentItem::InputImage { .. }))
    {
        ContextSourceKind::BrowserStatus
    } else {
        ContextSourceKind::ConversationHistory
    }
}

fn classify_status_item(item: &ResponseItem) -> ContextSourceKind {
    match item {
        ResponseItem::Message { content, .. }
            if content
                .iter()
                .any(|item| matches!(item, ContentItem::InputImage { .. })) =>
        {
            ContextSourceKind::BrowserStatus
        }
        _ => ContextSourceKind::StatusItem,
    }
}

fn persistence_for_input_item(item: &ResponseItem) -> ContextPersistence {
    match classify_input_item(item) {
        ContextSourceKind::ExplicitSkill => ContextPersistence::RequestOnly,
        ContextSourceKind::DeveloperInstructions
        | ContextSourceKind::UserInstructions
        | ContextSourceKind::SkillsManifest => ContextPersistence::Contextual,
        ContextSourceKind::EnvironmentContext | ContextSourceKind::BrowserStatus => {
            ContextPersistence::GeneratedPerAttempt
        }
        ContextSourceKind::ToolOutput => ContextPersistence::ToolResult,
        _ => ContextPersistence::Persisted,
    }
}

fn label_for_input_item(item: &ResponseItem) -> &'static str {
    match classify_input_item(item) {
        ContextSourceKind::ExplicitSkill => "explicit skill",
        ContextSourceKind::EnvironmentContext => "environment context",
        ContextSourceKind::BrowserStatus => "browser/status context",
        ContextSourceKind::AutoReviewLedger => "auto review ledger",
        ContextSourceKind::ToolOutput => "tool output",
        ContextSourceKind::DeveloperInstructions => "developer message",
        ContextSourceKind::UserInstructions => "user/project instructions",
        ContextSourceKind::SkillsManifest => "skills manifest",
        ContextSourceKind::ConversationHistory => "conversation history",
        _ => "input item",
    }
}

fn duplicate_key_for_input_item(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            let source = classify_user_message(content);
            match source {
                ContextSourceKind::ExplicitSkill => skill_name_from_content(content)
                    .map(|name| format!("explicit_skill:{name}"))
                    .or_else(|| Some(format!("explicit_skill:{:016x}", stable_hash(&content_text(content))))),
                ContextSourceKind::EnvironmentContext => Some("environment_context".to_string()),
                ContextSourceKind::UserInstructions | ContextSourceKind::SkillsManifest => {
                    Some("user_instructions".to_string())
                }
                _ => None,
            }
        }
        ResponseItem::FunctionCallOutput { call_id, .. }
        | ResponseItem::CustomToolCallOutput { call_id, .. } => Some(format!("tool_output:{call_id}")),
        ResponseItem::ToolSearchOutput { call_id, .. } => {
            call_id.as_ref().map(|call_id| format!("tool_output:{call_id}"))
        }
        _ => None,
    }
}

fn duplicate_key_for_status_item(item: &ResponseItem) -> Option<String> {
    match classify_status_item(item) {
        ContextSourceKind::BrowserStatus => Some("browser_status".to_string()),
        ContextSourceKind::StatusItem => Some(format!("status:{:016x}", stable_hash(&format!("{item:?}")))),
        _ => None,
    }
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn skill_name_from_content(content: &[ContentItem]) -> Option<String> {
    let text = content_text(content);
    text.lines()
        .find_map(|line| line.strip_prefix("<name>")?.strip_suffix("</name>"))
        .map(ToOwned::to_owned)
}

fn stable_hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug)]
pub enum ResponseEvent {
    ContextLedger(ContextLedger),
    Created {
        response_id: Option<String>,
        response_model: Option<String>,
    },
    ResponseHeaders(serde_json::Value),
    OutputItemDone { item: ResponseItem, sequence_number: Option<u64>, output_index: Option<u32> },
    /// Indicates that the server will include reasoning content on this stream.
    ///
    /// Some providers expose this as a handshake header on websocket streams.
    ServerReasoningIncluded(bool),
    Completed {
        response_id: String,
        token_usage: Option<TokenUsage>,
    },
    OutputTextDelta {
        delta: String,
        item_id: Option<String>,
        sequence_number: Option<u64>,
        output_index: Option<u32>,
    },
    ReasoningSummaryDelta {
        delta: String,
        item_id: Option<String>,
        sequence_number: Option<u64>,
        output_index: Option<u32>,
        summary_index: Option<u32>,
    },
    ReasoningContentDelta {
        delta: String,
        item_id: Option<String>,
        sequence_number: Option<u64>,
        output_index: Option<u32>,
        content_index: Option<u32>,
    },
    ReasoningSummaryPartAdded,
    WebSearchCallBegin {
        call_id: String,
    },
    WebSearchCallCompleted {
        call_id: String,
        query: Option<String>,
    },
    RateLimits(RateLimitSnapshotEvent),
    ModelsEtag(String),
}

#[derive(Debug, Serialize)]
pub(crate) struct Reasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<ReasoningEffortConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<ReasoningSummaryConfig>,
}

/// Text configuration for verbosity/format in OpenAI API responses.
#[derive(Debug, Clone)]
pub(crate) struct Text {
    pub(crate) verbosity: OpenAiTextVerbosity,
    pub(crate) format: Option<TextFormat>,
}

impl serde::Serialize for Text {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("verbosity", &self.verbosity)?;
        if let Some(fmt) = &self.format {
            map.serialize_entry("format", fmt)?;
        }
        map.end()
    }
}

/// OpenAI text verbosity level for serialization.
#[derive(Debug, Serialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OpenAiTextVerbosity {
    Low,
    #[default]
    Medium,
    High,
}

impl From<TextVerbosityConfig> for OpenAiTextVerbosity {
    fn from(verbosity: TextVerbosityConfig) -> Self {
        match verbosity {
            TextVerbosityConfig::Low => OpenAiTextVerbosity::Low,
            TextVerbosityConfig::Medium => OpenAiTextVerbosity::Medium,
            TextVerbosityConfig::High => OpenAiTextVerbosity::High,
        }
    }
}

/// Optional structured output format for `text.format` in the Responses API.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct TextFormat {
    #[serde(rename = "type")]
    pub r#type: String, // e.g. "json_schema"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// Limits the number of screenshots in the input to a maximum of 5.
/// Keeps the first screenshot and the last 4 screenshots.
/// Replaces removed screenshots with a placeholder message.
fn limit_screenshots_in_input(input: &mut Vec<ResponseItem>) {
    // Find all screenshot positions
    let mut screenshot_positions = Vec::new();
    
    for (idx, item) in input.iter().enumerate() {
        if let ResponseItem::Message { content, .. } = item {
            let has_screenshot = content
                .iter()
                .any(|c| matches!(c, ContentItem::InputImage { .. }));
            if has_screenshot {
                screenshot_positions.push(idx);
            }
        }
    }
    
    // If we have 5 or fewer screenshots, no action needed
    if screenshot_positions.len() <= 5 {
        return;
    }
    
    // Determine which screenshots to keep
    let mut positions_to_keep = std::collections::HashSet::new();
    
    // Keep the first screenshot
    if let Some(&first) = screenshot_positions.first() {
        positions_to_keep.insert(first);
    }
    
    // Keep the last 4 screenshots
    let last_four_start = screenshot_positions.len().saturating_sub(4);
    for &pos in &screenshot_positions[last_four_start..] {
        positions_to_keep.insert(pos);
    }
    
    // Replace screenshots that should be removed
    for &pos in &screenshot_positions {
        if !positions_to_keep.contains(&pos) {
            if let Some(ResponseItem::Message { content, .. }) = input.get_mut(pos) {
                // Replace image content with placeholder message
                let mut new_content = Vec::new();
                for item in content.iter() {
                    match item {
                        ContentItem::InputImage { .. } => {
                            new_content.push(ContentItem::InputText {
                                text: "[screenshot no longer available]".to_string(),
                            });
                        }
                        other => new_content.push(other.clone()),
                    }
                }
                *content = new_content;
            }
        }
    }
    
    tracing::debug!(
        "Limited screenshots from {} to {} (kept first and last 4)",
        screenshot_positions.len(),
        positions_to_keep.len()
    );
}

const SPARK_IMAGE_PLACEHOLDER: &str =
    "[image omitted: selected -spark model does not support image inputs]";
const IMAGE_GENERATION_REVISED_PROMPT_MAX_BYTES: usize = 8 * 1024;

/// Replace `input_image` payloads with text placeholders for models that are
/// known not to accept image inputs.
pub(crate) fn replace_image_payloads_for_model(input: &mut Vec<ResponseItem>, model_slug: &str) {
    if !model_slug.to_ascii_lowercase().contains("-spark") {
        return;
    }

    for item in input.iter_mut() {
        match item {
            ResponseItem::Message { content, .. } => {
                for content_item in content.iter_mut() {
                    if matches!(content_item, ContentItem::InputImage { .. }) {
                        *content_item = ContentItem::InputText {
                            text: SPARK_IMAGE_PLACEHOLDER.to_string(),
                        };
                    }
                }
            }
            ResponseItem::FunctionCallOutput { output, .. } => {
                if let Some(content_items) = output.content_items_mut() {
                    for output_item in content_items.iter_mut() {
                        if matches!(output_item, FunctionCallOutputContentItem::InputImage { .. }) {
                            *output_item = FunctionCallOutputContentItem::InputText {
                                text: SPARK_IMAGE_PLACEHOLDER.to_string(),
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Replace upstream `image_generation_call` output items with bounded text when
/// we replay stateless history.
pub(crate) fn rewrite_image_generation_calls_for_input(input: &mut Vec<ResponseItem>) {
    let original_items = std::mem::take(input);
    *input = original_items
        .into_iter()
        .map(|item| match item {
            ResponseItem::ImageGenerationCall {
                status,
                revised_prompt,
                result,
                ..
            } => {
                let bytes = result.len();
                let mut text = format!(
                    "[image generation result omitted from conversation replay; status={status}; {bytes} bytes]"
                );
                if let Some(revised_prompt) = revised_prompt {
                    text.push_str("\nRevised prompt: ");
                    text.push_str(
                        &truncate_middle(
                            &revised_prompt,
                            IMAGE_GENERATION_REVISED_PROMPT_MAX_BYTES,
                        )
                        .0,
                    );
                }

                ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText { text }],
                    end_turn: None,
                    phase: None,
                }
            }
            _ => item,
        })
        .collect();
}

/// Request object that is serialized as JSON and POST'ed when using the
/// Responses API.
#[derive(Debug, Serialize)]
pub(crate) struct ResponsesApiRequest<'a> {
    pub(crate) model: &'a str,
    pub(crate) instructions: &'a str,
    // TODO(mbolin): ResponseItem::Other should not be serialized. Currently,
    // we code defensively to avoid this case, but perhaps we should use a
    // separate enum for serialization.
    pub(crate) input: &'a Vec<ResponseItem>,
    pub(crate) tools: &'a [serde_json::Value],
    pub(crate) tool_choice: &'static str,
    pub(crate) parallel_tool_calls: bool,
    pub(crate) reasoning: Option<Reasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) text: Option<Text>,
    /// true when using the Responses API.
    pub(crate) store: bool,
    pub(crate) stream: bool,
    pub(crate) include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) prompt_cache_key: Option<String>,
}

pub(crate) fn create_reasoning_param_for_request(
    model_family: &ModelFamily,
    effort: Option<ReasoningEffortConfig>,
    summary: ReasoningSummaryConfig,
) -> Option<Reasoning> {
    if !model_family.supports_reasoning_summaries {
        return None;
    }

    let summary = match summary {
        ReasoningSummaryConfig::Auto => model_family.default_reasoning_summary,
        other => other,
    };

    let summary = if summary == ReasoningSummaryConfig::None {
        None
    } else {
        Some(summary)
    };

    let effort = effort.map(reasoning_effort_for_request);

    Some(Reasoning { effort, summary })
}

fn reasoning_effort_for_request(effort: ReasoningEffortConfig) -> ReasoningEffortConfig {
    match effort {
        ReasoningEffortConfig::Ultra => ReasoningEffortConfig::Max,
        effort => effort,
    }
}

// Removed legacy TextControls helper; use `Text` with `OpenAiTextVerbosity` instead.

pub struct ResponseStream {
    pub(crate) rx_event: mpsc::Receiver<Result<ResponseEvent>>,
}

impl Stream for ResponseStream {
    type Item = Result<ResponseEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use crate::model_family::find_family_for_model;
    use crate::context_ledger::ContextPersistence;
    use crate::context_ledger::ContextSourceKind;
    use code_apply_patch::APPLY_PATCH_TOOL_INSTRUCTIONS;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn ultra_reasoning_uses_max_for_requests() {
        assert_eq!(
            reasoning_effort_for_request(ReasoningEffortConfig::Ultra),
            ReasoningEffortConfig::Max
        );
        assert_eq!(
            reasoning_effort_for_request(ReasoningEffortConfig::Max),
            ReasoningEffortConfig::Max
        );
    }

    fn message(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn context_ledger_classifies_request_sources() {
        let model_family = find_family_for_model("gpt-5.1").expect("model family");
        let prompt = Prompt {
            input: vec![
                message("user", "hello"),
                message(
                    "user",
                    "<skill>\n<name>manual-skill</name>\n<path>skills/manual/SKILL.md</path>\nbody\n</skill>",
                ),
                ResponseItem::FunctionCallOutput {
                    call_id: "call-1".to_string(),
                    output: code_protocol::models::FunctionCallOutputPayload::from_text(
                        "tool output".to_string(),
                    ),
                },
            ],
            user_instructions: Some(
                "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
            ),
            status_items: vec![message("user", "status marker")],
            include_additional_instructions: false,
            ..Prompt::default()
        };

        let mut formatted = prompt.get_formatted_input();
        rewrite_image_generation_calls_for_input(&mut formatted);
        replace_image_payloads_for_model(&mut formatted, "gpt-5.1");
        let tools = vec![serde_json::json!({
            "type": "function",
            "name": "demo_tool",
            "parameters": {}
        })];
        let ledger = prompt.context_ledger_for_request(&model_family, &formatted, &tools);

        assert!(ledger.total_bytes() > 0);
        assert!(ledger.total_estimated_tokens() > 0);
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::BaseInstructions
                && entry.persistence == ContextPersistence::Contextual
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::ExplicitSkill
                && entry.persistence == ContextPersistence::RequestOnly
                && entry.duplicate_key.as_deref() == Some("explicit_skill:manual-skill")
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::ToolOutput
                && entry.persistence == ContextPersistence::ToolResult
                && entry.duplicate_key.as_deref() == Some("tool_output:call-1")
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::StatusItem
                && entry.persistence == ContextPersistence::GeneratedPerAttempt
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::ToolSchema
                && entry.persistence == ContextPersistence::Contextual
        }));
    }

    #[test]
    fn context_ledger_sees_project_doc_skills_manifest() {
        let prompt = Prompt {
            user_instructions: Some(
                "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
            ),
            include_additional_instructions: true,
            ..Prompt::default()
        };

        let (formatted, ledger) = prompt.get_formatted_input_with_ledger();

        assert!(formatted.iter().any(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.iter().any(|content| matches!(content,
                    ContentItem::InputText { text } if text.contains("### Available skills")
                ))
            }
            _ => false,
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::UserInstructions
                && entry.duplicate_key.as_deref() == Some("user_instructions")
        }));
        assert!(ledger.entries().iter().any(|entry| {
            entry.source == ContextSourceKind::SkillsManifest
                && entry.duplicate_key.as_deref() == Some("skills_manifest")
        }));
    }

    #[test]
    fn stable_project_instructions_precede_volatile_context() {
        let prompt = Prompt {
            user_instructions: Some(
                "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
            ),
            prepend_developer_messages: vec![
                "<auto_review_ledger schema_version=\"1\">fresh run state</auto_review_ledger>"
                    .to_string(),
            ],
            environment_context: Some(EnvironmentContext::new(
                Some("/workspace/project".into()),
                None,
                None,
                None,
            )),
            include_additional_instructions: true,
            ..Prompt::default()
        };

        let (formatted, ledger) = prompt.get_formatted_input_with_ledger();

        let item_kinds = formatted
            .iter()
            .map(classify_input_item)
            .collect::<Vec<_>>();
        let user_instructions_pos = item_kinds
            .iter()
            .position(|kind| {
                matches!(
                    kind,
                    ContextSourceKind::UserInstructions | ContextSourceKind::SkillsManifest
                )
            })
            .expect("stable user/project instructions item");
        let volatile_developer_pos = formatted
            .iter()
            .position(|item| match item {
                ResponseItem::Message { content, .. } => content.iter().any(|content| {
                    matches!(content,
                        ContentItem::InputText { text } if text.contains("<auto_review_ledger")
                    )
                }),
                _ => false,
            })
            .expect("prepended auto-review developer item");
        let environment_pos = item_kinds
            .iter()
            .position(|kind| *kind == ContextSourceKind::EnvironmentContext)
            .expect("environment context item");

        assert!(
            user_instructions_pos < volatile_developer_pos,
            "stable project instructions should precede volatile prepended developer context"
        );
        assert!(
            user_instructions_pos < environment_pos,
            "stable project instructions should precede generated environment context"
        );

        let ledger_kinds = ledger
            .entries()
            .iter()
            .map(|entry| entry.source)
            .collect::<Vec<_>>();
        let skills_pos = ledger_kinds
            .iter()
            .position(|kind| *kind == ContextSourceKind::SkillsManifest)
            .expect("skills manifest ledger entry");
        let auto_review_pos = ledger_kinds
            .iter()
            .position(|kind| *kind == ContextSourceKind::AutoReviewLedger)
            .expect("auto review ledger entry");

        assert!(
            skills_pos < auto_review_pos,
            "stable skills manifest should precede volatile auto-review ledger"
        );
    }

    #[test]
    fn volatile_context_does_not_break_static_prefix() {
        fn formatted_text(prompt: &Prompt) -> String {
            prompt
                .get_formatted_input()
                .iter()
                .map(|item| match item {
                    ResponseItem::Message { role, content, .. } => {
                        let text = content
                            .iter()
                            .filter_map(|content| match content {
                                ContentItem::InputText { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("{role}:{text}")
                    }
                    _ => format!("{item:?}"),
                })
                .collect::<Vec<_>>()
                .join("\n---\n")
        }

        fn prompt_with(ledger: &str, cwd: &str) -> Prompt {
            Prompt {
                user_instructions: Some(
                    "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
                ),
                prepend_developer_messages: vec![format!(
                    "<auto_review_ledger schema_version=\"1\">{ledger}</auto_review_ledger>"
                )],
                environment_context: Some(EnvironmentContext::new(
                    Some(cwd.into()),
                    None,
                    None,
                    None,
                )),
                include_additional_instructions: true,
                ..Prompt::default()
            }
        }

        let first = formatted_text(&prompt_with(
            "first volatile state",
            "/Users/me/.code/working/code/branches/feature-one",
        ));
        let second = formatted_text(&prompt_with(
            "second volatile state",
            "/Users/me/.code/working/code/branches/feature-two",
        ));
        let common_prefix_len = first
            .bytes()
            .zip(second.bytes())
            .take_while(|(left, right)| left == right)
            .count();
        let static_marker_end = first
            .find("### Available skills")
            .expect("skills marker")
            + "### Available skills".len();

        assert!(
            common_prefix_len >= static_marker_end,
            "volatile context should not appear before the static AGENTS/skills prefix"
        );
    }

    #[test]
    fn managed_worktree_subdirectories_do_not_break_static_prefix() {
        fn rendered_user_instructions(prompt: &Prompt) -> String {
            prompt
                .get_formatted_input()
                .iter()
                .find_map(|item| match item {
                    ResponseItem::Message { content, .. } => content.iter().find_map(|content| {
                        match content {
                            ContentItem::InputText { text }
                                if text.starts_with("# AGENTS.md instructions for ") =>
                            {
                                Some(text.clone())
                            }
                            _ => None,
                        }
                    }),
                    _ => None,
                })
                .expect("user instructions")
        }

        fn prompt_with(cwd: &str) -> Prompt {
            Prompt {
                user_instructions: Some(
                    "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
                ),
                environment_context: Some(EnvironmentContext::new(
                    Some(cwd.into()),
                    None,
                    None,
                    None,
                )),
                include_additional_instructions: true,
                ..Prompt::default()
            }
        }

        let first = rendered_user_instructions(&prompt_with(
            "/Users/me/.code/working/code/branches/feature-one/code-rs/core",
        ));
        let second = rendered_user_instructions(&prompt_with(
            "/Users/me/.code/working/code/branches/feature-two/code-rs/core",
        ));

        assert_eq!(first, second);
        assert!(first.starts_with(
            "# AGENTS.md instructions for /.code/working/code/branches/<branch>/code-rs/core"
        ));
    }

    #[test]
    fn volatile_context_sits_between_history_and_current_turn() {
        fn formatted_text(prompt: &Prompt) -> String {
            prompt
                .get_formatted_input()
                .iter()
                .map(|item| match item {
                    ResponseItem::Message { role, content, .. } => {
                        let text = content
                            .iter()
                            .filter_map(|content| match content {
                                ContentItem::InputText { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!("{role}:{text}")
                    }
                    _ => format!("{item:?}"),
                })
                .collect::<Vec<_>>()
                .join("\n---\n")
        }

        let prompt = Prompt {
            input: vec![
                message("user", "old stable user turn"),
                message("user", "live current turn"),
            ],
            volatile_context_insert_at: Some(1),
            user_instructions: Some(
                "project guidance\n\n## Skills\n### Available skills\n- demo: Demo skill".to_string(),
            ),
            prepend_developer_messages: vec![
                "<auto_review_ledger schema_version=\"1\">fresh run state</auto_review_ledger>"
                    .to_string(),
            ],
            environment_context: Some(EnvironmentContext::new(
                Some("/workspace/project".into()),
                None,
                None,
                None,
            )),
            include_additional_instructions: true,
            ..Prompt::default()
        };

        let rendered = formatted_text(&prompt);
        let skills_pos = rendered.find("### Available skills").expect("skills");
        let history_pos = rendered
            .find("old stable user turn")
            .expect("stable history");
        let ledger_pos = rendered.find("<auto_review_ledger").expect("ledger");
        let env_pos = rendered.find("<environment_context>").expect("environment");
        let current_pos = rendered.find("live current turn").expect("current turn");

        assert!(skills_pos < history_pos);
        assert!(history_pos < env_pos);
        assert!(env_pos < ledger_pos);
        assert!(ledger_pos < current_pos);
    }

    #[test]
    fn volatile_context_items_sit_between_history_and_current_turn() {
        let prompt = Prompt {
            input: vec![
                message("user", "old stable user turn"),
                message("user", "live current turn"),
            ],
            volatile_context_insert_at: Some(1),
            volatile_context_items: vec![message("user", "timeline env delta")],
            include_additional_instructions: false,
            ..Prompt::default()
        };

        let rendered = prompt
            .get_formatted_input()
            .iter()
            .map(|item| match item {
                ResponseItem::Message { role, content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|content| match content {
                            ContentItem::InputText { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("{role}:{text}")
                }
                _ => format!("{item:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n---\n");

        let history_pos = rendered
            .find("old stable user turn")
            .expect("stable history");
        let timeline_pos = rendered.find("timeline env delta").expect("timeline");
        let current_pos = rendered.find("live current turn").expect("current turn");

        assert!(history_pos < timeline_pos);
        assert!(timeline_pos < current_pos);
    }

    #[test]
    fn replace_image_payloads_for_spark_model_rewrites_images() {
        let mut input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![
                    ContentItem::InputText {
                        text: "Please inspect this".to_string(),
                    },
                    ContentItem::InputImage {
                        image_url: "data:image/png;base64,AAA".to_string(),
                    },
                ],
                end_turn: None,
                phase: None,
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call_1".to_string(),
                output: code_protocol::models::FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,BBB".to_string(),
                        detail: None,
                    },
                ]),
            },
        ];

        replace_image_payloads_for_model(&mut input, "gpt-5.3-codex-spark");

        assert!(matches!(
            &input[0],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.get(1),
                    Some(ContentItem::InputText { text }) if text == SPARK_IMAGE_PLACEHOLDER
                )
        ));

        assert!(matches!(
            &input[1],
            ResponseItem::FunctionCallOutput { output, .. }
                if matches!(
                    output.content_items().and_then(|items| items.first()),
                    Some(FunctionCallOutputContentItem::InputText { text })
                        if text == SPARK_IMAGE_PLACEHOLDER
                )
        ));
    }

    #[test]
    fn replace_image_payloads_for_non_spark_model_keeps_images() {
        let mut input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: "data:image/png;base64,AAA".to_string(),
            }],
            end_turn: None,
            phase: None,
        }];

        replace_image_payloads_for_model(&mut input, "gpt-5.3-codex");

        assert!(matches!(
            &input[0],
            ResponseItem::Message { content, .. }
                if matches!(content.first(), Some(ContentItem::InputImage { .. }))
        ));
    }

    #[test]
    fn rewrite_image_generation_calls_for_input_converts_to_bounded_assistant_message() {
        let mut input = vec![ResponseItem::ImageGenerationCall {
            id: "ig_1".to_string(),
            status: "completed".to_string(),
            revised_prompt: Some("a tidy diagram".to_string()),
            result: "Zm9v".to_string(),
        }];

        rewrite_image_generation_calls_for_input(&mut input);

        assert_eq!(input.len(), 1);
        assert!(matches!(
            &input[0],
            ResponseItem::Message { role, content, .. }
                if role == "assistant"
                    && matches!(
                        content.first(),
                        Some(ContentItem::OutputText { text })
                            if text.contains("image generation result omitted")
                                && text.contains("status=completed")
                                && text.contains("4 bytes")
                                && text.contains("a tidy diagram")
                                && !text.contains("Zm9v")
                    )
        ));
    }

    #[test]
    fn old_image_generation_results_are_not_replayed_as_full_images() {
        let mut input = vec![
            ResponseItem::ImageGenerationCall {
                id: "ig_old".to_string(),
                status: "completed".to_string(),
                revised_prompt: None,
                result: "A".repeat(64 * 1024),
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "continue".to_string(),
                }],
                end_turn: None,
                phase: None,
            },
        ];

        rewrite_image_generation_calls_for_input(&mut input);

        assert!(matches!(
            &input[0],
            ResponseItem::Message { content, .. }
                if matches!(
                    content.first(),
                    Some(ContentItem::OutputText { text })
                        if text.contains("image generation result omitted")
                            && text.contains("65536 bytes")
                            && !text.contains("data:image")
                            && !text.contains(&"A".repeat(1024))
                )
        ));
    }

    struct InstructionsTestCase {
        pub slug: &'static str,
        pub expects_apply_patch_instructions: bool,
    }
    #[test]
    fn get_full_instructions_no_user_content() {
        let prompt = Prompt {
            ..Default::default()
        };
        let test_cases = vec![
            InstructionsTestCase {
                slug: "gpt-3.5",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-4.1",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-4o",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-5.1",
                expects_apply_patch_instructions: false,
            },
            InstructionsTestCase {
                slug: "codex-mini-latest",
                expects_apply_patch_instructions: true,
            },
            InstructionsTestCase {
                slug: "gpt-oss:120b",
                expects_apply_patch_instructions: false,
            },
            InstructionsTestCase {
                slug: "gpt-5.1-codex",
                expects_apply_patch_instructions: false,
            },
        ];
        for test_case in test_cases {
            let model_family = find_family_for_model(test_case.slug).expect("known model slug");
            let full = prompt.get_full_instructions(&model_family);
            assert_eq!(full, model_family.base_instructions);
            if test_case.expects_apply_patch_instructions {
                assert!(
                    full.contains(APPLY_PATCH_TOOL_INSTRUCTIONS),
                    "expected apply_patch instructions for {}",
                    test_case.slug
                );
            } else {
                assert!(
                    !full.contains(APPLY_PATCH_TOOL_INSTRUCTIONS),
                    "did not expect apply_patch instructions for {}",
                    test_case.slug
                );
            }
        }
    }

    #[test]
    fn volatile_context_follows_history_and_precedes_current_turn() {
        use std::path::PathBuf;

        let mut prompt = Prompt::default();
        prompt.input.push(message("user", "stable history"));
        prompt.input.push(message("user", "current turn"));
        prompt.volatile_context_insert_at = Some(1);
        prompt.environment_context = Some(EnvironmentContext::new(
            Some(PathBuf::from("/workspace")),
            None,
            None,
            None,
        ));
        let coordinator_text = "Coordinator guidance";
        prompt
            .prepend_developer_messages
            .push(coordinator_text.to_string());

        let formatted = prompt.get_formatted_input();
        let rendered = formatted
            .iter()
            .map(|item| match item {
                ResponseItem::Message { role, content, .. } => {
                    let text = content
                        .iter()
                        .filter_map(|content| match content {
                            ContentItem::InputText { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("{role}:{text}")
                }
                _ => format!("{item:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n---\n");

        let history_pos = rendered.find("stable history").expect("history");
        let env_pos = rendered.find("<environment_context>").expect("environment");
        let coordinator_pos = rendered.find(coordinator_text).expect("developer context");
        let current_pos = rendered.find("current turn").expect("current turn");

        assert!(history_pos < env_pos);
        assert!(env_pos < coordinator_pos);
        assert!(coordinator_pos < current_pos);
    }

    #[test]
    fn duplicate_tool_outputs_are_filtered_across_prompt_split() {
        let prompt = Prompt {
            input: vec![ResponseItem::CustomToolCallOutput {
                call_id: "call-1".to_string(),
                name: None,
                output: code_protocol::models::FunctionCallOutputPayload::from_text(
                    "first".to_string(),
                ),
            }],
            volatile_context_insert_at: Some(1),
            status_items: vec![ResponseItem::CustomToolCallOutput {
                call_id: "call-status".to_string(),
                name: None,
                output: code_protocol::models::FunctionCallOutputPayload::from_text(
                    "status output is not part of split dedupe".to_string(),
                ),
            }],
            ..Prompt::default()
        };
        let mut prompt_with_duplicate = prompt.clone();
        prompt_with_duplicate.input.push(ResponseItem::CustomToolCallOutput {
            call_id: "call-1".to_string(),
            name: None,
            output: code_protocol::models::FunctionCallOutputPayload::from_text(
                "duplicate".to_string(),
            ),
        });

        let formatted = prompt_with_duplicate.get_formatted_input();
        let matching_outputs = formatted
            .iter()
            .filter(|item| {
                matches!(item, ResponseItem::CustomToolCallOutput { call_id, .. } if call_id == "call-1")
            })
            .count();

        assert_eq!(matching_outputs, 1);
    }

    #[test]
    fn auto_review_ledger_developer_message_has_distinct_context_source() {
        let mut prompt = Prompt::default();
        prompt.prepend_developer_messages.push(
            "<auto_review_ledger schema_version=\"1\">\nrun id=abc status=Reviewing\n</auto_review_ledger>"
                .to_string(),
        );

        let (_input, ledger) = prompt.get_formatted_input_with_ledger();
        let entry = ledger
            .entries()
            .iter()
            .find(|entry| entry.source == ContextSourceKind::AutoReviewLedger)
            .expect("auto review ledger entry");
        assert_eq!(entry.label, "prepended developer message");
        assert_eq!(entry.persistence, ContextPersistence::RequestOnly);
        assert_eq!(entry.duplicate_key.as_deref(), Some("developer:prepend:auto_review_ledger"));
    }

    #[test]
    fn serializes_text_verbosity_when_set() {
        let input: Vec<ResponseItem> = vec![];
        let tools: Vec<serde_json::Value> = vec![];
        let req = ResponsesApiRequest {
            model: "gpt-5.1",
            instructions: "i",
            input: &input,
            tools: &tools,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            service_tier: None,
            prompt_cache_key: None,
            text: Some(Text { verbosity: OpenAiTextVerbosity::Low, format: None }),
        };

        let v = serde_json::to_value(&req).expect("json");
        assert_eq!(
            v.get("text")
                .and_then(|t| t.get("verbosity"))
                .and_then(|s| s.as_str()),
            Some("low")
        );
    }

    #[test]
    fn serializes_text_schema_with_strict_format() {
        let input: Vec<ResponseItem> = vec![];
        let tools: Vec<serde_json::Value> = vec![];
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "answer": {"type": "string"}
            },
            "required": ["answer"],
        });
        let req = ResponsesApiRequest {
            model: "gpt-5.1",
            instructions: "i",
            input: &input,
            tools: &tools,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            service_tier: None,
            prompt_cache_key: None,
            text: Some(Text {
                verbosity: OpenAiTextVerbosity::Medium,
                format: Some(TextFormat {
                    r#type: "json_schema".to_string(),
                    name: Some("code_output_schema".to_string()),
                    strict: Some(true),
                    schema: Some(schema.clone()),
                }),
            }),
        };

        let v = serde_json::to_value(&req).expect("json");
        let text = v.get("text").expect("text field");
        assert_eq!(
            text.get("verbosity").and_then(|v| v.as_str()),
            Some("medium")
        );
        let format = text.get("format").expect("format field");

        assert_eq!(
            format.get("name"),
            Some(&serde_json::Value::String("code_output_schema".into()))
        );
        assert_eq!(
            format.get("type"),
            Some(&serde_json::Value::String("json_schema".into()))
        );
        assert_eq!(format.get("strict"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(format.get("schema"), Some(&schema));
    }

    #[test]
    fn omits_text_when_not_set() {
        let input: Vec<ResponseItem> = vec![];
        let tools: Vec<serde_json::Value> = vec![];
        let req = ResponsesApiRequest {
            model: "gpt-5.1",
            instructions: "i",
            input: &input,
            tools: &tools,
            tool_choice: "auto",
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: vec![],
            service_tier: None,
            prompt_cache_key: None,
            text: None,
        };

        let v = serde_json::to_value(&req).expect("json");
        assert!(v.get("text").is_none());
    }
}
