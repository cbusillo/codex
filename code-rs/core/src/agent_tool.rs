use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde::de;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;
use std::fs::{self, OpenOptions};
use std::io::Write as IoWrite;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration as TokioDuration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration as StdDuration, Instant};
use crate::spawn::spawn_tokio_command_with_retry;
use crate::protocol::AgentSourceKind;
use tracing::{debug, info, warn};

#[cfg(target_os = "windows")]
fn default_pathext_or_default() -> Vec<String> {
    std::env::var("PATHEXT")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        // Keep a sane default set even if PATHEXT is missing or empty. Include
        // .ps1 because PowerShell users can invoke scripts without specifying
        // the extension; CreateProcess still resolves fine when we provide the
        // full path with extension.
        .unwrap_or_else(|| vec![
            ".com".into(),
            ".exe".into(),
            ".bat".into(),
            ".cmd".into(),
            ".ps1".into(),
        ])
}

#[cfg(target_os = "windows")]
fn resolve_in_path(command: &str) -> Option<std::path::PathBuf> {
    use std::path::Path;

    let cmd_path = Path::new(command);

    // Absolute or contains separators: respect it directly if it points to a file.
    if cmd_path.is_absolute() || command.contains(['\\', '/']) {
        if cmd_path.is_file() {
            return Some(cmd_path.to_path_buf());
        }
    }

    // Search PATH with PATHEXT semantics and return the first hit.
    let exts = default_pathext_or_default();
    let Some(path_os) = std::env::var_os("PATH") else { return None; };
    let has_ext = cmd_path.extension().is_some();
    for dir in std::env::split_paths(&path_os) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if has_ext {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
        } else {
            for ext in &exts {
                let candidate = dir.join(format!("{command}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(target_os = "windows")]
fn resolve_existing_path_with_pathext(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if is_executable_file(path) {
        return Some(path.to_path_buf());
    }

    if path.extension().is_some() {
        return None;
    }

    let Some(file_name) = path.file_name() else {
        return None;
    };

    let base = file_name.to_os_string();
    for ext in default_pathext_or_default() {
        let mut name = base.clone();
        name.push(ext);
        let candidate = path.with_file_name(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    None
}

#[cfg(not(target_os = "windows"))]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };

    meta.is_file() && (meta.permissions().mode() & 0o111 != 0)
}

#[cfg(target_os = "windows")]
fn resolve_explicit_command_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    resolve_existing_path_with_pathext(path)
}

#[cfg(not(target_os = "windows"))]
fn resolve_explicit_command_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    is_executable_file(path).then_some(path.to_path_buf())
}

#[cfg(target_os = "windows")]
fn resolve_command_on_path(command: &str) -> Option<std::path::PathBuf> {
    resolve_in_path(command)
}

#[cfg(not(target_os = "windows"))]
fn resolve_command_on_path(command: &str) -> Option<std::path::PathBuf> {
    let path_os = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_os) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(command);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn home_fallback_command_candidates(command: &str) -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();

    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if let Some(home) = home.as_ref() {
        candidates.push(home.join(".local/bin").join(command));
        candidates.push(home.join(".n/bin").join(command));
        candidates.push(home.join(".npm-global/bin").join(command));
    }

    if command.eq_ignore_ascii_case("claude") {
        if let Some(claude_config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
            candidates.push(std::path::PathBuf::from(claude_config_dir).join("local").join(command));
        }
        if let Some(home) = home {
            candidates.push(home.join(".claude/local").join(command));
        }
    }

    candidates
}

pub(crate) fn resolve_external_agent_command_path(command: &str) -> Option<std::path::PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains(std::path::MAIN_SEPARATOR) || trimmed.contains('/') || trimmed.contains('\\') {
        let path = std::path::PathBuf::from(trimmed);
        return resolve_explicit_command_path(&path);
    }

    if let Some(path) = resolve_command_on_path(trimmed) {
        return Some(path);
    }

    for candidate in home_fallback_command_candidates(trimmed) {
        if let Some(path) = resolve_explicit_command_path(&candidate) {
            return Some(path);
        }
    }

    None
}

pub fn external_agent_command_exists(command: &str) -> bool {
    resolve_external_agent_command_path(command).is_some()
}

use crate::agent_defaults::{FABLE_AGENT_GUIDANCE, agent_model_spec, default_params_for};
use shlex::split as shlex_split;
use crate::config_types::AgentConfig;
use crate::openai_tools::JsonSchema;
use crate::openai_tools::OpenAiTool;
use crate::openai_tools::ResponsesApiTool;
use crate::protocol::AgentInfo;

fn current_code_binary_path() -> Result<std::path::PathBuf, String> {
    if let Ok(path) = std::env::var("CODE_BINARY_PATH") {
        let p = std::path::PathBuf::from(path);
        if !p.exists() {
            return Err(format!(
                "CODE_BINARY_PATH points to '{}' but that file is missing. Rebuild with ./build-fast.sh or update CODE_BINARY_PATH.",
                p.display()
            ));
        }
        return Ok(p);
    }
    let exe = std::env::current_exe().map_err(|e| format!("Failed to resolve current executable: {}", e))?;

    // If the kernel reports the path as "(deleted)", strip the suffix and prefer the live file
    // at the same location (common when a rebuild replaces the inode under a long-running process).
    let cleaned = strip_deleted_suffix(&exe);
    if cleaned.exists() {
        return Ok(cleaned);
    }

    if let Some(fallback) = fallback_code_binary_path() {
        return Ok(fallback);
    }

    Err(format!(
        "Current code binary is missing on disk ({}). It may have been deleted while running. Rebuild with ./build-fast.sh or reinstall 'code' to continue.",
        exe.display()
    ))
}

fn strip_deleted_suffix(path: &std::path::Path) -> std::path::PathBuf {
    const DELETED_SUFFIX: &str = " (deleted)";
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_suffix(DELETED_SUFFIX) {
        return std::path::PathBuf::from(stripped);
    }
    path.to_path_buf()
}

fn fallback_code_binary_path() -> Option<std::path::PathBuf> {
    // If the running binary was pruned (e.g., shared target cache rotation), try to locate
    // a fresh dev build in the repository, and if missing, trigger a quick rebuild.
    let repo_root = find_repo_root(std::env::current_dir().ok()?)?;
    let workspace = repo_root.join("code-rs");

    // Probe likely build outputs in priority order.
    let mut candidates = vec![
        workspace.join("target/dev-fast/code"),
        workspace.join("target/debug/code"),
        workspace.join("target/release-prod/code"),
        workspace.join("target/release/code"),
        workspace.join("bin/code"),
    ];

    if let Some(found) = candidates.iter().find(|p| p.exists()).cloned() {
        return Some(found);
    }

    // Best-effort rebuild; swallow errors so caller can surface the original message.
    let status = std::process::Command::new("bash")
        .current_dir(&repo_root)
        .args(["-lc", "./build-fast.sh >/dev/null 2>&1"])
        .status()
        .ok();

    if status.map(|s| s.success()).unwrap_or(false) {
        candidates.retain(|p| p.exists());
        if let Some(found) = candidates.first().cloned() {
            return Some(found);
        }
    }

    None
}

fn find_repo_root(start: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let mut dir = Some(start.as_path());
    while let Some(path) = dir {
        if path.join(".git").exists() {
            return Some(path.to_path_buf());
        }
        dir = path.parent();
    }
    None
}

/// Format a helpful error message when an agent command is not found.
/// Provides platform-specific guidance for resolving PATH issues.
fn format_agent_not_found_error(agent_name: &str, command: &str) -> String {
    let mut msg = format!("Agent '{}' could not be found.", agent_name);

    #[cfg(target_os = "windows")]
    {
        msg.push_str(&format!(
            "\n\nTroubleshooting steps:\n\
            1. Check if '{}' is installed and available in your PATH\n\
            2. Try using an absolute path in your config.toml:\n\
               [[agents]]\n\
               name = \"{}\"\n\
               command = \"C:\\\\Users\\\\YourUser\\\\AppData\\\\Roaming\\\\npm\\\\{}.cmd\"\n\
            3. Verify your PATH includes the directory containing '{}'\n\
            4. On Windows, ensure the file has a valid extension (.exe, .cmd, .bat, .com)\n\n\
            For more information, see: https://github.com/just-every/code/blob/main/code-rs/config.md",
            command, agent_name, command, command
        ));
    }

    #[cfg(not(target_os = "windows"))]
    {
        msg.push_str(&format!(
            "\n\nTroubleshooting steps:\n\
            1. Check if '{}' is installed: which {}\n\
            2. Verify '{}' is in your PATH: echo $PATH\n\
            3. Try using an absolute path in your config.toml:\n\
               [[agents]]\n\
               name = \"{}\"\n\
               command = \"/absolute/path/to/{}\"\n\n\
            For more information, see: https://github.com/just-every/code/blob/main/code-rs/config.md",
            command, command, command, agent_name, command
        ));
    }

    msg
}

// Agent status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// Agent information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    #[serde(default)]
    pub owner_session_id: Option<Uuid>,
    pub batch_id: Option<String>,
    pub model: String,
    #[serde(default)]
    pub name: Option<String>,
    pub prompt: String,
    pub context: Option<String>,
    pub output_goal: Option<String>,
    pub files: Vec<String>,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub context_budget_tokens: Option<u64>,
    pub read_only: bool,
    pub status: AgentStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u64>,
    #[serde(default)]
    pub retry: AgentRetryMetadata,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub progress: Vec<String>,
    pub worktree_path: Option<String>,
    pub branch_name: Option<String>,
    #[serde(default)]
    pub worktree_base: Option<String>,
    #[serde(default)]
    pub workspace_root: Option<PathBuf>,
    #[serde(default)]
    pub source_kind: Option<AgentSourceKind>,
    #[serde(skip)]
    pub log_tag: Option<String>,
    #[serde(skip)]
    #[allow(dead_code)]
    pub config: Option<AgentConfig>,
    pub reasoning_effort: code_protocol::config_types::ReasoningEffort,
    #[serde(skip)]
    pub last_activity: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRetryMetadata {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub original_model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub final_model: String,
    pub retry_count: u32,
    pub max_retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_retryable_error: Option<String>,
}

impl Default for AgentRetryMetadata {
    fn default() -> Self {
        Self {
            original_model: String::new(),
            final_model: String::new(),
            retry_count: 0,
            max_retries: DEFAULT_AGENT_PROVIDER_MAX_RETRIES as u32,
            last_retryable_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentExecutionOutput {
    output: String,
    token_count: Option<u64>,
}

impl AgentExecutionOutput {
    fn new(output: String, token_count: Option<u64>) -> Self {
        Self { output, token_count }
    }

    fn from_child_output(output: String, stderr: &str) -> Self {
        let token_count = extract_agent_token_count(&output).or_else(|| extract_agent_token_count(stderr));
        Self::new(output, token_count)
    }
}

// Global agent manager
lazy_static::lazy_static! {
    pub static ref AGENT_MANAGER: Arc<RwLock<AgentManager>> = Arc::new(RwLock::new(AgentManager::new()));
}

pub struct AgentManager {
    agents: HashMap<String, Agent>,
    // Session-scoped archive so pruned terminal agents remain queryable
    // (including worktree/branch metadata) for the full Auto Drive run.
    archived_terminal_agents: HashMap<String, Agent>,
    handles: HashMap<String, JoinHandle<()>>,
    event_senders: Vec<AgentStatusSender>,
    debug_log_root: Option<PathBuf>,
    watchdog_handle: Option<JoinHandle<()>>,
    inactivity_timeout: Duration,
    diagnostics: AgentManagerDiagnostics,
}

#[derive(Debug, Clone, Default)]
struct AgentManagerDiagnostics {
    terminal_compactions: u64,
    progress_entries_trimmed: u64,
    progress_lines_truncated: u64,
    payloads_truncated: u64,
    terminal_agents_pruned: u64,
    archived_terminal_agents: u64,
    status_terminal_agents_omitted: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct AgentCompactionDelta {
    progress_entries_trimmed: usize,
    progress_lines_truncated: usize,
    payloads_truncated: usize,
}

impl AgentCompactionDelta {
    fn any(self) -> bool {
        self.progress_entries_trimmed > 0
            || self.progress_lines_truncated > 0
            || self.payloads_truncated > 0
    }
}

const MAX_AGENT_PROGRESS_ENTRIES: usize = 96;
const MAX_AGENT_PROGRESS_LINE_BYTES: usize = 2048;
const MAX_AGENT_RESULT_BYTES: usize = 64 * 1024;
const MAX_TRACKED_TERMINAL_AGENTS: usize = 512;
const DEFAULT_CONTEXT_FILE_BUDGET_TOKENS: u64 = 16_000;
const MAX_CONTEXT_FILE_BUDGET_TOKENS: u64 = 900_000;
const CONTEXT_FILE_TOKEN_BYTES_ESTIMATE: u64 = 4;
const AGENT_PROMPT_STDIN_THRESHOLD_BYTES: usize = 32 * 1024;
const AGENT_PROMPT_ARGV_THRESHOLD_BYTES: usize = {
    #[cfg(target_os = "windows")]
    {
        8 * 1024
    }
    #[cfg(not(target_os = "windows"))]
    {
        AGENT_PROMPT_STDIN_THRESHOLD_BYTES
    }
};

const CONTEXT_FILE_BUDGET_GUIDANCE: &str = "context_budget_tokens must be a non-negative integer token budget. context_files inline file contents into the agent prompt; set context_budget_tokens explicitly for intentional large-context launches, or use files for lightweight path hints. For strict one-shot rollout/model evaluation, prefer `code llm request --message-file`.";
const MAX_STATUS_TERMINAL_AGENTS: usize = 128;
const DEFAULT_AGENT_PROVIDER_MAX_RETRIES: usize = 2;
const AGENT_PROVIDER_RETRY_BASE_DELAY: StdDuration = StdDuration::from_secs(2);
const AGENT_PROVIDER_RETRY_MAX_DELAY: StdDuration = StdDuration::from_secs(10);
pub(crate) const CODE_AGENT_SPAWN_DEPTH_ENV: &str = "CODE_AGENT_SPAWN_DEPTH";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentProviderFailureClass {
    Retryable,
    NonRetryable,
}

fn classify_agent_provider_failure(error: &str) -> AgentProviderFailureClass {
    let lower = error.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));

    if contains_any(&[
        "unauthorized",
        "authentication",
        "auth failed",
        "invalid api key",
        "invalid token",
        "permission denied",
        "forbidden",
        "not found",
        "could not be found",
        "not installed",
        "no such file",
        "invalid config",
        "misconfigured",
        "cancelled",
        "canceled",
        "interrupted",
        "policy",
    ]) {
        return AgentProviderFailureClass::NonRetryable;
    }

    if contains_any(&[
        "overloaded",
        "rate limit",
        "rate_limit",
        "too many requests",
        "temporarily unavailable",
        "temporary failure",
        "transient",
        "timeout",
        "timed out",
        "deadline exceeded",
        "service unavailable",
        "internal server error",
        "bad gateway",
        "gateway timeout",
        "connection reset",
        "connection refused",
        "connection closed",
        "connection aborted",
        "broken pipe",
        "transport error",
        "stream disconnected",
        "network error",
        "http 408",
        "http 409",
        "http 429",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
        "status 408",
        "status 409",
        "status 429",
        "status 500",
        "status 502",
        "status 503",
        "status 504",
        "api error: 408",
        "api error: 409",
        "api error: 429",
        "api error: 500",
        "api error: 502",
        "api error: 503",
        "api error: 504",
    ]) {
        return AgentProviderFailureClass::Retryable;
    }

    AgentProviderFailureClass::NonRetryable
}

fn agent_retry_delay(attempt_index: usize) -> StdDuration {
    let multiplier = 1_u32 << attempt_index.min(8);
    AGENT_PROVIDER_RETRY_BASE_DELAY
        .saturating_mul(multiplier)
        .min(AGENT_PROVIDER_RETRY_MAX_DELAY)
}

pub(crate) fn current_agent_spawn_depth() -> i32 {
    std::env::var(CODE_AGENT_SPAWN_DEPTH_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|depth| *depth >= 0)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct AgentStatusUpdatePayload {
    pub agents: Vec<AgentInfo>,
    pub context: Option<String>,
    pub task: Option<String>,
}

struct AgentStatusSender {
    owner_session_id: Uuid,
    sender: mpsc::UnboundedSender<AgentStatusUpdatePayload>,
}

fn agent_belongs_to_session(agent: &Agent, owner_session_id: Option<Uuid>) -> bool {
    match owner_session_id {
        Some(owner_session_id) => agent
            .owner_session_id
            .is_none_or(|agent_owner_session_id| agent_owner_session_id == owner_session_id),
        None => true,
    }
}

fn agent_can_be_cancelled_by_session(agent: &Agent, owner_session_id: Uuid) -> bool {
    agent.owner_session_id
        .is_none_or(|agent_owner_session_id| agent_owner_session_id == owner_session_id)
}

fn agent_info_for_status(agent: &Agent, now: DateTime<Utc>) -> AgentInfo {
    // Just show the model name - status provides the useful info.
    let name = agent
        .name
        .as_ref()
        .map(|value| value.clone())
        .unwrap_or_else(|| agent.model.clone());
    let start = agent.started_at.unwrap_or(agent.created_at);
    let end = agent.completed_at.unwrap_or(now);
    let elapsed_ms = match end.signed_duration_since(start).num_milliseconds() {
        value if value >= 0 => Some(value as u64),
        _ => None,
    };

    AgentInfo {
        id: agent.id.clone(),
        name,
        status: format!("{:?}", agent.status).to_lowercase(),
        batch_id: agent.batch_id.clone(),
        model: Some(agent.model.clone()),
        last_progress: agent.progress.last().cloned(),
        result: agent.result.clone(),
        error: agent.error.clone(),
        elapsed_ms,
        token_count: agent.token_count,
        last_activity_at: match agent.status {
            AgentStatus::Pending | AgentStatus::Running => Some(agent.last_activity.to_rfc3339()),
            _ => None,
        },
        seconds_since_last_activity: match agent.status {
            AgentStatus::Pending | AgentStatus::Running => Some(
                now.signed_duration_since(agent.last_activity)
                    .num_seconds()
                    .max(0) as u64,
            ),
            _ => None,
        },
        source_kind: agent.source_kind.clone(),
        owner_session_id: agent.owner_session_id.map(|id| id.to_string()),
        worktree_base: agent.worktree_base.clone(),
    }
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
            archived_terminal_agents: HashMap::new(),
            handles: HashMap::new(),
            event_senders: Vec::new(),
            debug_log_root: None,
            watchdog_handle: None,
            inactivity_timeout: Duration::minutes(30),
            diagnostics: AgentManagerDiagnostics::default(),
        }
    }

    pub fn set_event_sender(
        &mut self,
        owner_session_id: Uuid,
        sender: mpsc::UnboundedSender<AgentStatusUpdatePayload>,
    ) {
        self.event_senders
            .retain(|registered| !registered.sender.is_closed());
        let fresh_sender_set = self.event_senders.is_empty();
        self.event_senders.push(AgentStatusSender {
            owner_session_id,
            sender,
        });
        // Keep archived results for connected sessions and legacy unowned
        // agents, but drop archives from older disconnected sessions so a
        // long-lived manager does not retain stale terminal agents forever.
        let connected_sessions: HashSet<Uuid> = self
            .event_senders
            .iter()
            .filter(|registered| !registered.sender.is_closed())
            .map(|registered| registered.owner_session_id)
            .collect();
        self.archived_terminal_agents.retain(|_, agent| {
            agent
                .owner_session_id
                .is_none_or(|agent_owner_session_id| connected_sessions.contains(&agent_owner_session_id))
        });
        self.diagnostics.archived_terminal_agents = self.archived_terminal_agents.len() as u64;

        if fresh_sender_set {
            self.diagnostics = AgentManagerDiagnostics::default();
            self.diagnostics.archived_terminal_agents = self.archived_terminal_agents.len() as u64;
        }
        self.start_watchdog();
    }

    fn start_watchdog(&mut self) {
        if self.watchdog_handle.is_some() {
            return;
        }

        let timeout = self.inactivity_timeout;
        let manager = Arc::downgrade(&AGENT_MANAGER);
        self.watchdog_handle = Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TokioDuration::from_secs(60));
            loop {
                ticker.tick().await;

                let Some(manager_arc) = manager.upgrade() else { break; };

                let mut mgr = manager_arc.write().await;
                let now = Utc::now();
                let timeout_ids: Vec<String> = mgr
                    .agents
                    .iter()
                    .filter(|(_, agent)| matches!(agent.status, AgentStatus::Pending | AgentStatus::Running))
                    .filter(|(_, agent)| now - agent.last_activity > timeout)
                    .map(|(id, _)| id.clone())
                    .collect();

                if timeout_ids.is_empty() {
                    continue;
                }

                for agent_id in timeout_ids.iter() {
                    if let Some(handle) = mgr.handles.remove(agent_id) {
                        handle.abort();
                    }
                    if let Some(agent) = mgr.agents.get_mut(agent_id) {
                        agent.status = AgentStatus::Failed;
                        agent.error = Some(format!(
                            "Agent timed out after {} minutes of inactivity.",
                            timeout.num_minutes()
                        ));
                        agent.completed_at = Some(now);
                        Self::record_activity(agent);
                    }
                    mgr.finalize_terminal_agent(agent_id);
                }

                // Notify listeners once per sweep.
                mgr.send_agent_status_update().await;
            }
        }));
    }

    pub fn set_debug_log_root(&mut self, root: Option<PathBuf>) {
        self.debug_log_root = root;
    }

    async fn touch_agent(agent_id: &str) {
        if let Some(manager) = Arc::downgrade(&AGENT_MANAGER).upgrade() {
            let mut mgr = manager.write().await;
            if let Some(agent) = mgr.agents.get_mut(agent_id) {
                Self::record_activity(agent);
            }
        }
    }

    fn record_activity(agent: &mut Agent) {
        agent.last_activity = Utc::now();
    }

    fn trim_to_tail_utf8(text: &str, max_bytes: usize) -> String {
        let bytes = text.as_bytes();
        if bytes.len() <= max_bytes {
            return text.to_string();
        }

        let mut start = bytes.len().saturating_sub(max_bytes);
        while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
            start += 1;
        }

        let tail = String::from_utf8_lossy(&bytes[start..]).to_string();
        format!("…{tail}")
    }

    fn compact_terminal_agent(agent: &mut Agent) -> AgentCompactionDelta {
        let mut delta = AgentCompactionDelta::default();

        if agent.progress.len() > MAX_AGENT_PROGRESS_ENTRIES {
            let drain = agent.progress.len() - MAX_AGENT_PROGRESS_ENTRIES;
            agent.progress.drain(0..drain);
            delta.progress_entries_trimmed = drain;
        }

        for line in &mut agent.progress {
            let original_len = line.len();
            let trimmed = Self::trim_to_tail_utf8(line, MAX_AGENT_PROGRESS_LINE_BYTES);
            if trimmed.len() < original_len {
                delta.progress_lines_truncated = delta.progress_lines_truncated.saturating_add(1);
            }
            *line = trimmed;
        }

        if let Some(result) = agent.result.as_mut() {
            let original_len = result.len();
            let trimmed = Self::trim_to_tail_utf8(result, MAX_AGENT_RESULT_BYTES);
            if trimmed.len() < original_len {
                delta.payloads_truncated = delta.payloads_truncated.saturating_add(1);
            }
            *result = trimmed;
        }

        if let Some(error) = agent.error.as_mut() {
            let original_len = error.len();
            let trimmed = Self::trim_to_tail_utf8(error, MAX_AGENT_RESULT_BYTES);
            if trimmed.len() < original_len {
                delta.payloads_truncated = delta.payloads_truncated.saturating_add(1);
            }
            *error = trimmed;
        }

        // Keep branch/worktree metadata so users can still merge/inspect results,
        // but release heavy prompt payloads once an agent is terminal.
        agent.prompt.clear();
        agent.context = None;
        agent.output_goal = None;
        agent.files.clear();
        agent.context_files.clear();
        agent.context_budget_tokens = None;

        delta
    }

    fn prune_terminal_agents(&mut self) {
        let terminal_count = self
            .agents
            .values()
            .filter(|agent| {
                matches!(
                    agent.status,
                    AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .count();

        if terminal_count <= MAX_TRACKED_TERMINAL_AGENTS {
            return;
        }

        let mut terminal: Vec<(DateTime<Utc>, String)> = self
            .agents
            .iter()
            .filter(|(_, agent)| {
                matches!(
                    agent.status,
                    AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .map(|(id, agent)| {
                (
                    agent.completed_at.unwrap_or(agent.created_at),
                    id.clone(),
                )
            })
            .collect();

        terminal.sort_by_key(|(completed_at, _)| *completed_at);

        let mut to_remove = terminal_count.saturating_sub(MAX_TRACKED_TERMINAL_AGENTS);
        let mut pruned = 0usize;
        for (_, agent_id) in terminal {
            if to_remove == 0 {
                break;
            }

            self.handles.remove(&agent_id);
            if let Some(agent) = self.agents.remove(&agent_id) {
                self.archived_terminal_agents.insert(agent_id.clone(), agent);
            }
            pruned = pruned.saturating_add(1);
            to_remove = to_remove.saturating_sub(1);
        }

        if pruned > 0 {
            self.diagnostics.terminal_agents_pruned = self
                .diagnostics
                .terminal_agents_pruned
                .saturating_add(pruned as u64);
            self.diagnostics.archived_terminal_agents = self.archived_terminal_agents.len() as u64;
            info!(
                pruned,
                retained = self.agents.len(),
                archived = self.archived_terminal_agents.len(),
                "agent manager pruned terminal agents from live cache"
            );
        }
    }

    fn finalize_terminal_agent(&mut self, agent_id: &str) {
        self.handles.remove(agent_id);

        if let Some(agent) = self.agents.get_mut(agent_id) {
            let delta = Self::compact_terminal_agent(agent);
            self.diagnostics.terminal_compactions =
                self.diagnostics.terminal_compactions.saturating_add(1);
            self.diagnostics.progress_entries_trimmed = self
                .diagnostics
                .progress_entries_trimmed
                .saturating_add(delta.progress_entries_trimmed as u64);
            self.diagnostics.progress_lines_truncated = self
                .diagnostics
                .progress_lines_truncated
                .saturating_add(delta.progress_lines_truncated as u64);
            self.diagnostics.payloads_truncated = self
                .diagnostics
                .payloads_truncated
                .saturating_add(delta.payloads_truncated as u64);
            if delta.any() {
                debug!(
                    agent_id,
                    progress_entries_trimmed = delta.progress_entries_trimmed,
                    progress_lines_truncated = delta.progress_lines_truncated,
                    payloads_truncated = delta.payloads_truncated,
                    total_terminal_compactions = self.diagnostics.terminal_compactions,
                    total_progress_entries_trimmed = self.diagnostics.progress_entries_trimmed,
                    total_progress_lines_truncated = self.diagnostics.progress_lines_truncated,
                    total_payloads_truncated = self.diagnostics.payloads_truncated,
                    "compacted terminal agent state"
                );
            }
        }

        self.prune_terminal_agents();
    }

    fn visible_agents_for_status(&self, owner_session_id: Option<Uuid>) -> Vec<&Agent> {
        let mut active: Vec<&Agent> = self
            .agents
            .values()
            .filter(|agent| agent_belongs_to_session(agent, owner_session_id))
            .filter(|agent| matches!(agent.status, AgentStatus::Pending | AgentStatus::Running))
            .collect();

        active.sort_by_key(|agent| agent.created_at);

        let mut terminal: Vec<&Agent> = self
            .agents
            .values()
            .filter(|agent| agent_belongs_to_session(agent, owner_session_id))
            .filter(|agent| {
                matches!(
                    agent.status,
                    AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .collect();

        terminal.sort_by_key(|agent| agent.completed_at.unwrap_or(agent.created_at));

        if terminal.len() > MAX_STATUS_TERMINAL_AGENTS {
            let keep_from = terminal.len() - MAX_STATUS_TERMINAL_AGENTS;
            terminal = terminal.split_off(keep_from);
        }

        active.extend(terminal);
        active
    }

    pub fn status_visible_agents(&self) -> Vec<Agent> {
        self.visible_agents_for_status(None)
            .into_iter()
            .cloned()
            .collect()
    }

    pub fn status_visible_agents_for_session(&self, owner_session_id: Uuid) -> Vec<Agent> {
        self.visible_agents_for_status(Some(owner_session_id))
            .into_iter()
            .cloned()
            .collect()
    }

    fn status_payload_for_session(&mut self, owner_session_id: Uuid) -> AgentStatusUpdatePayload {
        let now = Utc::now();

        let total_terminal = self
            .agents
            .values()
            .filter(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
            .filter(|agent| {
                matches!(
                    agent.status,
                    AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
                )
            })
            .count();
        let omitted_terminal = total_terminal.saturating_sub(MAX_STATUS_TERMINAL_AGENTS);
        if omitted_terminal > 0 {
            self.diagnostics.status_terminal_agents_omitted = self
                .diagnostics
                .status_terminal_agents_omitted
                .saturating_add(omitted_terminal as u64);
            debug!(
                omitted_terminal,
                total_terminal,
                owner_session_id = %owner_session_id,
                running_agents = self
                    .agents
                    .values()
                    .filter(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
                    .filter(|agent| {
                        matches!(agent.status, AgentStatus::Pending | AgentStatus::Running)
                    })
                    .count(),
                cumulative_omitted = self.diagnostics.status_terminal_agents_omitted,
                "omitting terminal agents from status payload to keep UI responsive"
            );
        }

        let agents: Vec<AgentInfo> = self
            .visible_agents_for_status(Some(owner_session_id))
            .into_iter()
            .map(|agent| agent_info_for_status(agent, now))
            .collect();

        // Prefer active agents for shared context/task; terminal agents may
        // have had heavy fields compacted already.
        let source_agent = self
            .agents
            .values()
            .filter(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
            .find(|agent| matches!(agent.status, AgentStatus::Pending | AgentStatus::Running))
            .or_else(|| {
                self.agents
                    .values()
                    .find(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
            });
        let (context, task) = source_agent
            .map(|agent| {
                let context = agent.context.as_ref().and_then(|value| {
                    if value.trim().is_empty() {
                        None
                    } else {
                        Some(value.clone())
                    }
                });
                let task = if agent.prompt.trim().is_empty() {
                    None
                } else {
                    Some(agent.prompt.clone())
                };
                (context, task)
            })
            .unwrap_or((None, None));

        AgentStatusUpdatePayload { agents, context, task }
    }

    fn append_agent_log(&self, log_tag: &str, line: &str) {
        let Some(root) = &self.debug_log_root else { return; };
        let dir = root.join(log_tag);
        if let Err(err) = fs::create_dir_all(&dir) {
            warn!("failed to create agent log dir {:?}: {}", dir, err);
            return;
        }

        let file = dir.join("progress.log");
        match OpenOptions::new().create(true).append(true).open(&file) {
            Ok(mut fh) => {
                if let Err(err) = writeln!(fh, "{}", line) {
                    warn!("failed to write agent log {:?}: {}", file, err);
                }
            }
            Err(err) => warn!("failed to open agent log {:?}: {}", file, err),
        }
    }

    async fn send_agent_status_update(&mut self) {
        if !self.event_senders.is_empty() {
            let sessions: Vec<(usize, Uuid)> = self
                .event_senders
                .iter()
                .enumerate()
                .filter(|(_, registered)| !registered.sender.is_closed())
                .map(|(idx, registered)| (idx, registered.owner_session_id))
                .collect();
            let sender_payloads: Vec<(usize, AgentStatusUpdatePayload)> = sessions
                .into_iter()
                .map(|(idx, owner_session_id)| (idx, self.status_payload_for_session(owner_session_id)))
                .collect();

            for (idx, payload) in sender_payloads.into_iter().rev() {
                if self.event_senders[idx].sender.send(payload).is_err() {
                    self.event_senders.remove(idx);
                }
            }
            self.event_senders
                .retain(|registered| !registered.sender.is_closed());
        }
    }

    pub async fn create_agent(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        owner_session_id: Uuid,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        let workspace_root = std::env::current_dir().ok();
        self.create_agent_in_workspace(
            model,
            name,
            prompt,
            context,
            output_goal,
            files,
            context_files,
            context_budget_tokens,
            read_only,
            batch_id,
            owner_session_id,
            workspace_root,
            reasoning_effort,
        )
        .await
    }

    pub async fn create_agent_in_workspace(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        owner_session_id: Uuid,
        workspace_root: Option<PathBuf>,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        self.create_agent_internal(
            model,
            name,
            prompt,
            context,
            output_goal,
            files,
            context_files,
            context_budget_tokens,
            read_only,
            batch_id,
            None,
            owner_session_id,
            None,
            None,
            None,
            workspace_root,
            reasoning_effort,
        )
        .await
    }

    pub async fn create_agent_with_config(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        config: AgentConfig,
        owner_session_id: Uuid,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        let workspace_root = std::env::current_dir().ok();
        self.create_agent_with_config_in_workspace(
            model,
            name,
            prompt,
            context,
            output_goal,
            files,
            context_files,
            context_budget_tokens,
            read_only,
            batch_id,
            config,
            owner_session_id,
            workspace_root,
            reasoning_effort,
        )
        .await
    }

    pub async fn create_agent_with_config_in_workspace(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        config: AgentConfig,
        owner_session_id: Uuid,
        workspace_root: Option<PathBuf>,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        self.create_agent_internal(
            model,
            name,
            prompt,
            context,
            output_goal,
            files,
            context_files,
            context_budget_tokens,
            read_only,
            batch_id,
            Some(config),
            owner_session_id,
            None,
            None,
            None,
            workspace_root,
            reasoning_effort,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn create_agent_with_options(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        config: Option<AgentConfig>,
        owner_session_id: Uuid,
        worktree_branch: Option<String>,
        worktree_base: Option<String>,
        source_kind: Option<AgentSourceKind>,
        workspace_root: Option<PathBuf>,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        self
            .create_agent_internal(
                model,
                name,
                prompt,
                context,
                output_goal,
                files,
                context_files,
                context_budget_tokens,
                read_only,
                batch_id,
                config,
                owner_session_id,
                worktree_branch,
                worktree_base,
                source_kind,
                workspace_root,
                reasoning_effort,
            )
            .await
    }

    async fn create_agent_internal(
        &mut self,
        model: String,
        name: Option<String>,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        context_files: Vec<String>,
        context_budget_tokens: Option<u64>,
        read_only: bool,
        batch_id: Option<String>,
        config: Option<AgentConfig>,
        owner_session_id: Uuid,
        worktree_branch: Option<String>,
        worktree_base: Option<String>,
        source_kind: Option<AgentSourceKind>,
        workspace_root: Option<PathBuf>,
        reasoning_effort: code_protocol::config_types::ReasoningEffort,
    ) -> String {
        let agent_id = Uuid::new_v4().to_string();

        let log_tag = match source_kind {
            Some(AgentSourceKind::AutoReview) => {
                Some(format!("agents/auto-review/{}", agent_id))
            }
            _ => None,
        };

        let retry = AgentRetryMetadata {
            original_model: model.clone(),
            final_model: model.clone(),
            ..AgentRetryMetadata::default()
        };

        let agent = Agent {
            id: agent_id.clone(),
            owner_session_id: Some(owner_session_id),
            batch_id,
            model,
            name: normalize_agent_name(name),
            prompt,
            context,
            output_goal,
            files,
            context_files,
            context_budget_tokens,
            read_only,
            status: AgentStatus::Pending,
            result: None,
            error: None,
            token_count: None,
            retry,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            progress: Vec::new(),
            worktree_path: None,
            branch_name: worktree_branch,
            worktree_base,
            workspace_root,
            source_kind,
            log_tag,
            config: config.clone(),
            reasoning_effort,
            last_activity: Utc::now(),
        };

        self.agents.insert(agent_id.clone(), agent.clone());

        // Send initial status update
        self.send_agent_status_update().await;

        // Spawn async agent
        let agent_id_clone = agent_id.clone();
        let handle = tokio::spawn(async move {
            execute_agent(agent_id_clone, config).await;
        });

        self.handles.insert(agent_id.clone(), handle);

        agent_id
    }

    pub fn get_agent(&self, agent_id: &str) -> Option<Agent> {
        self
            .agents
            .get(agent_id)
            .cloned()
            .or_else(|| self.archived_terminal_agents.get(agent_id).cloned())
    }

    pub fn get_agent_for_session(&self, agent_id: &str, owner_session_id: Uuid) -> Option<Agent> {
        self
            .get_agent(agent_id)
            .filter(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
    }

    pub fn get_all_agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    pub fn list_agents(
        &self,
        status_filter: Option<AgentStatus>,
        batch_id: Option<String>,
        recent_only: bool,
    ) -> Vec<Agent> {
        let cutoff = if recent_only {
            Some(Utc::now() - Duration::hours(2))
        } else {
            None
        };

        let mut out = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        let mut collect_filtered = |agent: &Agent| {
            if let Some(ref filter) = status_filter {
                if agent.status != *filter {
                    return;
                }
            }
            if let Some(ref batch) = batch_id {
                if agent.batch_id.as_ref() != Some(batch) {
                    return;
                }
            }
            if let Some(cutoff) = cutoff {
                if agent.created_at < cutoff {
                    return;
                }
            }
            if seen_ids.insert(agent.id.clone()) {
                out.push(agent.clone());
            }
        };

        for agent in self.agents.values() {
            collect_filtered(agent);
        }
        for agent in self.archived_terminal_agents.values() {
            collect_filtered(agent);
        }

        out
    }

    pub fn list_agents_for_session(
        &self,
        status_filter: Option<AgentStatus>,
        batch_id: Option<String>,
        recent_only: bool,
        owner_session_id: Uuid,
    ) -> Vec<Agent> {
        self
            .list_agents(status_filter, batch_id, recent_only)
            .into_iter()
            .filter(|agent| agent_belongs_to_session(agent, Some(owner_session_id)))
            .collect()
    }

    pub fn has_active_agents(&self) -> bool {
        self.agents
            .values()
            .any(|agent| matches!(agent.status, AgentStatus::Pending | AgentStatus::Running))
    }

    pub async fn cancel_agent(&mut self, agent_id: &str) -> bool {
        if let Some(handle) = self.handles.remove(agent_id) {
            handle.abort();
            if let Some(agent) = self.agents.get_mut(agent_id) {
                agent.status = AgentStatus::Cancelled;
                agent.completed_at = Some(Utc::now());
            }
            self.finalize_terminal_agent(agent_id);
            true
        } else if self
            .agents
            .get(agent_id)
            .is_some_and(|agent| matches!(agent.status, AgentStatus::Pending | AgentStatus::Running))
        {
            if let Some(agent) = self.agents.get_mut(agent_id) {
                agent.status = AgentStatus::Cancelled;
                agent.completed_at = Some(Utc::now());
                Self::record_activity(agent);
            }
            self.finalize_terminal_agent(agent_id);
            true
        } else {
            false
        }
    }

    pub async fn cancel_agent_for_session(
        &mut self,
        agent_id: &str,
        owner_session_id: Uuid,
    ) -> bool {
        if self
            .get_agent(agent_id)
            .is_some_and(|agent| agent_can_be_cancelled_by_session(&agent, owner_session_id))
        {
            self.cancel_agent(agent_id).await
        } else {
            false
        }
    }

    pub async fn cancel_batch(&mut self, batch_id: &str) -> usize {
        let agent_ids: Vec<String> = self
            .agents
            .values()
            .filter(|agent| agent.batch_id.as_ref() == Some(&batch_id.to_string()))
            .map(|agent| agent.id.clone())
            .collect();

        let mut count = 0;
        for agent_id in agent_ids {
            if self.cancel_agent(&agent_id).await {
                count += 1;
            }
        }
        count
    }

    pub async fn cancel_batch_for_session(
        &mut self,
        batch_id: &str,
        owner_session_id: Uuid,
    ) -> usize {
        let agent_ids: Vec<String> = self
            .agents
            .values()
            .filter(|agent| agent.batch_id.as_ref() == Some(&batch_id.to_string()))
            .filter(|agent| agent_can_be_cancelled_by_session(agent, owner_session_id))
            .map(|agent| agent.id.clone())
            .collect();

        let mut count = 0;
        for agent_id in agent_ids {
            if self.cancel_agent(&agent_id).await {
                count += 1;
            }
        }
        count
    }

    pub async fn update_agent_status(&mut self, agent_id: &str, status: AgentStatus) {
        let mut terminal = false;
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.status = status;
            if agent.status == AgentStatus::Running && agent.started_at.is_none() {
                agent.started_at = Some(Utc::now());
            }
            if matches!(
                agent.status,
                AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
            ) {
                agent.completed_at = Some(Utc::now());
                terminal = true;
            }
            Self::record_activity(agent);
        }

        if terminal {
            self.finalize_terminal_agent(agent_id);
        }

        // Send status update event
        self.send_agent_status_update().await;
    }

    pub async fn update_agent_result(&mut self, agent_id: &str, result: Result<String, String>) {
        self.update_agent_result_with_token_count(agent_id, result, None)
            .await;
    }

    pub async fn update_agent_result_with_token_count(
        &mut self,
        agent_id: &str,
        result: Result<String, String>,
        token_count: Option<u64>,
    ) {
        let debug_enabled = self.debug_log_root.is_some();
        let mut updated = false;

        if let Some((log_tag, log_lines)) = self.agents.get_mut(agent_id).map(|agent| {
            let log_tag = if debug_enabled { agent.log_tag.clone() } else { None };

            let mut log_lines: Vec<String> = Vec::new();
            if debug_enabled {
                let stamp = Utc::now().format("%H:%M:%S");
                match &result {
                    Ok(output) => {
                        log_lines.push(format!("{stamp}: [result] completed"));
                        if !output.trim().is_empty() {
                            log_lines.push(output.trim_end().to_string());
                        }
                    }
                    Err(error) => {
                        log_lines.push(format!("{stamp}: [result] failed"));
                        log_lines.push(error.clone());
                    }
                }
            }

            match result {
                Ok(output) => {
                    agent.result = Some(output);
                    agent.status = AgentStatus::Completed;
                    agent.token_count = token_count.or(agent.token_count);
                }
                Err(error) => {
                    agent.error = Some(error);
                    agent.status = AgentStatus::Failed;
                    agent.token_count = token_count.or(agent.token_count);
                }
            }
            agent.completed_at = Some(Utc::now());
            Self::record_activity(agent);
            updated = true;

            (log_tag, log_lines)
        }) {
            if let Some(tag) = log_tag {
                for line in log_lines {
                    self.append_agent_log(&tag, &line);
                }
            }
            if updated {
                self.finalize_terminal_agent(agent_id);
            }
            // Send status update event
            self.send_agent_status_update().await;
        }
    }

    pub async fn update_agent_retry_metadata(
        &mut self,
        agent_id: &str,
        retry_count: u32,
        last_retryable_error: Option<String>,
    ) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.retry.retry_count = retry_count;
            agent.retry.last_retryable_error = last_retryable_error;
            Self::record_activity(agent);
            self.send_agent_status_update().await;
        }
    }

    pub async fn add_progress(&mut self, agent_id: &str, message: String) {
        let debug_enabled = self.debug_log_root.is_some();

        if let Some((log_tag, entry, line_truncated, entries_trimmed)) =
            self.agents.get_mut(agent_id).map(|agent| {
            let raw_entry = format!("{}: {}", Utc::now().format("%H:%M:%S"), message);
            let line_truncated = raw_entry.len() > MAX_AGENT_PROGRESS_LINE_BYTES;
            let entry = Self::trim_to_tail_utf8(&raw_entry, MAX_AGENT_PROGRESS_LINE_BYTES);
            let log_tag = if debug_enabled { agent.log_tag.clone() } else { None };
            agent.progress.push(entry.clone());
            let mut entries_trimmed = 0usize;
            if agent.progress.len() > MAX_AGENT_PROGRESS_ENTRIES {
                let drain = agent.progress.len() - MAX_AGENT_PROGRESS_ENTRIES;
                agent.progress.drain(0..drain);
                entries_trimmed = drain;
            }
            Self::record_activity(agent);
            (log_tag, entry, line_truncated, entries_trimmed)
        }) {
            if line_truncated {
                self.diagnostics.progress_lines_truncated = self
                    .diagnostics
                    .progress_lines_truncated
                    .saturating_add(1);
            }
            if entries_trimmed > 0 {
                self.diagnostics.progress_entries_trimmed = self
                    .diagnostics
                    .progress_entries_trimmed
                    .saturating_add(entries_trimmed as u64);
            }
            if line_truncated || entries_trimmed > 0 {
                debug!(
                    agent_id,
                    line_truncated,
                    entries_trimmed,
                    total_progress_lines_truncated = self.diagnostics.progress_lines_truncated,
                    total_progress_entries_trimmed = self.diagnostics.progress_entries_trimmed,
                    "trimmed agent progress backlog"
                );
            }
            if let Some(tag) = log_tag {
                self.append_agent_log(&tag, &entry);
            }
            // Send updated agent status with the latest progress
            self.send_agent_status_update().await;
        }
    }

    pub async fn update_worktree_info(
        &mut self,
        agent_id: &str,
        worktree_path: String,
        branch_name: String,
    ) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.worktree_path = Some(worktree_path);
            agent.branch_name = Some(branch_name);
        }
    }
}

async fn get_git_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(&["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| format!("Git not installed or not in a git repository: {}", e))?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    } else {
        Err("Not in a git repository".to_string())
    }
}

use crate::git_worktree::sanitize_ref_component;

fn generate_branch_id(model: &str, agent: &str) -> String {
    // Extract first few meaningful words from agent for the branch name
    let stop = ["the", "and", "for", "with", "from", "into", "goal"]; // skip boilerplate
    let words: Vec<&str> = agent
        .split_whitespace()
        .filter(|w| w.len() > 2 && !stop.contains(&w.to_ascii_lowercase().as_str()))
        .take(3)
        .collect();

    let raw_suffix = if words.is_empty() {
        Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("agent")
            .to_string()
    } else {
        words.join("-")
    };

    // Sanitize both model and suffix for safety
    let model_s = sanitize_ref_component(model);
    let mut suffix_s = sanitize_ref_component(&raw_suffix);

    // Constrain length to keep branch names readable
    if suffix_s.len() > 40 {
        suffix_s.truncate(40);
        suffix_s = suffix_s.trim_matches('-').to_string();
        if suffix_s.is_empty() {
            suffix_s = "agent".to_string();
        }
    }

    format!("code-{}-{}", model_s, suffix_s)
}

use crate::git_worktree::setup_worktree;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextFilesPrompt {
    block: String,
    estimated_tokens: u64,
    included_files: usize,
    budget_tokens: u64,
}

fn estimate_context_file_tokens(content: &str) -> u64 {
    let word_count = content
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .count() as u64;
    let byte_estimate = (content.len() as u64)
        .saturating_add(CONTEXT_FILE_TOKEN_BYTES_ESTIMATE - 1)
        / CONTEXT_FILE_TOKEN_BYTES_ESTIMATE;
    word_count.max(byte_estimate).max(1)
}

fn is_probably_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|byte| *byte == 0)
}

fn xml_attr_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn context_budget_byte_limit(budget: u64) -> u64 {
    budget.saturating_mul(CONTEXT_FILE_TOKEN_BYTES_ESTIMATE)
}

fn deliver_agent_prompt(
    family: &str,
    args: &mut Vec<String>,
    prompt: &str,
    supports_stdin_prompt: bool,
    force_stdin: bool,
) -> Result<Option<String>, String> {
    let use_stdin = force_stdin || prompt.len() > AGENT_PROMPT_ARGV_THRESHOLD_BYTES;
    if !use_stdin {
        args.push(prompt.to_string());
        return Ok(None);
    }

    if supports_stdin_prompt {
        args.push("-".to_string());
        Ok(Some(prompt.to_string()))
    } else {
        Err(format!(
            "agent prompt is {} bytes, above the {} byte argv delivery threshold, and provider family '{family}' does not support large-prompt stdin delivery in this configuration. Use a built-in Every Code/Codex agent such as code-gpt-5.4, reduce context_files, or lower context_budget_tokens.",
            prompt.len(),
            AGENT_PROMPT_ARGV_THRESHOLD_BYTES
        ))
    }
}

fn canonicalize_allowed_context_file(path: &str, workspace_root: &Path) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("context_files contains an empty path".to_string());
    }

    let requested = Path::new(trimmed);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };

    let canonical = candidate
        .canonicalize()
        .map_err(|err| format!("failed to resolve context file {trimmed}: {err}"))?;
    let canonical_root = workspace_root.canonicalize().map_err(|err| {
        format!(
            "failed to resolve workspace root {}: {err}",
            workspace_root.display()
        )
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!(
            "context file {} is outside the workspace root {}",
            canonical.display(),
            canonical_root.display()
        ));
    }
    if canonical.is_dir() {
        return Err(format!(
            "context file {} is a directory; list explicit files instead",
            canonical.display()
        ));
    }

    Ok(canonical)
}

fn build_context_files_prompt(
    context_files: &[String],
    context_budget_tokens: Option<u64>,
    workspace_root: &Path,
) -> Result<Option<ContextFilesPrompt>, String> {
    if context_files.is_empty() {
        return Ok(None);
    }

    let requested_budget = context_budget_tokens.unwrap_or(DEFAULT_CONTEXT_FILE_BUDGET_TOKENS);
    if requested_budget > MAX_CONTEXT_FILE_BUDGET_TOKENS {
        return Err(format!(
            "context_budget_tokens {requested_budget} exceeds the maximum {MAX_CONTEXT_FILE_BUDGET_TOKENS}"
        ));
    }
    let budget = requested_budget;
    let mut seen = HashSet::new();
    let mut entries: Vec<(String, String, u64, u64)> = Vec::new();
    let mut total_tokens = 0_u64;

    for path in context_files {
        let canonical = canonicalize_allowed_context_file(path, workspace_root)?;
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let metadata = fs::metadata(&canonical)
            .map_err(|err| format!("failed to inspect context file {}: {err}", canonical.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "context file {} is not a regular file",
                canonical.display()
            ));
        }
        let byte_len = metadata.len();
        let remaining_budget = budget.saturating_sub(total_tokens);
        let byte_limit = context_budget_byte_limit(remaining_budget);
        if byte_len > byte_limit {
            return Err(format!(
                "context file {} is {} bytes, above the remaining budget of {} estimated tokens ({} bytes max). {CONTEXT_FILE_BUDGET_GUIDANCE}",
                canonical.display(),
                byte_len,
                remaining_budget,
                byte_limit
            ));
        }
        let bytes = fs::read(&canonical)
            .map_err(|err| format!("failed to read context file {}: {err}", canonical.display()))?;
        if is_probably_binary(&bytes) {
            return Err(format!(
                "context file {} appears to be binary; context_files only supports UTF-8 text",
                canonical.display()
            ));
        }
        let content = String::from_utf8(bytes).map_err(|err| {
            format!(
                "context file {} is not valid UTF-8: {err}",
                canonical.display()
            )
        })?;
        let estimated_tokens = estimate_context_file_tokens(&content);
        total_tokens = total_tokens.saturating_add(estimated_tokens);
        if total_tokens > budget {
            return Err(format!(
                "context_files estimated {total_tokens} tokens, above budget {budget}. {CONTEXT_FILE_BUDGET_GUIDANCE}"
            ));
        }
        entries.push((
            canonical.display().to_string(),
            content,
            estimated_tokens,
            byte_len,
        ));
    }

    let mut block = String::new();
    block.push_str("Preloaded context files:\n");
    block.push_str(&format!(
        "Included {} file(s), estimated {} tokens, budget {} tokens. These files were snapshotted before the subagent launched; do not re-read them unless fresh contents are needed.\n",
        entries.len(),
        total_tokens,
        budget
    ));
    for (path, content, estimated_tokens, bytes) in &entries {
        block.push_str(&format!(
            "\n<context_file path=\"{}\" bytes=\"{}\" estimated_tokens=\"{}\">\n{}\n</context_file>\n",
            xml_attr_escape(path),
            bytes,
            estimated_tokens,
            content
        ));
    }

    Ok(Some(ContextFilesPrompt {
        block,
        estimated_tokens: total_tokens,
        included_files: entries.len(),
        budget_tokens: budget,
    }))
}

fn build_agent_full_prompt(
    prompt: &str,
    config: Option<&AgentConfig>,
    context: Option<&String>,
    output_goal: Option<&String>,
    files: &[String],
    context_files: &[String],
    context_budget_tokens: Option<u64>,
    workspace_root: &Path,
) -> Result<(String, Option<ContextFilesPrompt>), String> {
    let mut full_prompt = prompt.to_string();
    if let Some(cfg) = config {
        if let Some(instr) = cfg.instructions.as_ref() {
            if !instr.trim().is_empty() {
                full_prompt = format!("{}\n\n{}", instr.trim(), full_prompt);
            }
        }
    }
    if let Some(context) = context {
        let trimmed = full_prompt.trim_start();
        if trimmed.starts_with('/') {
            full_prompt = format!("{full_prompt}\n\nContext: {context}");
        } else {
            full_prompt = format!("Context: {context}\n\nAgent: {full_prompt}");
        }
    }
    if let Some(output_goal) = output_goal {
        full_prompt = format!("{}\n\nDesired output: {}", full_prompt, output_goal);
    }
    if !files.is_empty() {
        full_prompt = format!("{}\n\nFiles to consider: {}", full_prompt, files.join(", "));
    }

    let context_prompt =
        build_context_files_prompt(context_files, context_budget_tokens, workspace_root)?;
    if let Some(context_prompt) = context_prompt.as_ref() {
        full_prompt = format!("{}\n\n{}", full_prompt, context_prompt.block);
    }

    Ok((full_prompt, context_prompt))
}

async fn execute_agent(agent_id: String, config: Option<AgentConfig>) {
    let mut manager = AGENT_MANAGER.write().await;

    // Get agent details
    let agent = match manager.get_agent(&agent_id) {
        Some(t) => t,
        None => return,
    };

    // Update status to running
    manager
        .update_agent_status(&agent_id, AgentStatus::Running)
        .await;
    manager
        .add_progress(
            &agent_id,
            format!("Starting agent with model: {}", agent.model),
        )
        .await;

    let model = agent.model.clone();
    let model_spec = agent_model_spec(&model);
    let prompt = agent.prompt.clone();
    let read_only = agent.read_only;
    let context = agent.context.clone();
    let output_goal = agent.output_goal.clone();
    let files = agent.files.clone();
    let context_files = agent.context_files.clone();
    let context_budget_tokens = agent.context_budget_tokens;
    let reasoning_effort = agent.reasoning_effort;
    let source_kind = agent.source_kind.clone();
    let log_tag = agent.log_tag.clone();

    drop(manager); // Release the lock before executing

    let prompt_workspace = agent
        .workspace_root
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let (mut full_prompt, context_files_prompt) = match build_agent_full_prompt(
        &prompt,
        config.as_ref(),
        context.as_ref(),
        output_goal.as_ref(),
        &files,
        &context_files,
        context_budget_tokens,
        &prompt_workspace,
    ) {
        Ok(value) => value,
        Err(err) => {
            let mut manager = AGENT_MANAGER.write().await;
            manager
                .add_progress(&agent_id, format!("context_files failed: {err}"))
                .await;
            manager.update_agent_result(&agent_id, Err(err)).await;
            return;
        }
    };
    if let Some(summary) = context_files_prompt.as_ref() {
        let mut manager = AGENT_MANAGER.write().await;
        manager
            .add_progress(
                &agent_id,
                format!(
                    "Inlined {} context file(s), estimated {} tokens (budget {}).",
                    summary.included_files, summary.estimated_tokens, summary.budget_tokens
                ),
            )
            .await;
        drop(manager);
    }

    // Setup working directory and execute
    let gating_error_message = |spec: &crate::agent_defaults::AgentModelSpec| {
        if let Some(flag) = spec.gating_env {
            format!(
                "agent model '{}' is disabled; set {}=1 to enable it",
                spec.slug, flag
            )
        } else {
            format!("agent model '{}' is disabled", spec.slug)
        }
    };

    // Track optional review output path for /review agents (AutoReview)
    let mut review_output_json_path_capture: Option<PathBuf> = None;

    let result = if !read_only {
        // Check git and setup worktree for non-read-only mode
        match get_git_root().await {
            Ok(git_root) => {
                let branch_id = agent
                    .branch_name
                    .clone()
                    .unwrap_or_else(|| generate_branch_id(&model, &prompt));

                let mut manager = AGENT_MANAGER.write().await;
                manager
                    .add_progress(&agent_id, format!("Creating git worktree: {}", branch_id))
                    .await;
                drop(manager);

                match setup_worktree(&git_root, &branch_id, agent.worktree_base.as_deref()).await {
                    Ok((worktree_path, used_branch)) => {
                        let mut manager = AGENT_MANAGER.write().await;
                        manager
                            .add_progress(
                                &agent_id,
                                format!("Executing in worktree: {}", worktree_path.display()),
                            )
                            .await;
                        manager
                            .update_worktree_info(
                                &agent_id,
                                worktree_path.display().to_string(),
                                used_branch.clone(),
                            )
                            .await;
                        drop(manager);

                        // Prepare optional review-output JSON path for /review agents
                        let review_output_json_path: Option<PathBuf> = agent
                            .source_kind
                            .as_ref()
                            .and_then(|kind| matches!(kind, AgentSourceKind::AutoReview).then(|| {
                                let filename = format!("{}.review-output.json", agent_id);
                                std::env::temp_dir().join(filename)
                            }));
                        review_output_json_path_capture = review_output_json_path.clone();

                        // Execute with full permissions in the worktree
                        let use_built_in_cloud = config.is_none()
                            && model_spec
                                .map(|spec| spec.cli.eq_ignore_ascii_case("cloud"))
                                .unwrap_or_else(|| model.eq_ignore_ascii_case("cloud"));

                        if use_built_in_cloud {
                            if let Some(spec) = model_spec {
                                if !spec.is_enabled() {
                                    Err(gating_error_message(spec))
                                } else {
                                    execute_agent_provider_with_retries(&agent_id, &model, || {
                                        async {
                                            execute_cloud_built_in_streaming(
                                                &agent_id,
                                                &full_prompt,
                                                Some(worktree_path.clone()),
                                                config.clone(),
                                                spec.slug,
                                            )
                                            .await
                                            .map(|output| AgentExecutionOutput::from_child_output(output, ""))
                                        }
                                    })
                                    .await
                                }
                            } else {
                                execute_agent_provider_with_retries(&agent_id, &model, || {
                                    async {
                                        execute_cloud_built_in_streaming(
                                            &agent_id,
                                            &full_prompt,
                                            Some(worktree_path.clone()),
                                            config.clone(),
                                            model.as_str(),
                                        )
                                        .await
                                        .map(|output| AgentExecutionOutput::from_child_output(output, ""))
                                    }
                                })
                                .await
                            }
                        } else {
                            execute_agent_provider_with_retries(&agent_id, &model, || {
                                execute_model_with_permissions_detailed(
                                    &agent_id,
                                    &model,
                                    &full_prompt,
                                    false,
                                    Some(worktree_path.clone()),
                                    config.clone(),
                                    reasoning_effort,
                                    review_output_json_path.as_ref(),
                                    source_kind.clone(),
                                    log_tag.as_deref(),
                                )
                            })
                            .await
                        }
                    }
                    Err(e) => Err(format!("Failed to setup worktree: {}", e)),
                }
            }
            Err(e) => Err(format!("Git is required for non-read-only agents: {}", e)),
        }
    } else {
        // Execute in read-only mode
        full_prompt = format!(
            "{}\n\n[Running in read-only mode - no modifications allowed]",
            full_prompt
        );
        let use_built_in_cloud = config.is_none()
            && model_spec
                .map(|spec| spec.cli.eq_ignore_ascii_case("cloud"))
                .unwrap_or_else(|| model.eq_ignore_ascii_case("cloud"));

        if use_built_in_cloud {
            if let Some(spec) = model_spec {
                if !spec.is_enabled() {
                    Err(gating_error_message(spec))
                } else {
                    execute_agent_provider_with_retries(&agent_id, &model, || {
                        async {
                            execute_cloud_built_in_streaming(
                                &agent_id,
                                &full_prompt,
                                None,
                                config.clone(),
                                spec.slug,
                            )
                            .await
                            .map(|output| AgentExecutionOutput::from_child_output(output, ""))
                        }
                    })
                    .await
                }
            } else {
                execute_agent_provider_with_retries(&agent_id, &model, || {
                    async {
                        execute_cloud_built_in_streaming(
                            &agent_id,
                            &full_prompt,
                            None,
                            config.clone(),
                            model.as_str(),
                        )
                        .await
                        .map(|output| AgentExecutionOutput::from_child_output(output, ""))
                    }
                })
                .await
            }
        } else {
            execute_agent_provider_with_retries(&agent_id, &model, || {
                execute_model_with_permissions_detailed(
                    &agent_id,
                    &model,
                    &full_prompt,
                    true,
                    None,
                    config.clone(),
                    reasoning_effort,
                    None,
                    source_kind.clone(),
                    log_tag.as_deref(),
                )
            })
            .await
        }
    };

    // Update result; if a review-output JSON was produced, prefer its contents.
    let final_result = prefer_json_result_detailed(review_output_json_path_capture.as_ref(), result);
    let mut manager = AGENT_MANAGER.write().await;
    match final_result {
        Ok(output) => {
            manager
                .update_agent_result_with_token_count(&agent_id, Ok(output.output), output.token_count)
                .await;
        }
        Err(error) => {
            manager.update_agent_result(&agent_id, Err(error)).await;
        }
    }
}

async fn execute_agent_provider_with_retries<F, Fut>(
    agent_id: &str,
    model: &str,
    mut run: F,
) -> Result<AgentExecutionOutput, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<AgentExecutionOutput, String>>,
{
    let mut retry_count = 0usize;
    let mut last_retryable_error: Option<String> = None;

    loop {
        let result = run().await;
        match result {
            Ok(output) => {
                if retry_count > 0 {
                    let mut manager = AGENT_MANAGER.write().await;
                    manager
                        .update_agent_retry_metadata(
                            agent_id,
                            retry_count as u32,
                            last_retryable_error.clone(),
                        )
                        .await;
                }
                return Ok(output);
            }
            Err(error)
                if retry_count < DEFAULT_AGENT_PROVIDER_MAX_RETRIES
                    && classify_agent_provider_failure(&error)
                        == AgentProviderFailureClass::Retryable =>
            {
                retry_count += 1;
                last_retryable_error = Some(error.clone());
                let delay = agent_retry_delay(retry_count - 1);
                let error_preview = error.trim().replace('\n', " ");

                {
                    let mut manager = AGENT_MANAGER.write().await;
                    manager
                        .update_agent_retry_metadata(
                            agent_id,
                            retry_count as u32,
                            last_retryable_error.clone(),
                        )
                        .await;
                    manager
                        .add_progress(
                            agent_id,
                            format!(
                                "Retrying {model} after transient provider failure ({retry_count}/{DEFAULT_AGENT_PROVIDER_MAX_RETRIES}) in {:.1}s: {}",
                                delay.as_secs_f32(),
                                AgentManager::trim_to_tail_utf8(&error_preview, 500)
                            ),
                        )
                        .await;
                }

                tokio::time::sleep(TokioDuration::from_secs_f32(delay.as_secs_f32())).await;
            }
            Err(error) => {
                if retry_count > 0 {
                    let mut manager = AGENT_MANAGER.write().await;
                    manager
                        .update_agent_retry_metadata(
                            agent_id,
                            retry_count as u32,
                            last_retryable_error.clone(),
                        )
                        .await;
                    if classify_agent_provider_failure(&error) == AgentProviderFailureClass::Retryable
                    {
                        manager
                            .add_progress(
                                agent_id,
                                format!(
                                    "Exhausted {retry_count} automatic provider retries for {model}."
                                ),
                            )
                            .await;
                    }
                }
                return Err(error);
            }
        }
    }
}

#[cfg(test)]
fn prefer_json_result(path: Option<&PathBuf>, fallback: Result<String, String>) -> Result<String, String> {
    prefer_json_result_detailed(
        path,
        fallback.map(|output| AgentExecutionOutput::new(output, None)),
    )
    .map(|output| output.output)
}

fn prefer_json_result_detailed(
    path: Option<&PathBuf>,
    fallback: Result<AgentExecutionOutput, String>,
) -> Result<AgentExecutionOutput, String> {
    if let Some(p) = path {
        let json = std::fs::read_to_string(p).ok();
        if let Err(err) = std::fs::remove_file(p) {
            tracing::debug!("failed to clean review output file {}: {err}", p.display());
        }
        if let Some(json) = json {
            let token_count = fallback
                .as_ref()
                .ok()
                .and_then(|output| output.token_count)
                .or_else(|| fallback.as_ref().err().and_then(|error| extract_agent_token_count(error)));
            return Ok(AgentExecutionOutput::new(
                json,
                token_count,
            ));
        }
    }
    fallback
}

fn extract_agent_token_count(output: &str) -> Option<u64> {
    output
        .lines()
        .rev()
        .find_map(|line| extract_tokens_used_from_line(line))
}

fn extract_tokens_used_from_line(line: &str) -> Option<u64> {
    let marker = "tokens used:";
    let lower = line.to_ascii_lowercase();
    let marker_start = lower.find(marker)?;
    let after_marker = &line[marker_start + marker.len()..];
    let digits = after_marker
        .chars()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit() || *ch == ',')
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u64>().ok()
    }
}

fn remove_review_output_json(path: Option<&PathBuf>) {
    let Some(path) = path else { return };
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::debug!(
            "failed to remove stale review output file {}: {err}",
            path.display()
        ),
    }
}

async fn execute_model_with_permissions(
    agent_id: &str,
    model: &str,
    prompt: &str,
    read_only: bool,
    working_dir: Option<PathBuf>,
    config: Option<AgentConfig>,
    reasoning_effort: code_protocol::config_types::ReasoningEffort,
    review_output_json_path: Option<&PathBuf>,
    source_kind: Option<AgentSourceKind>,
    log_tag: Option<&str>,
) -> Result<String, String> {
    execute_model_with_permissions_detailed(
        agent_id,
        model,
        prompt,
        read_only,
        working_dir,
        config,
        reasoning_effort,
        review_output_json_path,
        source_kind,
        log_tag,
    )
    .await
    .map(|output| output.output)
}

async fn execute_model_with_permissions_detailed(
    agent_id: &str,
    model: &str,
    prompt: &str,
    read_only: bool,
    working_dir: Option<PathBuf>,
    config: Option<AgentConfig>,
    reasoning_effort: code_protocol::config_types::ReasoningEffort,
    review_output_json_path: Option<&PathBuf>,
    source_kind: Option<AgentSourceKind>,
    log_tag: Option<&str>,
) -> Result<AgentExecutionOutput, String> {
    remove_review_output_json(review_output_json_path);

    let spec_opt = agent_model_spec(model)
        .or_else(|| config.as_ref().and_then(|cfg| agent_model_spec(&cfg.name)));

    if let Some(spec) = spec_opt {
        if !spec.is_enabled() {
            if let Some(flag) = spec.gating_env {
                return Err(format!(
                    "agent model '{}' is disabled; set {}=1 to enable it",
                    spec.slug, flag
                ));
            }
            return Err(format!("agent model '{}' is disabled", spec.slug));
        }
    }

    // Use config command if provided, otherwise fall back to the spec CLI (or the
    // lowercase model string).
    let command = if let Some(ref cfg) = config {
        let cmd = cfg.command.trim();
        if !cmd.is_empty() {
            cfg.command.clone()
        } else if let Some(spec) = spec_opt {
            spec.cli.to_string()
        } else {
            cfg.name.clone()
        }
    } else if let Some(spec) = spec_opt {
        spec.cli.to_string()
    } else {
        model.to_lowercase()
    };

    let (command_base, command_extra_args) = split_command_and_args(&command);
    let command_for_spawn = if command_base.is_empty() {
        command.clone()
    } else {
        command_base.clone()
    };

    // Special case: for the built‑in Codex agent, prefer invoking the currently
    // running executable with the `exec` subcommand rather than relying on a
    // `codex` binary to be present on PATH. This improves portability,
    // especially on Windows where global shims may be missing.
    let model_lower = model.to_lowercase();
    let command_lower = command_for_spawn.to_ascii_lowercase();
    // `gemini` remains here for explicitly configured Gemini CLI agents, but it
    // is no longer advertised as a built-in default.
    fn is_known_family(s: &str) -> bool {
        matches!(
            s,
            "antigravity" | "claude" | "gemini" | "copilot" | "qwen" | "codex" | "code"
            | "cloud" | "coder"
        )
    }

    let slug_for_defaults = spec_opt.map(|spec| spec.slug).unwrap_or(model);
    let spec_family = spec_opt.map(|spec| spec.family);
    let family = if let Some(spec_family) = spec_family {
        spec_family
    } else if is_known_family(model_lower.as_str()) {
        model_lower.as_str()
    } else if is_known_family(command_lower.as_str()) {
        command_lower.as_str()
    } else {
        model_lower.as_str()
    };

    let command_missing = !external_agent_command_exists(&command_for_spawn);
    let use_current_exe = should_use_current_exe_for_agent(family, command_missing, config.as_ref());

    let mut final_args: Vec<String> = command_extra_args;
    let prompt_stdin: Option<String>;

    if let Some(ref cfg) = config {
        if read_only {
            if let Some(ro) = cfg.args_read_only.as_ref() {
                final_args.extend(ro.iter().cloned());
            } else {
                final_args.extend(cfg.args.iter().cloned());
            }
        } else if let Some(w) = cfg.args_write.as_ref() {
            final_args.extend(w.iter().cloned());
        } else {
            final_args.extend(cfg.args.iter().cloned());
        }
    }

    strip_model_flags(&mut final_args);

    let spec_model_args: Vec<String> = if let Some(spec) = spec_opt {
        spec.model_args.iter().map(|arg| (*arg).to_string()).collect()
    } else {
        Vec::new()
    };

    let built_in_cloud = family == "cloud" && config.is_none();

    // Clamp reasoning effort to what the target model supports.
    let target_model = slug_for_defaults
        .strip_prefix("code-")
        .or_else(|| slug_for_defaults.strip_prefix("cloud-"))
        .unwrap_or(slug_for_defaults);
    let clamped_effort: code_protocol::config_types::ReasoningEffort =
        crate::reasoning::clamp_reasoning_effort_for_model(target_model, reasoning_effort.into())
            .into();

    // Configuration overrides for Codex CLI families. External CLIs
    // (antigravity, claude, gemini, copilot, qwen) do not understand our config
    // flags, so only attach these when launching Codex binaries.
    let effort_override = format!(
        "model_reasoning_effort={}",
        clamped_effort.to_string().to_ascii_lowercase()
    );
    let auto_effort_override = format!(
        "auto_drive.model_reasoning_effort={}",
        clamped_effort.to_string().to_ascii_lowercase()
    );
    match family {
        "copilot" => {
            let mut defaults = default_params_for(slug_for_defaults, read_only);
            strip_model_flags(&mut defaults);
            final_args.extend(defaults);
            final_args.extend(spec_model_args.iter().cloned());
            final_args.push("--reasoning-effort".into());
            final_args.push(clamped_effort.to_string().to_ascii_lowercase());
            final_args.push("-p".into());
            prompt_stdin = deliver_agent_prompt(family, &mut final_args, prompt, false, false)?;
        }
        "antigravity" | "claude" | "gemini" | "qwen" => {
            let mut defaults = default_params_for(slug_for_defaults, read_only);
            strip_model_flags(&mut defaults);
            final_args.extend(defaults);
            final_args.extend(spec_model_args.iter().cloned());
            if family == "antigravity" {
                let dir = working_dir
                    .clone()
                    .or_else(|| std::env::current_dir().ok());
                if let Some(dir) = dir {
                    final_args.push("--add-dir".into());
                    final_args.push(dir.display().to_string());
                }
            }
            final_args.push("-p".into());
            prompt_stdin = deliver_agent_prompt(family, &mut final_args, prompt, false, false)?;
        }
        "codex" | "code" => {
            let have_mode_args = config
                .as_ref()
                .map(|c| if read_only { c.args_read_only.is_some() } else { c.args_write.is_some() })
                .unwrap_or(false);
            if !have_mode_args {
                let mut defaults = default_params_for(slug_for_defaults, read_only);
                strip_model_flags(&mut defaults);
                final_args.extend(defaults);
            }
            final_args.extend(spec_model_args.iter().cloned());
            final_args.push("-c".into());
            final_args.push(effort_override.clone());
            final_args.push("-c".into());
            final_args.push(auto_effort_override.clone());
            prompt_stdin = deliver_agent_prompt(family, &mut final_args, prompt, true, false)?;
        }
        "cloud" => {
            if built_in_cloud {
                final_args.extend(["cloud", "submit", "--wait"].map(String::from));
            }
            let have_mode_args = config
                .as_ref()
                .map(|c| if read_only { c.args_read_only.is_some() } else { c.args_write.is_some() })
                .unwrap_or(false);
            if !have_mode_args {
                let mut defaults = default_params_for(slug_for_defaults, read_only);
                strip_model_flags(&mut defaults);
                final_args.extend(defaults);
            }
            final_args.extend(spec_model_args.iter().cloned());
            final_args.push("-c".into());
            final_args.push(effort_override.clone());
            final_args.push("-c".into());
            final_args.push(auto_effort_override);
            prompt_stdin = deliver_agent_prompt(family, &mut final_args, prompt, false, false)?;
        }
        _ => {
            final_args.extend(spec_model_args.iter().cloned());
            prompt_stdin = deliver_agent_prompt(family, &mut final_args, prompt, false, false)?;
        }
    }

    let log_tag_owned = log_tag.map(str::to_string);
    let debug_subagent = debug_subagents_enabled()
        && matches!(source_kind, Some(AgentSourceKind::AutoReview));
    let child_log_tag: Option<String> = if debug_subagent {
        Some(log_tag_owned.clone().unwrap_or_else(|| format!("agents/{agent_id}")))
    } else {
        log_tag_owned
    };

    if debug_subagent && use_current_exe && !has_debug_flag(&final_args) {
        final_args.insert(0, "--debug".to_string());
    }

    if let Some(path) = review_output_json_path {
        final_args.push("--review-output-json".to_string());
        final_args.push(path.display().to_string());
    }

    if use_current_exe
        && (final_args.iter().any(|arg| arg == "exec") || review_output_json_path.is_some())
    {
        let mut reordered: Vec<String> = Vec::with_capacity(final_args.len() + 1);
        reordered.push("exec".to_string());
        for arg in final_args.into_iter() {
            if arg != "exec" {
                reordered.push(arg);
            }
        }
        final_args = reordered;
    }

    // Proactively check for presence of external command before spawn when not
    // using the current executable fallback. This avoids confusing OS errors
    // like "program not found" and lets us surface a cleaner message.
    if !(family == "codex" || family == "code" || (family == "cloud" && config.is_none()))
        && !external_agent_command_exists(&command_for_spawn)
    {
        return Err(format_agent_not_found_error(&command, &command_for_spawn));
    }

    // Agents: run without OS sandboxing; rely on per-branch worktrees for isolation.
    use crate::protocol::SandboxPolicy;
    use crate::spawn::StdioPolicy;
    // Build env from current process then overlay any config-provided vars.
    let mut env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let orig_home: Option<String> = env.get("HOME").cloned();
    if let Some(ref cfg) = config {
        if let Some(ref e) = cfg.env { for (k, v) in e { env.insert(k.clone(), v.clone()); } }
    }
    let child_spawn_depth = current_agent_spawn_depth().saturating_add(1);
    env.insert(
        CODE_AGENT_SPAWN_DEPTH_ENV.to_string(),
        child_spawn_depth.to_string(),
    );

    if debug_subagent {
        env.entry("CODE_SUBAGENT_DEBUG".to_string())
            .or_insert_with(|| "1".to_string());
        if let Some(tag) = child_log_tag.as_ref() {
            env.insert("CODE_DEBUG_LOG_TAG".to_string(), tag.clone());
        }
    }

    // Tag OpenAI requests originating from agent runs so server-side telemetry
    // can distinguish subagent traffic.
    if use_current_exe || family == "codex" || family == "code" {
        let subagent = match source_kind {
            Some(AgentSourceKind::AutoReview) => "review",
            _ => "agent",
        };
        env.entry("CODE_OPENAI_SUBAGENT".to_string())
            .or_insert_with(|| subagent.to_string());
    }

    // Convenience: map common key names so external CLIs "just work".
    if let Some(google_key) = env.get("GOOGLE_API_KEY").cloned() {
        env.entry("GEMINI_API_KEY".to_string()).or_insert(google_key);
    }
    if let Some(claude_key) = env.get("CLAUDE_API_KEY").cloned() {
        env.entry("ANTHROPIC_API_KEY".to_string()).or_insert(claude_key);
    }
    if let Some(anthropic_key) = env.get("ANTHROPIC_API_KEY").cloned() {
        env.entry("CLAUDE_API_KEY".to_string()).or_insert(anthropic_key);
    }
    if let Some(anthropic_base) = env.get("ANTHROPIC_BASE_URL").cloned() {
        env.entry("CLAUDE_BASE_URL".to_string()).or_insert(anthropic_base);
    }
    // Qwen/DashScope convenience: mirror API keys and base URLs both ways so
    // either variable name works across tools.
    if let Some(qwen_key) = env.get("QWEN_API_KEY").cloned() {
        env.entry("DASHSCOPE_API_KEY".to_string()).or_insert(qwen_key);
    }
    if let Some(dashscope_key) = env.get("DASHSCOPE_API_KEY").cloned() {
        env.entry("QWEN_API_KEY".to_string()).or_insert(dashscope_key);
    }
    if let Some(qwen_base) = env.get("QWEN_BASE_URL").cloned() {
        env.entry("DASHSCOPE_BASE_URL".to_string()).or_insert(qwen_base);
    }
    if let Some(ds_base) = env.get("DASHSCOPE_BASE_URL").cloned() {
        env.entry("QWEN_BASE_URL".to_string()).or_insert(ds_base);
    }
    if family == "qwen" {
        env.insert("OPENAI_API_KEY".to_string(), String::new());
    }
    // Reduce startup overhead for Claude CLI: disable auto-updater/telemetry.
    env.entry("DISABLE_AUTOUPDATER".to_string()).or_insert("1".to_string());
    env.entry("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".to_string()).or_insert("1".to_string());
    env.entry("DISABLE_ERROR_REPORTING".to_string()).or_insert("1".to_string());
    // Prefer explicit Claude config dir to avoid touching $HOME/.claude.json.
    // Do not force CLAUDE_CONFIG_DIR here; leave CLI free to use its default
    // (including Keychain) unless we explicitly redirect HOME below.

    // If GEMINI_API_KEY not provided, try pointing to host config for read‑only
    // discovery (Gemini CLI supports GEMINI_CONFIG_DIR). We keep HOME as-is so
    // CLIs that require ~/.gemini and ~/.claude continue to work with your
    // existing config.
    maybe_set_gemini_config_dir(&mut env, orig_home.clone());

    let output = if !read_only {
        // Resolve the command and args we prepared above into Vec<String> for spawn helpers.
        let program = resolve_program_path(use_current_exe, &command_for_spawn)?;
        let args = final_args.clone();
        let prompt_stdin_for_child = prompt_stdin.clone();
        let launch_cwd = agent_launch_cwd(family, working_dir.clone(), orig_home.as_deref());
        if family == "antigravity" && let Err(err) = std::fs::create_dir_all(&launch_cwd) {
            return Err(format!(
                "Failed to create agent launch directory {}: {err}",
                launch_cwd.display()
            ));
        }

        let child_result: std::io::Result<tokio::process::Child> = crate::spawn::spawn_child_async(
            program.clone(),
            args.clone(),
            Some(program.to_string_lossy().as_ref()),
            launch_cwd,
            &SandboxPolicy::DangerFullAccess,
            if prompt_stdin_for_child.is_some() {
                StdioPolicy::RedirectForShellToolWithPipedStdin
            } else {
                StdioPolicy::RedirectForShellTool
            },
            env.clone(),
        )
        .await;

        match child_result {
            Ok(child) => stream_child_output(agent_id, child, prompt_stdin_for_child).await?,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(format_agent_not_found_error(&command, &command_for_spawn));
                }
                return Err(format!("Failed to spawn sandboxed agent: {}", e));
            }
        }
    } else {
        // Read-only path: must honor resolve_program_path (and CODE_BINARY_PATH) just
        // like the write path; skipping this can regress to PATH resolution and
        // launch the npm shim on Windows (issue #497).
        let program = resolve_program_path(use_current_exe, &command_for_spawn)?;
        let mut cmd = Command::new(program);
        let launch_cwd = agent_launch_cwd(family, working_dir.clone(), orig_home.as_deref());
        if family == "antigravity" && let Err(err) = std::fs::create_dir_all(&launch_cwd) {
            return Err(format!(
                "Failed to create agent launch directory {}: {err}",
                launch_cwd.display()
            ));
        }

        cmd.current_dir(launch_cwd);

        cmd.args(final_args.clone());
        if prompt_stdin.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        for (k, v) in &env {
            cmd.env(k, v);
        }

        // Ensure the child is terminated if this process dies unexpectedly.
        cmd.kill_on_drop(true);

        match spawn_tokio_command_with_retry(&mut cmd).await {
            Ok(child) => stream_child_output(agent_id, child, prompt_stdin).await?,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(format_agent_not_found_error(&command, &command_for_spawn));
                }

                return Err(format!("Failed to execute {}: {}", model, e));
            }
        }
    };

    let (status, stdout_buf, stderr_buf) = output;

    if status.success() {
        Ok(AgentExecutionOutput::from_child_output(stdout_buf, &stderr_buf))
    } else {
        let stderr = stderr_buf.trim();
        let stdout = stdout_buf.trim();
        let combined = if stderr.is_empty() {
            stdout.to_string()
        } else if stdout.is_empty() {
            stderr.to_string()
        } else {
            format!("{}\n{}", stderr, stdout)
        };
        Err(format!("Command failed: {}", combined))
    }
}

const STREAM_PROGRESS_INTERVAL: StdDuration = StdDuration::from_secs(2);
const STREAM_PROGRESS_BYTES: usize = 2 * 1024;

async fn stream_child_output(
    agent_id: &str,
    mut child: tokio::process::Child,
    stdin_content: Option<String>,
) -> Result<(std::process::ExitStatus, String, String), String> {
    let agent_id_owned = agent_id.to_string();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_clone = stop_flag.clone();
    let heartbeat = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TokioDuration::from_secs(30));
        loop {
            ticker.tick().await;
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            AgentManager::touch_agent(&agent_id_owned).await;
        }
    });

    let stdout_task = child.stdout.take().map(|stdout| {
        let agent = agent_id.to_string();
        tokio::spawn(async move { stream_reader_to_progress(agent, "stdout", stdout).await })
    });

    let stderr_task = child.stderr.take().map(|stderr| {
        let agent = agent_id.to_string();
        tokio::spawn(async move { stream_reader_to_progress(agent, "stderr", stderr).await })
    });

    let stdin_task = if let Some(stdin_content) = stdin_content {
        let Some(mut stdin) = child.stdin.take() else {
            return Err("failed to open agent stdin for large prompt delivery".to_string());
        };
        Some(tokio::spawn(async move {
            stdin
                .write_all(stdin_content.as_bytes())
                .await
                .map_err(|err| format!("failed to write agent prompt to stdin: {err}"))
        }))
    } else {
        None
    };

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for agent process: {e}"))?;

    if let Some(handle) = stdin_task {
        handle
            .await
            .map_err(|e| format!("Failed to join agent stdin writer: {e}"))??;
    }

    let stdout_buf = match stdout_task {
        Some(handle) => handle
            .await
            .map_err(|e| format!("Failed to read agent stdout: {e}"))?,
        None => String::new(),
    };

    let stderr_buf = match stderr_task {
        Some(handle) => handle
            .await
            .map_err(|e| format!("Failed to read agent stderr: {e}"))?,
        None => String::new(),
    };

    stop_flag.store(true, Ordering::Relaxed);
    heartbeat.abort();

    Ok((status, stdout_buf, stderr_buf))
}

async fn stream_reader_to_progress<R>(agent_id: String, label: &str, reader: R) -> String
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut full = String::new();
    let mut chunk = String::new();
    let mut last_flush = Instant::now();

    while let Ok(Some(line)) = lines.next_line().await {
        let clean = line.trim_end_matches('\r');
        full.push_str(clean);
        full.push('\n');
        chunk.push_str(clean);
        chunk.push('\n');

        if chunk.len() >= STREAM_PROGRESS_BYTES || last_flush.elapsed() >= STREAM_PROGRESS_INTERVAL {
            flush_progress(&agent_id, label, &mut chunk).await;
            last_flush = Instant::now();
        }
    }

    if !chunk.is_empty() {
        flush_progress(&agent_id, label, &mut chunk).await;
    }

    full
}

async fn flush_progress(agent_id: &str, label: &str, chunk: &mut String) {
    let message = format!("[{label}] {}", chunk.trim_end());
    let mut mgr = AGENT_MANAGER.write().await;
    mgr.add_progress(agent_id, message).await;
    chunk.clear();
}

fn debug_subagents_enabled() -> bool {
    match std::env::var("CODE_SUBAGENT_DEBUG") {
        Ok(val) => {
            let lower = val.to_ascii_lowercase();
            matches!(lower.as_str(), "1" | "true" | "yes" | "on")
        }
        Err(_) => false,
    }
}

fn has_debug_flag(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--debug" || arg == "-d")
}

fn maybe_set_gemini_config_dir(env: &mut HashMap<String, String>, orig_home: Option<String>) {
    if env.get("GEMINI_API_KEY").is_some() {
        return;
    }

    let Some(home) = orig_home else { return; };
    let host_gem_cfg = std::path::PathBuf::from(&home).join(".gemini");
    if host_gem_cfg.is_dir() {
        env.insert(
            "GEMINI_CONFIG_DIR".to_string(),
            host_gem_cfg.to_string_lossy().to_string(),
        );
    }
}

fn antigravity_launch_dir(orig_home: Option<&str>) -> PathBuf {
    crate::config::find_code_home()
        .or_else(|_| {
            orig_home
                .map(|home| PathBuf::from(home).join(".code"))
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))
        })
        .unwrap_or_else(|_| std::env::temp_dir().join("code"))
        .join("agent-cache")
        .join("antigravity")
}

fn agent_launch_cwd(family: &str, working_dir: Option<PathBuf>, orig_home: Option<&str>) -> PathBuf {
    if family == "antigravity" {
        return antigravity_launch_dir(orig_home);
    }

    working_dir.unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    })
}

pub(crate) fn should_use_current_exe_for_agent(
    family: &str,
    command_missing: bool,
    config: Option<&AgentConfig>,
) -> bool {
    if !matches!(family, "code" | "codex" | "cloud" | "coder") {
        return false;
    }

    // If the command is missing/empty, always use the current binary.
    if command_missing {
        return true;
    }

    if let Some(cfg) = config {
        let trimmed = cfg.command.trim();
        if trimmed.is_empty() {
            return true;
        }

        // If the configured command matches the canonical CLI for this spec, prefer self.
        if let Some(spec) = agent_model_spec(&cfg.name).or_else(|| agent_model_spec(trimmed)) {
            if trimmed.eq_ignore_ascii_case(spec.cli) {
                return true;
            }
        }

        // Otherwise assume the user intentionally set a custom command; do not override.
        false
    } else {
        // No explicit config: built-in families should use the current binary.
        true
    }
}

fn resolve_program_path(use_current_exe: bool, command_for_spawn: &str) -> Result<std::path::PathBuf, String> {
    if use_current_exe {
        return current_code_binary_path();
    }

    if let Some(path) = resolve_external_agent_command_path(command_for_spawn) {
        return Ok(path);
    }

    Ok(std::path::PathBuf::from(command_for_spawn))
}

fn strip_model_flags(args: &mut Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        let lowered = args[i].to_ascii_lowercase();
        if lowered == "--model" || lowered == "-m" {
            args.remove(i);
            if i < args.len() {
                args.remove(i);
            }
            continue;
        }
        if lowered.starts_with("--model=") || lowered.starts_with("-m=") {
            args.remove(i);
            continue;
        }
        i += 1;
    }
}

pub fn split_command_and_args(command: &str) -> (String, Vec<String>) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return (String::new(), Vec::new());
    }
    if let Some(tokens) = shlex_split(trimmed) {
        if let Some((first, rest)) = tokens.split_first() {
            return (first.clone(), rest.to_vec());
        }
    }

    let tokens: Vec<String> = trimmed.split_whitespace().map(|s| s.to_string()).collect();
    if tokens.is_empty() {
        (String::new(), Vec::new())
    } else {
        let head = tokens[0].clone();
        (head, tokens[1..].to_vec())
    }
}

const AGENT_SMOKE_TEST_PROMPT: &str = "Reply only with the string \"ok\". Do not include any other words.";
const AGENT_SMOKE_TEST_EXPECTED: &str = "ok";
const AGENT_SMOKE_TEST_TIMEOUT: TokioDuration = TokioDuration::from_secs(20);

fn should_validate_in_read_only(_cfg: &AgentConfig) -> bool { true }

async fn run_agent_smoke_test(cfg: AgentConfig) -> Result<String, String> {
    let model_name = cfg.name.clone();
    let read_only = should_validate_in_read_only(&cfg);
    let mut task = tokio::spawn(async move {
        execute_model_with_permissions(
            "agent-smoke-test",
            &model_name,
            AGENT_SMOKE_TEST_PROMPT,
            read_only,
            None,
            Some(cfg),
            code_protocol::config_types::ReasoningEffort::High,
            None,
            None,
            None,
        )
        .await
    });
    let timer = tokio::time::sleep(AGENT_SMOKE_TEST_TIMEOUT);
    tokio::pin!(timer);
    tokio::select! {
        res = &mut task => {
            res.map_err(|e| format!("agent validation task failed: {e}"))?
        }
        _ = timer.as_mut() => {
            task.abort();
            let _ = task.await;
            return Err(format!(
                "agent validation timed out after {}s",
                AGENT_SMOKE_TEST_TIMEOUT.as_secs()
            ));
        }
    }
}

fn summarize_agent_output(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "<empty response>".to_string();
    }
    const MAX_LEN: usize = 240;
    if trimmed.len() <= MAX_LEN {
        trimmed.to_string()
    } else {
        let mut cutoff = MAX_LEN.min(trimmed.len());
        while cutoff > 0 && !trimmed.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        if cutoff == 0 {
            // Fallback: take first char to avoid empty slice
            let mut chars = trimmed.chars();
            if let Some(first) = chars.next() {
                format!("{}…", first)
            } else {
                "…".to_string()
            }
        } else {
            format!("{}…", &trimmed[..cutoff])
        }
    }
}

pub async fn smoke_test_agent(cfg: AgentConfig) -> Result<(), String> {
    let output = run_agent_smoke_test(cfg).await?;
    let normalized = output.trim().to_ascii_lowercase();
    if normalized == AGENT_SMOKE_TEST_EXPECTED {
        Ok(())
    } else {
        Err(format!(
            "agent response missing \"ok\": {}",
            summarize_agent_output(&output)
        ))
    }
}

fn run_smoke_test_with_new_runtime(cfg: AgentConfig) -> Result<(), String> {
    TokioRuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build validation runtime: {}", e))?
        .block_on(smoke_test_agent(cfg))
}

pub fn smoke_test_agent_blocking(cfg: AgentConfig) -> Result<(), String> {
    if tokio::runtime::Handle::try_current().is_ok() {
        thread::Builder::new()
            .name("agent-smoke-test".into())
            .spawn(move || run_smoke_test_with_new_runtime(cfg))
            .map_err(|e| format!("failed to spawn agent validation thread: {}", e))?
            .join()
            .map_err(|_| "agent validation thread panicked".to_string())?
    } else {
        run_smoke_test_with_new_runtime(cfg)
    }
}

/// Execute the built-in cloud agent via the current `code` binary, streaming
/// stderr lines into the HUD as progress and returning final stdout. Applies a
/// modest truncation cap to very large outputs to keep UI responsive.
async fn execute_cloud_built_in_streaming(
    agent_id: &str,
    prompt: &str,
    working_dir: Option<std::path::PathBuf>,
    _config: Option<AgentConfig>,
    model_slug: &str,
) -> Result<String, String> {
    if prompt.len() > AGENT_PROMPT_ARGV_THRESHOLD_BYTES {
        return Err(format!(
            "built-in cloud agent prompt is {} bytes, above the {} byte argv delivery threshold. Use a built-in Every Code/Codex agent such as code-gpt-5.4 for large context_files, or reduce the inlined context.",
            prompt.len(),
            AGENT_PROMPT_ARGV_THRESHOLD_BYTES
        ));
    }

    // Program and argv
    let program = current_code_binary_path()?;
    let mut args: Vec<String> = vec!["cloud".into(), "submit".into(), "--wait".into()];
    if let Some(spec) = agent_model_spec(model_slug) {
        args.extend(spec.model_args.iter().map(|arg| (*arg).to_string()));
    }
    args.push(prompt.into());

    // Baseline env mirrors behavior in execute_model_with_permissions
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();

    use crate::protocol::SandboxPolicy;
    use crate::spawn::StdioPolicy;
    let mut child = crate::spawn::spawn_child_async(
        program.clone(),
        args.clone(),
        Some(program.to_string_lossy().as_ref()),
        working_dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))),
        &SandboxPolicy::DangerFullAccess,
        StdioPolicy::RedirectForShellTool,
        env,
    )
    .await
    .map_err(|e| format!("Failed to spawn cloud submit: {}", e))?;

    // Stream stderr to HUD
    let stderr_task = if let Some(stderr) = child.stderr.take() {
        let agent = agent_id.to_string();
        Some(tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg = line.trim();
                if msg.is_empty() { continue; }
                let mut mgr = AGENT_MANAGER.write().await;
                mgr.add_progress(&agent, msg.to_string()).await;
            }
        }))
    } else { None };

    // Collect stdout fully (final result)
    let mut stdout_buf = String::new();
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            stdout_buf.push_str(&line);
            stdout_buf.push('\n');
        }
    }

    let status = child.wait().await.map_err(|e| format!("Failed to wait: {}", e))?;
    if let Some(t) = stderr_task { let _ = t.await; }
    if !status.success() {
        return Err(format!("cloud submit exited with status {}", status));
    }

    if let Some(dir) = working_dir.as_ref() {
        let diff_text_opt = if stdout_buf.starts_with("diff --git ") {
            Some(stdout_buf.trim())
        } else {
            stdout_buf
                .find("\ndiff --git ")
                .map(|idx| stdout_buf[idx + 1..].trim())
        };

        if let Some(diff_text) = diff_text_opt {
            if !diff_text.is_empty() {
                let mut apply = Command::new("git");
                apply.arg("apply").arg("--whitespace=nowarn");
                apply.current_dir(dir);
                apply.stdin(Stdio::piped());

                let mut child = spawn_tokio_command_with_retry(&mut apply)
                    .await
                    .map_err(|e| format!("Failed to spawn git apply: {}", e))?;

                if let Some(mut stdin) = child.stdin.take() {
                    stdin
                        .write_all(diff_text.as_bytes())
                        .await
                        .map_err(|e| format!("Failed to write diff to git apply: {}", e))?;
                }

                let status = child
                    .wait()
                    .await
                    .map_err(|e| format!("Failed to wait for git apply: {}", e))?;

                if !status.success() {
                    return Err(format!(
                        "git apply exited with status {} while applying cloud diff",
                        status
                    ));
                }
            }
        }
    }

    // Truncate large outputs
    const MAX_BYTES: usize = 500_000; // ~500 KB
    if stdout_buf.len() > MAX_BYTES {
        let omitted = stdout_buf.len() - MAX_BYTES;
        let mut truncated = String::with_capacity(MAX_BYTES + 128);
        truncated.push_str(&stdout_buf[..MAX_BYTES]);
        truncated.push_str(&format!("\n… [truncated: {} bytes omitted]", omitted));
        Ok(truncated)
    } else {
        Ok(stdout_buf)
    }
}

// Tool creation functions

pub fn create_agent_tool(allowed_models: &[String]) -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "action".to_string(),
        JsonSchema::String {
            description: Some(
                "Required: choose one of ['create','status','wait','result','cancel','list']".to_string(),
            ),
            allowed_values: Some(
                ["create", "status", "wait", "result", "cancel", "list"]
                    .into_iter()
                    .map(|value| value.to_string())
                    .collect(),
            ),
        },
    );

    let mut create_properties = BTreeMap::new();
    create_properties.insert(
        "name".to_string(),
        JsonSchema::String {
            description: Some("Display name shown in the UI (e.g., \"Plan TUI Refactor\")".to_string()),
            allowed_values: None,
        },
    );
    create_properties.insert(
        "task".to_string(),
        JsonSchema::String {
            description: Some("Task prompt to execute".to_string()),
            allowed_values: None,
        },
    );
    create_properties.insert(
        "context".to_string(),
        JsonSchema::String {
            description: Some("Optional background context".to_string()),
            allowed_values: None,
        },
    );
    create_properties.insert(
        "models".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String {
                description: None,
                allowed_values: if allowed_models.is_empty() {
                    None
                } else {
                    Some(allowed_models.iter().cloned().collect())
                },
            }),
            description: Some(format!(
                "Optional array of agent/model selector slugs. For explicit multi-agent or dissent requests, prefer diverse families when useful and budget allows (for example ['code-gpt-5.5','claude-sonnet-4.6','antigravity']). For multi-agent release/workflow, infrastructure, security, or product-risk work, proactively use `antigravity` for Google/Gemini-family perspective unless there is a clear reason to skip it; AGY uses its configured model rather than per-run Gemini Pro/Flash selection. If you skip an obvious family, briefly explain why. Fable restriction: {FABLE_AGENT_GUIDANCE}"
            )),
        },
    );
    create_properties.insert(
        "files".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String {
                description: None,
                allowed_values: None,
            }),
            description: Some(
                "Optional array of file paths for the agent to consider. Contents are not inlined; use context_files when the subagent needs file contents in its initial prompt.".to_string(),
            ),
        },
    );
    create_properties.insert(
        "context_files".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String {
                description: None,
                allowed_values: None,
            }),
            description: Some(
                "Optional array of text file paths whose contents should be snapshotted and inlined into the spawned agent's initial prompt. Use sparingly for curated context; prefer files for path hints and code llm request --message-file for strict rollout/model evaluation.".to_string(),
            ),
        },
    );
    create_properties.insert(
        "context_budget_tokens".to_string(),
        JsonSchema::Number {
            description: Some(
                "Approximate integer-valued token budget for inlined context_files. Defaults to 16000 and caps at 900000; set explicitly for expensive large-context launches.".to_string(),
            ),
        },
    );
    create_properties.insert(
        "output".to_string(),
        JsonSchema::String {
            description: Some("Optional desired output description".to_string()),
            allowed_values: None,
        },
    );
    create_properties.insert(
        "write".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Enable isolated write worktrees for each agent (default: true). Set false to keep the agent read-only.".to_string(),
            ),
        },
    );
    create_properties.insert(
        "read_only".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Deprecated: inverse of `write`. Prefer setting `write` instead.".to_string(),
            ),
        },
    );
    properties.insert(
        "create".to_string(),
        JsonSchema::Object {
            properties: create_properties,
            required: Some(vec!["task".to_string()]),
            additional_properties: Some(false.into()),
        },
    );

    let mut status_properties = BTreeMap::new();
    status_properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("Agent identifier to inspect".to_string()),
            allowed_values: None,
        },
    );
    properties.insert(
        "status".to_string(),
        JsonSchema::Object {
            properties: status_properties,
            required: Some(vec!["agent_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    );

    let mut result_properties = BTreeMap::new();
    result_properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("Agent identifier whose result should be fetched".to_string()),
            allowed_values: None,
        },
    );
    properties.insert(
        "result".to_string(),
        JsonSchema::Object {
            properties: result_properties,
            required: Some(vec!["agent_id".to_string()]),
            additional_properties: Some(false.into()),
        },
    );

    let mut cancel_properties = BTreeMap::new();
    cancel_properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("Cancel a specific agent".to_string()),
            allowed_values: None,
        },
    );
    cancel_properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some("Cancel all agents in the batch".to_string()),
            allowed_values: None,
        },
    );
    properties.insert(
        "cancel".to_string(),
        JsonSchema::Object {
            properties: cancel_properties,
            required: Some(Vec::new()),
            additional_properties: Some(false.into()),
        },
    );

    let mut wait_properties = BTreeMap::new();
    wait_properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("Wait for a specific agent".to_string()),
            allowed_values: None,
        },
    );
    wait_properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some("Wait for any agent in the batch".to_string()),
            allowed_values: None,
        },
    );
    wait_properties.insert(
        "timeout_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Optional timeout before giving up (default 300, max 600)".to_string(),
            ),
        },
    );
    wait_properties.insert(
        "return_all".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "When waiting on a batch, return all completed agents instead of the first".to_string(),
            ),
        },
    );
    properties.insert(
        "wait".to_string(),
        JsonSchema::Object {
            properties: wait_properties,
            required: Some(Vec::new()),
            additional_properties: Some(false.into()),
        },
    );

    let mut list_properties = BTreeMap::new();
    list_properties.insert(
        "status_filter".to_string(),
        JsonSchema::String {
            description: Some(
                "Optional status filter (pending, running, completed, failed, cancelled)".to_string(),
            ),
            allowed_values: None,
        },
    );
    list_properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some("Limit results to a batch".to_string()),
            allowed_values: None,
        },
    );
    list_properties.insert(
        "recent_only".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "When true, only include agents from the last two hours".to_string(),
            ),
        },
    );
    properties.insert(
        "list".to_string(),
        JsonSchema::Object {
            properties: list_properties,
            required: Some(Vec::new()),
            additional_properties: Some(false.into()),
        },
    );

    let required = Some(vec!["action".to_string()]);

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent".to_string(),
        description: "Unified agent manager for launching, monitoring, and collecting results from asynchronous agents.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required,
            additional_properties: Some(false.into()),
        },
    })
}

// Parameter structs for handlers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAgentParams {
    pub task: String,
    #[serde(default, deserialize_with = "deserialize_models_field")]
    pub models: Vec<String>,
    pub context: Option<String>,
    pub output: Option<String>,
    pub files: Option<Vec<String>>,
    pub context_files: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_number")]
    pub context_budget_tokens: Option<u64>,
    #[serde(default)]
    pub write: Option<bool>,
    #[serde(default)]
    pub read_only: Option<bool>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCreateOptions {
    pub task: Option<String>,
    #[serde(default, deserialize_with = "deserialize_models_field")]
    pub models: Vec<String>,
    pub context: Option<String>,
    pub output: Option<String>,
    pub files: Option<Vec<String>>,
    pub context_files: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_number")]
    pub context_budget_tokens: Option<u64>,
    #[serde(default)]
    pub write: Option<bool>,
    #[serde(default)]
    pub read_only: Option<bool>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentifierOptions {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCancelOptions {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWaitOptions {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub return_all: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentListOptions {
    pub status_filter: Option<String>,
    pub batch_id: Option<String>,
    pub recent_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolRequest {
    pub action: String,
    pub create: Option<AgentCreateOptions>,
    pub status: Option<AgentIdentifierOptions>,
    pub result: Option<AgentIdentifierOptions>,
    pub cancel: Option<AgentCancelOptions>,
    pub wait: Option<AgentWaitOptions>,
    pub list: Option<AgentListOptions>,
}

pub(crate) fn normalize_agent_name(name: Option<String>) -> Option<String> {
    let Some(name) = name.map(|value| value.trim().to_string()) else {
        return None;
    };

    if name.is_empty() {
        return None;
    }

    let canonicalized = canonicalize_agent_word_boundaries(&name);
    let words: Vec<&str> = canonicalized.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }

    Some(
        words
            .into_iter()
            .map(format_agent_word)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn canonicalize_agent_word_boundaries(input: &str) -> String {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut prev_char: Option<char> = None;
    let mut uppercase_run: usize = 0;

    while let Some(ch) = chars.next() {
        if ch.is_whitespace() || matches!(ch, '_' | '-' | '/' | ':' | '.') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_char = None;
            uppercase_run = 0;
            continue;
        }

        let next_char = chars.peek().copied();
        let mut split = false;

        if !current.is_empty() {
            if let Some(prev) = prev_char {
                if prev.is_ascii_lowercase() && ch.is_ascii_uppercase() {
                    split = true;
                } else if prev.is_ascii_uppercase()
                    && ch.is_ascii_uppercase()
                    && uppercase_run > 0
                    && next_char.map_or(false, |c| c.is_ascii_lowercase())
                {
                    split = true;
                }
            }
        }

        if split {
            tokens.push(std::mem::take(&mut current));
            uppercase_run = 0;
        }

        current.push(ch);

        if ch.is_ascii_uppercase() {
            uppercase_run += 1;
        } else {
            uppercase_run = 0;
        }

        prev_char = Some(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens.join(" ")
}

const AGENT_NAME_ACRONYMS: &[&str] = &[
    "AI", "API", "CLI", "CPU", "DB", "GPU", "HTTP", "HTTPS", "ID", "LLM", "SDK", "SQL", "TUI", "UI", "UX",
];

fn format_agent_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }

    let uppercase = word.to_ascii_uppercase();
    if AGENT_NAME_ACRONYMS.contains(&uppercase.as_str()) {
        return uppercase;
    }

    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };

    let mut formatted = String::new();
    formatted.extend(first.to_uppercase());
    formatted.push_str(&chars.flat_map(char::to_lowercase).collect::<String>());
    formatted
}

fn deserialize_models_field<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ModelsInput {
        Seq(Vec<String>),
        One(String),
    }

    let parsed = Option::<ModelsInput>::deserialize(deserializer)?;
    Ok(match parsed {
        Some(ModelsInput::Seq(seq)) => seq,
        Some(ModelsInput::One(single)) => vec![single],
        None => Vec::new(),
    })
}

fn deserialize_optional_u64_number<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::Number(number) => {
            if let Some(int_value) = number.as_u64() {
                return Ok(Some(int_value));
            }
            if let Some(float_value) = number.as_f64()
                && float_value.is_finite()
                && float_value >= 0.0
                && float_value.fract() == 0.0
                && float_value <= u64::MAX as f64
            {
                return Ok(Some(float_value as u64));
            }
            Err(de::Error::custom(format!(
                "expected context_budget_tokens to be a non-negative integer, got {number}. {CONTEXT_FILE_BUDGET_GUIDANCE}"
            )))
        }
        other => Err(de::Error::custom(format!(
            "expected context_budget_tokens to be an integer token budget, got {other}. {CONTEXT_FILE_BUDGET_GUIDANCE}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::Agent;
    use super::AgentCreateOptions;
    use super::AgentManager;
    use super::AgentExecutionOutput;
    use super::AgentProviderFailureClass;
    use super::AgentRetryMetadata;
    use super::AgentStatus;
    use super::AGENT_PROVIDER_RETRY_MAX_DELAY;
    use super::create_agent_tool;
    use super::MAX_AGENT_PROGRESS_ENTRIES;
    use super::MAX_AGENT_RESULT_BYTES;
    use super::MAX_TRACKED_TERMINAL_AGENTS;
    use super::classify_agent_provider_failure;
    use super::normalize_agent_name;
    use super::maybe_set_gemini_config_dir;
    use super::execute_model_with_permissions;
    use super::execute_agent_provider_with_retries;
    use super::extract_agent_token_count;
    use super::resolve_program_path;
    use super::should_use_current_exe_for_agent;
    use super::prefer_json_result;
    use super::prefer_json_result_detailed;
    use super::remove_review_output_json;
    use super::current_code_binary_path;
    use super::agent_retry_delay;
    use super::build_agent_full_prompt;
    use super::build_context_files_prompt;
    use super::AGENT_MANAGER;
    use crate::config_types::AgentConfig;
    use crate::openai_tools::{JsonSchema, OpenAiTool};
    use code_protocol::config_types::ReasoningEffort;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use tempfile::tempdir;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration as StdDuration;
    use uuid::Uuid;

    #[cfg(unix)]
    static STDIN_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(unix)]
    struct StdinRedirectGuard {
        saved_stdin_fd: i32,
        read_fd: i32,
        write_fd: i32,
    }

    #[cfg(unix)]
    impl StdinRedirectGuard {
        fn install_pipe_as_stdin() -> Self {
            let mut fds = [0; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
            let saved_stdin_fd = unsafe { libc::dup(libc::STDIN_FILENO) };
            assert!(saved_stdin_fd >= 0, "dup stdin");
            assert_eq!(unsafe { libc::dup2(fds[0], libc::STDIN_FILENO) }, libc::STDIN_FILENO, "dup2 stdin");
            Self {
                saved_stdin_fd,
                read_fd: fds[0],
                write_fd: fds[1],
            }
        }
    }

    #[cfg(unix)]
    impl Drop for StdinRedirectGuard {
        fn drop(&mut self) {
            unsafe {
                assert_eq!(libc::dup2(self.saved_stdin_fd, libc::STDIN_FILENO), libc::STDIN_FILENO, "restore stdin");
                libc::close(self.saved_stdin_fd);
                libc::close(self.read_fd);
                libc::close(self.write_fd);
            }
        }
    }

    #[test]
    fn drops_empty_names() {
        assert_eq!(normalize_agent_name(None), None);
        assert_eq!(normalize_agent_name(Some("   ".into())), None);
    }

    #[test]
    fn title_cases_and_restores_separators() {
        assert_eq!(
            normalize_agent_name(Some("plan_tui_refactor".into())),
            Some("Plan TUI Refactor".into())
        );
        assert_eq!(
            normalize_agent_name(Some("run-ui-tests".into())),
            Some("Run UI Tests".into())
        );
    }

    #[test]
    fn handles_camel_case_and_acronyms() {
        assert_eq!(
            normalize_agent_name(Some("shipCloudAPI".into())),
            Some("Ship Cloud API".into())
        );
    }

    #[test]
    fn prefer_json_result_uses_json_when_available() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.json");
        let payload = "{\"findings\":[],\"overall_explanation\":\"ok\"}";
        std::fs::write(&path, payload).unwrap();

        let res = prefer_json_result(Some(&path), Err("fallback".to_string()));
        assert_eq!(res.unwrap(), payload);
        assert!(!path.exists(), "review output file should be cleaned up");
    }

    #[test]
    fn prefer_json_result_falls_back_when_missing() {
        let missing = PathBuf::from("/nonexistent/path.json");
        let res = prefer_json_result(Some(&missing), Ok("orig".to_string()));
        assert_eq!(res.unwrap(), "orig");
    }

    #[test]
    fn extracts_tokens_used_from_agent_output() {
        let output = "[2026-06-04T00:24:57] codex\n\nOK\n[2026-06-04T00:24:57] tokens used: 25,915\n";
        assert_eq!(extract_agent_token_count(output), Some(25_915));
        assert_eq!(
            extract_agent_token_count("cumulative tokens used: 5,000, session tokens used: 1,200"),
            Some(5_000)
        );
        assert_eq!(extract_agent_token_count("tokens used: nope"), None);
        assert_eq!(extract_agent_token_count("no usage here"), None);
    }

    #[test]
    fn agent_execution_output_prefers_stdout_token_count() {
        let output = AgentExecutionOutput::from_child_output(
            "answer\ntokens used: 25,915\n".to_string(),
            "tokens used: 0\n",
        );
        assert_eq!(output.token_count, Some(25_915));
    }

    #[test]
    fn prefer_json_result_preserves_token_count_from_failed_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.json");
        let payload = "{\"findings\":[],\"overall_explanation\":\"ok\"}";
        std::fs::write(&path, payload).unwrap();

        let res = prefer_json_result_detailed(
            Some(&path),
            Err("Command failed: review error\ntokens used: 25,915".to_string()),
        )
        .expect("sidecar should win");
        assert_eq!(res.output, payload);
        assert_eq!(res.token_count, Some(25_915));
    }

    #[tokio::test]
    async fn update_agent_result_persists_token_count_for_status() {
        let mut manager = AgentManager::new();
        let session_id = Uuid::new_v4();
        let agent_id = manager
            .create_agent_with_options(
                "code-gpt-5.5".to_string(),
                Some("Token Test".to_string()),
                "prompt".to_string(),
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                true,
                None,
                None,
                session_id,
                None,
                None,
                None,
                None,
                ReasoningEffort::Low,
            )
            .await;

        manager
            .update_agent_result_with_token_count(&agent_id, Ok("done".to_string()), Some(25_915))
            .await;

        let agent = manager.get_agent(&agent_id).expect("agent retained");
        assert_eq!(agent.token_count, Some(25_915));

        let visible = manager.status_visible_agents_for_session(session_id);
        let info = visible
            .iter()
            .find(|agent| agent.id == agent_id)
            .expect("agent visible");
        assert_eq!(info.token_count, Some(25_915));
    }

    #[test]
    fn remove_review_output_json_clears_stale_output() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.json");
        std::fs::write(&path, "stale").unwrap();

        remove_review_output_json(Some(&path));

        assert!(!path.exists(), "stale review output should be removed");
        remove_review_output_json(Some(&path));
    }

    fn agent_with_command(command: &str) -> AgentConfig {
        AgentConfig {
            name: "code-gpt-5.5".to_string(),
            command: command.to_string(),
            args: Vec::new(),
            read_only: false,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        }
    }

    fn test_agent(
        id: &str,
        owner_session_id: Uuid,
        batch_id: &str,
        status: AgentStatus,
    ) -> Agent {
        let now = chrono::Utc::now();
        Agent {
            id: id.to_string(),
            owner_session_id: Some(owner_session_id),
            batch_id: Some(batch_id.to_string()),
            model: "code-gpt-5.5".to_string(),
            name: Some(id.to_string()),
            prompt: "prompt".to_string(),
            context: None,
            output_goal: None,
            files: Vec::new(),
            context_files: Vec::new(),
            context_budget_tokens: None,
            read_only: true,
            status,
            result: None,
            error: None,
            token_count: None,
            retry: AgentRetryMetadata {
                original_model: "code-gpt-5.5".to_string(),
                final_model: "code-gpt-5.5".to_string(),
                ..AgentRetryMetadata::default()
            },
            created_at: now,
            started_at: Some(now),
            completed_at: None,
            progress: Vec::new(),
            worktree_path: None,
            branch_name: None,
            worktree_base: None,
            workspace_root: None,
            source_kind: None,
            log_tag: None,
            config: None,
            reasoning_effort: ReasoningEffort::Low,
            last_activity: now,
        }
    }

    #[test]
    fn code_family_falls_back_when_command_missing() {
        let cfg = agent_with_command("definitely-not-present-429");
        let use_current = should_use_current_exe_for_agent("code", true, Some(&cfg));
        assert!(use_current);
    }

    #[test]
    fn code_family_prefers_current_exe_even_if_coder_in_path() {
        let cfg = agent_with_command("coder");
        let use_current = should_use_current_exe_for_agent("code", false, Some(&cfg));
        assert!(use_current);
    }

    #[test]
    fn code_family_respects_custom_command_override() {
        let cfg = agent_with_command("/usr/local/bin/my-coder");
        let use_current = should_use_current_exe_for_agent("code", false, Some(&cfg));
        assert!(!use_current);
    }

    #[test]
    fn program_path_uses_current_exe_when_requested() {
        let expected = current_code_binary_path().expect("current binary path");
        let resolved = resolve_program_path(true, "coder").expect("resolved program");
        assert_eq!(resolved, expected);

        let custom = resolve_program_path(false, "custom-coder").expect("resolved custom");
        assert_eq!(custom, std::path::PathBuf::from("custom-coder"));
    }

    #[test]
    fn context_files_inline_text_and_preserve_files_as_hints() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("bundle.txt");
        std::fs::write(&file, "alpha beta gamma").expect("write context file");
        std::fs::create_dir(tmp.path().join("src")).expect("create src dir");
        std::fs::write(
            tmp.path().join("src/main.rs"),
            "DO_NOT_INLINE_FILE_HINT_SENTINEL",
        )
        .expect("write hinted file");

        let files = vec!["src/main.rs".to_string()];
        let context_files = vec!["bundle.txt".to_string()];
        let (prompt, summary) = build_agent_full_prompt(
            "Inspect the bundle and report the sentinel.",
            None,
            None,
            None,
            &files,
            &context_files,
            Some(10),
            tmp.path(),
        )
        .expect("prompt built");

        assert!(prompt.contains("Files to consider: src/main.rs"));
        assert!(!prompt.contains("DO_NOT_INLINE_FILE_HINT_SENTINEL"));
        assert!(prompt.contains("<context_file"));
        assert!(prompt.contains("alpha beta gamma"));
        let summary = summary.expect("summary present");
        assert_eq!(summary.included_files, 1);
        assert!(summary.estimated_tokens > 0);
        assert!(summary.estimated_tokens <= summary.budget_tokens);
    }

    #[test]
    fn context_files_fail_when_budget_is_too_small() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("large.txt"), "one two three four five")
            .expect("write context file");

        let err = build_context_files_prompt(
            &["large.txt".to_string()],
            Some(4),
            tmp.path(),
        )
        .expect_err("budget should fail");

        assert!(err.contains("above the remaining budget"));
    }

    #[test]
    fn context_files_default_budget_requires_explicit_large_launch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("rollout.txt"),
            "x".repeat(((super::DEFAULT_CONTEXT_FILE_BUDGET_TOKENS + 1) * 4) as usize),
        )
        .expect("write context file");

        let err = build_context_files_prompt(&["rollout.txt".to_string()], None, tmp.path())
            .expect_err("implicit budget should fail for large context files");

        assert!(err.contains("context_files inline file contents"));
        assert!(err.contains("context_budget_tokens explicitly"));
        assert!(err.contains("code llm request --message-file"));
    }

    #[test]
    fn context_files_explicit_budget_allows_large_launch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("rollout.txt"),
            "x".repeat(((super::DEFAULT_CONTEXT_FILE_BUDGET_TOKENS + 1) * 4) as usize),
        )
        .expect("write context file");

        let prompt = build_context_files_prompt(
            &["rollout.txt".to_string()],
            Some(super::DEFAULT_CONTEXT_FILE_BUDGET_TOKENS + 1),
            tmp.path(),
        )
        .expect("explicit budget should allow large context files")
        .expect("prompt present");

        assert_eq!(prompt.included_files, 1);
        assert_eq!(
            prompt.budget_tokens,
            super::DEFAULT_CONTEXT_FILE_BUDGET_TOKENS + 1
        );
    }

    #[test]
    fn context_files_reject_oversized_file_before_reading() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let large = tmp.path().join("large.txt");
        std::fs::write(&large, "x".repeat(128)).expect("write context file");

        let err = build_context_files_prompt(&["large.txt".to_string()], Some(1), tmp.path())
            .expect_err("oversized file should fail before read");

        assert!(err.contains("above the remaining budget"));
        assert!(err.contains("bytes max"));
    }

    #[test]
    fn context_files_reject_budget_above_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("small.txt"), "ok").expect("write context file");

        let err = build_context_files_prompt(
            &["small.txt".to_string()],
            Some(super::MAX_CONTEXT_FILE_BUDGET_TOKENS + 1),
            tmp.path(),
        )
        .expect_err("oversized budget should fail");

        assert!(err.contains("exceeds the maximum"));
    }

    #[test]
    fn context_budget_schema_documents_integer_valued_number() {
        let tool = create_agent_tool(&[]);
        let function = match tool {
            OpenAiTool::Function(function) => function,
            _ => panic!("agent tool should be a function"),
        };
        let JsonSchema::Object { properties, .. } = function.parameters else {
            panic!("agent tool should have object parameters");
        };
        let create_schema = properties.get("create").expect("create schema");
        let JsonSchema::Object { properties: create_properties, .. } = create_schema else {
            panic!("create schema should be an object");
        };
        let budget_schema = create_properties
            .get("context_budget_tokens")
            .expect("context_budget_tokens schema");

        let JsonSchema::Number { description } = budget_schema else {
            panic!("context_budget_tokens should be a number schema");
        };
        let description = description.as_deref().expect("budget description");
        assert!(description.contains("integer-valued token budget"));
    }

    #[test]
    fn context_budget_parse_error_includes_guidance() {
        let err = serde_json::from_value::<AgentCreateOptions>(serde_json::json!({
            "task": "review rollout",
            "context_budget_tokens": 12.5,
        }))
        .expect_err("fractional budget should be rejected");
        let err = err.to_string();

        assert!(err.contains("non-negative integer"));
        assert!(err.contains("context_files inline file contents"));
        assert!(err.contains("code llm request --message-file"));
    }

    #[test]
    fn context_files_reject_path_escape() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::NamedTempFile::new().expect("outside file");

        let err = build_context_files_prompt(
            &[outside.path().display().to_string()],
            Some(100),
            tmp.path(),
        )
        .expect_err("outside file should fail");

        assert!(err.contains("outside the workspace root"));
    }

    #[test]
    fn context_files_reject_binary_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("blob.bin"), [b'a', 0, b'b']).expect("write binary");

        let err = build_context_files_prompt(
            &["blob.bin".to_string()],
            Some(100),
            tmp.path(),
        )
        .expect_err("binary should fail");

        assert!(err.contains("appears to be binary"));
    }

    #[test]
    fn context_files_escape_path_attributes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let name = "a&b\"c.txt";
        std::fs::write(tmp.path().join(name), "ok").expect("write context file");

        let prompt = build_context_files_prompt(&[name.to_string()], Some(100), tmp.path())
            .expect("prompt should build")
            .expect("summary present");

        assert!(prompt.block.contains("a&amp;b&quot;c.txt"));
    }

    #[tokio::test]
    #[serial]
    async fn create_agent_preserves_explicit_workspace_root_for_context_files() {
        let workspace = tempfile::tempdir().expect("workspace");
        let process_cwd = tempfile::tempdir().expect("process cwd");
        std::fs::write(workspace.path().join("context.txt"), "workspace context")
            .expect("write workspace context");

        let old_cwd = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(process_cwd.path()).expect("set process cwd");
        let mut manager = AgentManager::new();
        let agent_id = manager
            .create_agent_in_workspace(
                "code-gpt-5.5".to_string(),
                Some("workspace-root".to_string()),
                "task".to_string(),
                None,
                None,
                Vec::new(),
                vec!["context.txt".to_string()],
                Some(100),
                true,
                Some("batch".to_string()),
                Uuid::new_v4(),
                Some(workspace.path().to_path_buf()),
                ReasoningEffort::Low,
            )
            .await;
        std::env::set_current_dir(old_cwd).expect("restore cwd");

        let agent = manager.agents.get(&agent_id).expect("agent stored");
        assert_eq!(agent.workspace_root.as_deref(), Some(workspace.path()));
    }

    #[tokio::test]
    async fn agent_status_updates_are_broadcast_to_all_sessions() {
        let mut manager = AgentManager::new();
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();

        manager.set_event_sender(session_a, tx_a);
        manager.set_event_sender(session_b, tx_b);
        manager.send_agent_status_update().await;

        assert!(
            rx_a.try_recv().is_ok(),
            "first session should still receive updates"
        );
        assert!(rx_b.try_recv().is_ok(), "second session should receive updates");
    }

    #[tokio::test]
    async fn agent_status_updates_include_only_owned_agents() {
        let mut manager = AgentManager::new();
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
        let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();

        manager.set_event_sender(session_a, tx_a);
        manager.set_event_sender(session_b, tx_b);
        let agent_a = manager
            .create_agent(
                "code-gpt-5.5".to_string(),
                Some("session-a".to_string()),
                "task a".to_string(),
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                true,
                Some("batch-a".to_string()),
                session_a,
                ReasoningEffort::Low,
            )
            .await;
        let agent_b = manager
            .create_agent(
                "code-gpt-5.5".to_string(),
                Some("session-b".to_string()),
                "task b".to_string(),
                None,
                None,
                Vec::new(),
                Vec::new(),
                None,
                true,
                Some("batch-b".to_string()),
                session_b,
                ReasoningEffort::Low,
            )
            .await;

        let mut payload_a = rx_a
            .try_recv()
            .expect("session A should receive an update");
        while let Ok(next) = rx_a.try_recv() {
            payload_a = next;
        }
        let mut payload_b = rx_b
            .try_recv()
            .expect("session B should receive an update");
        while let Ok(next) = rx_b.try_recv() {
            payload_b = next;
        }

        assert!(payload_a.agents.iter().any(|agent| agent.id == agent_a));
        assert!(!payload_a.agents.iter().any(|agent| agent.id == agent_b));
        assert!(payload_b.agents.iter().any(|agent| agent.id == agent_b));
        assert!(!payload_b.agents.iter().any(|agent| agent.id == agent_a));
    }

    #[tokio::test]
    async fn model_facing_agent_queries_are_session_scoped() {
        let mut manager = AgentManager::new();
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        let shared_batch = "shared-batch";

        manager.agents.insert(
            "agent-a".to_string(),
            test_agent("agent-a", session_a, shared_batch, AgentStatus::Running),
        );
        manager.agents.insert(
            "agent-b".to_string(),
            test_agent("agent-b", session_b, shared_batch, AgentStatus::Running),
        );
        manager.handles.insert("agent-a".to_string(), tokio::spawn(async {}));
        manager.handles.insert("agent-b".to_string(), tokio::spawn(async {}));
        manager.archived_terminal_agents.insert(
            "archived-b".to_string(),
            test_agent("archived-b", session_b, shared_batch, AgentStatus::Completed),
        );
        let mut legacy_agent = test_agent(
            "legacy-agent",
            session_b,
            shared_batch,
            AgentStatus::Running,
        );
        legacy_agent.owner_session_id = None;
        manager
            .agents
            .insert("legacy-agent".to_string(), legacy_agent);

        let visible_to_a = manager.list_agents_for_session(
            None,
            Some(shared_batch.to_string()),
            false,
            session_a,
        );
        assert_eq!(visible_to_a.len(), 2);
        assert!(visible_to_a.iter().any(|agent| agent.id == "agent-a"));
        assert!(visible_to_a.iter().any(|agent| agent.id == "legacy-agent"));
        assert!(manager.get_agent_for_session("agent-b", session_a).is_none());
        assert!(manager.get_agent_for_session("archived-b", session_a).is_none());
        assert!(manager
            .get_agent_for_session("legacy-agent", session_a)
            .is_some());
        assert!(!manager.cancel_agent_for_session("agent-b", session_a).await);

        assert!(manager.agents.contains_key("agent-b"));
        assert_eq!(manager.cancel_batch_for_session(shared_batch, session_a).await, 2);
        assert_eq!(
            manager.agents.get("agent-a").map(|agent| &agent.status),
            Some(&AgentStatus::Cancelled),
        );
        assert_eq!(
            manager
                .agents
                .get("legacy-agent")
                .map(|agent| &agent.status),
            Some(&AgentStatus::Cancelled),
        );
        assert_eq!(
            manager.agents.get("agent-b").map(|agent| &agent.status),
            Some(&AgentStatus::Running),
        );
    }

    #[tokio::test]
    async fn cancel_agent_reaps_stale_active_record_without_handle() {
        let mut manager = AgentManager::new();
        let session_id = Uuid::new_v4();
        manager.agents.insert(
            "stale-agent".to_string(),
            test_agent("stale-agent", session_id, "batch-stale", AgentStatus::Running),
        );

        assert!(manager.has_active_agents());
        assert!(manager.cancel_agent_for_session("stale-agent", session_id).await);
        assert!(!manager.has_active_agents());
        assert_eq!(
            manager
                .agents
                .get("stale-agent")
                .map(|agent| &agent.status),
            Some(&AgentStatus::Cancelled),
        );
        assert!(
            manager
                .agents
                .get("stale-agent")
                .and_then(|agent| agent.completed_at)
                .is_some(),
            "stale close should mark a completion time",
        );
    }

    #[tokio::test]
    async fn agent_status_updates_prune_closed_session_senders() {
        let mut manager = AgentManager::new();
        let session_closed = Uuid::new_v4();
        let session_open = Uuid::new_v4();
        let (tx_closed, rx_closed) = tokio::sync::mpsc::unbounded_channel();
        let (tx_open, mut rx_open) = tokio::sync::mpsc::unbounded_channel();
        drop(rx_closed);

        manager.set_event_sender(session_closed, tx_closed);
        manager.set_event_sender(session_open, tx_open);
        manager.send_agent_status_update().await;

        assert!(rx_open.try_recv().is_ok(), "open session should receive updates");
        assert_eq!(manager.event_senders.len(), 1);
    }

    #[tokio::test]
    async fn registering_second_session_keeps_existing_archived_agents() {
        let mut manager = AgentManager::new();
        let session_a = Uuid::new_v4();
        let (tx_a, _rx_a) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(session_a, tx_a);

        let now = chrono::Utc::now();
        let archived_id = "archived-agent".to_string();
        manager.archived_terminal_agents.insert(
            archived_id.clone(),
            Agent {
                id: archived_id.clone(),
                owner_session_id: Some(session_a),
                batch_id: Some("batch-archived".to_string()),
                model: "code-gpt-5.5".to_string(),
                name: Some("Archived".to_string()),
                prompt: String::new(),
                context: None,
                output_goal: None,
                files: Vec::new(),
                context_files: Vec::new(),
                context_budget_tokens: None,
                read_only: true,
                status: AgentStatus::Completed,
                result: Some("ok".to_string()),
                error: None,
                token_count: None,
                retry: AgentRetryMetadata {
                    original_model: "code-gpt-5.5".to_string(),
                    final_model: "code-gpt-5.5".to_string(),
                    ..AgentRetryMetadata::default()
                },
                created_at: now,
                started_at: Some(now),
                completed_at: Some(now),
                progress: Vec::new(),
                worktree_path: None,
                branch_name: None,
                worktree_base: None,
                workspace_root: None,
                source_kind: None,
                log_tag: None,
                config: None,
                reasoning_effort: ReasoningEffort::Low,
                last_activity: now,
            },
        );

        let (tx_b, _rx_b) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(Uuid::new_v4(), tx_b);

        assert!(manager.archived_terminal_agents.contains_key(&archived_id));
    }

    #[tokio::test]
    async fn reconnect_after_sender_gap_keeps_existing_archived_agents() {
        let mut manager = AgentManager::new();
        let session_id = Uuid::new_v4();
        let (tx_a, rx_a) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(session_id, tx_a);
        drop(rx_a);

        let archived_id = "archived-after-gap".to_string();
        manager.archived_terminal_agents.insert(
            archived_id.clone(),
            test_agent(
                &archived_id,
                session_id,
                "batch-archived-gap",
                AgentStatus::Completed,
            ),
        );

        let (tx_b, _rx_b) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(session_id, tx_b);

        assert!(manager.archived_terminal_agents.contains_key(&archived_id));
    }

    #[tokio::test]
    async fn fresh_sender_set_drops_archives_from_other_sessions() {
        let mut manager = AgentManager::new();
        let old_session = Uuid::new_v4();
        let new_session = Uuid::new_v4();
        let (tx_old, rx_old) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(old_session, tx_old);
        drop(rx_old);

        manager.archived_terminal_agents.insert(
            "old-archived".to_string(),
            test_agent(
                "old-archived",
                old_session,
                "batch-old-archived",
                AgentStatus::Completed,
            ),
        );
        manager.archived_terminal_agents.insert(
            "new-archived".to_string(),
            test_agent(
                "new-archived",
                new_session,
                "batch-new-archived",
                AgentStatus::Completed,
            ),
        );

        let (tx_new, _rx_new) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(new_session, tx_new);

        assert!(manager.archived_terminal_agents.contains_key("new-archived"));
        assert!(!manager.archived_terminal_agents.contains_key("old-archived"));
        assert_eq!(manager.diagnostics.archived_terminal_agents, 1);
    }

    #[tokio::test]
    async fn registering_sender_prunes_archives_from_disconnected_sessions() {
        let mut manager = AgentManager::new();
        let connected_session = Uuid::new_v4();
        let disconnected_session = Uuid::new_v4();
        let new_session = Uuid::new_v4();
        let (tx_connected, _rx_connected) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(connected_session, tx_connected);

        manager.archived_terminal_agents.insert(
            "connected-archived".to_string(),
            test_agent(
                "connected-archived",
                connected_session,
                "batch-connected-archived",
                AgentStatus::Completed,
            ),
        );
        manager.archived_terminal_agents.insert(
            "disconnected-archived".to_string(),
            test_agent(
                "disconnected-archived",
                disconnected_session,
                "batch-disconnected-archived",
                AgentStatus::Completed,
            ),
        );
        let mut legacy_archived = test_agent(
            "legacy-archived",
            disconnected_session,
            "batch-legacy-archived",
            AgentStatus::Completed,
        );
        legacy_archived.owner_session_id = None;
        manager
            .archived_terminal_agents
            .insert("legacy-archived".to_string(), legacy_archived);

        let (tx_new, _rx_new) = tokio::sync::mpsc::unbounded_channel();
        manager.set_event_sender(new_session, tx_new);

        assert!(manager
            .archived_terminal_agents
            .contains_key("connected-archived"));
        assert!(manager
            .archived_terminal_agents
            .contains_key("legacy-archived"));
        assert!(!manager
            .archived_terminal_agents
            .contains_key("disconnected-archived"));
        assert_eq!(manager.diagnostics.archived_terminal_agents, 2);
    }

    #[tokio::test]
    async fn read_only_agents_use_code_binary_path() {
        let _lock = env_lock().lock().expect("env lock");
        let _reset_path = EnvReset::capture("PATH");
        let _reset_binary = EnvReset::capture("CODE_BINARY_PATH");

        let dir = tempdir().expect("tempdir");
        let current = script_path(dir.path(), "current");
        let shim = script_path(dir.path(), "coder");

        write_script(&current, "current");
        write_script(&shim, "path");

        unsafe {
            std::env::set_var("CODE_BINARY_PATH", &current);
            std::env::set_var("PATH", prepend_path(dir.path()));
        }

        let output = execute_model_with_permissions(
            "agent-test",
            "code-gpt-5.5",
            "ok",
            true,
            None,
            None,
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute read-only agent");

        assert_eq!(output.trim(), "current");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_only_agents_redirect_stdin_away_from_parent_pipe() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _stdin_lock = STDIN_LOCK.lock().expect("stdin lock");
        let _reset_binary = EnvReset::capture("CODE_BINARY_PATH");

        let dir = tempdir().expect("tempdir");
        let current = script_path(dir.path(), "current");
        write_stdin_mode_script(&current);

        let _stdin_guard = StdinRedirectGuard::install_pipe_as_stdin();

        unsafe {
            std::env::set_var("CODE_BINARY_PATH", &current);
        }

        let output = execute_model_with_permissions(
            "agent-test",
            "code-gpt-5.5",
            "ok",
            true,
            None,
            None,
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute read-only agent");

        assert_eq!(output.trim(), "detached");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_code_agent_prompt_is_delivered_over_stdin() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _reset_binary = EnvReset::capture("CODE_BINARY_PATH");

        let dir = tempdir().expect("tempdir");
        let current = script_path(dir.path(), "current");
        write_large_prompt_probe_script(&current);

        unsafe {
            std::env::set_var("CODE_BINARY_PATH", &current);
        }

        let prompt = "x".repeat(super::AGENT_PROMPT_STDIN_THRESHOLD_BYTES + 1);
        let output = execute_model_with_permissions(
            "agent-test",
            "code-gpt-5.4",
            &prompt,
            true,
            None,
            None,
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute read-only agent");

        assert_eq!(
            output.trim(),
            format!("prompt_arg=- stdin_len={}", prompt.len())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_code_agent_prompt_streams_output_while_writing_stdin() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _reset_binary = EnvReset::capture("CODE_BINARY_PATH");

        let dir = tempdir().expect("tempdir");
        let current = script_path(dir.path(), "current");
        write_stdout_before_stdin_probe_script(&current);

        unsafe {
            std::env::set_var("CODE_BINARY_PATH", &current);
        }

        let prompt = "x".repeat(super::AGENT_PROMPT_STDIN_THRESHOLD_BYTES + 1);
        let output = tokio::time::timeout(
            StdDuration::from_secs(5),
            execute_model_with_permissions(
                "agent-test",
                "code-gpt-5.4",
                &prompt,
                true,
                None,
                None,
                ReasoningEffort::Low,
                None,
                None,
                None,
            ),
        )
        .await
        .expect("large prompt execution should not deadlock")
        .expect("execute read-only agent");

        assert!(output.contains(&format!("stdin_len={}", prompt.len())));
    }

    #[tokio::test]
    async fn large_external_agent_prompt_fails_before_spawn() {
        let dir = tempdir().expect("tempdir");
        let copilot = script_path(dir.path(), "copilot");
        write_argv_script(&copilot);

        let cfg = AgentConfig {
            name: "github-copilot".to_string(),
            command: copilot.display().to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };
        let prompt = "x".repeat(super::AGENT_PROMPT_STDIN_THRESHOLD_BYTES + 1);

        let err = execute_model_with_permissions(
            "agent-test",
            "github-copilot",
            &prompt,
            true,
            None,
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect_err("large external prompt should fail");

        assert!(err.contains("argv delivery threshold"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_custom_code_command_prompt_uses_stdin() {
        let dir = tempdir().expect("tempdir");
        let custom_code = script_path(dir.path(), "custom-code");
        write_large_prompt_probe_script(&custom_code);

        let cfg = AgentConfig {
            name: "code-gpt-5.4".to_string(),
            command: custom_code.display().to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };
        let prompt = "x".repeat(super::AGENT_PROMPT_STDIN_THRESHOLD_BYTES + 1);

        let output = execute_model_with_permissions(
            "agent-test",
            "code-gpt-5.4",
            &prompt,
            true,
            None,
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("large custom code prompt should use stdin");

        assert_eq!(
            output.trim(),
            format!("prompt_arg=- stdin_len={}", prompt.len())
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[tokio::test]
    async fn claude_agent_uses_local_install_when_not_on_path() {
        let _lock = env_lock().lock().expect("env lock");
        let _reset_path = EnvReset::capture("PATH");
        let _reset_home = EnvReset::capture("HOME");

        let dir = tempdir().expect("tempdir");
        let claude_dir = dir.path().join(".claude").join("local");
        std::fs::create_dir_all(&claude_dir).expect("create claude dir");
        let claude_script = claude_dir.join("claude");
        write_script(&claude_script, "local-claude");

        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("PATH", "/usr/bin:/bin");
        }

        let cfg = AgentConfig {
            name: "claude-sonnet-4.6".to_string(),
            command: "claude".to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "claude-sonnet-4.6",
            "ok",
            true,
            None,
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute claude with local fallback");

        assert_eq!(output.trim(), "local-claude");
    }

    #[tokio::test]
    async fn github_copilot_launches_with_agent_mode_flags() {
        let dir = tempdir().expect("tempdir");
        let copilot = script_path(dir.path(), "copilot");
        write_argv_script(&copilot);

        let cfg = AgentConfig {
            name: "github-copilot".to_string(),
            command: copilot.display().to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "github-copilot",
            "hello from copilot",
            true,
            None,
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute copilot agent");

        let args: Vec<&str> = output.trim().split('|').collect();
        assert_eq!(
            args,
            vec![
                "--autopilot",
                "--allow-all-tools",
                "--no-ask-user",
                "-s",
                "--reasoning-effort",
                "low",
                "-p",
                "hello from copilot",
            ]
        );
    }

    #[tokio::test]
    async fn github_copilot_write_mode_uses_yolo() {
        let dir = tempdir().expect("tempdir");
        let copilot = script_path(dir.path(), "copilot");
        write_argv_script(&copilot);

        let cfg = AgentConfig {
            name: "github-copilot".to_string(),
            command: copilot.display().to_string(),
            args: Vec::new(),
            read_only: false,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "github-copilot",
            "hello from copilot",
            false,
            None,
            Some(cfg),
            ReasoningEffort::High,
            None,
            None,
            None,
        )
        .await
        .expect("execute copilot write agent");

        let args: Vec<&str> = output.trim().split('|').collect();
        assert_eq!(
            args,
            vec![
                "--autopilot",
                "--yolo",
                "--no-ask-user",
                "-s",
                "--reasoning-effort",
                "high",
                "-p",
                "hello from copilot",
            ]
        );
    }

    #[tokio::test]
    async fn antigravity_receives_workspace_add_dir() {
        let dir = tempdir().expect("tempdir");
        let agy = script_path(dir.path(), "agy");
        write_argv_script(&agy);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let cfg = AgentConfig {
            name: "antigravity".to_string(),
            command: agy.display().to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "antigravity",
            "hello from agy",
            true,
            Some(workspace.clone()),
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute antigravity agent");

        let args: Vec<&str> = output.trim().split('|').collect();
        assert_eq!(
            args,
            vec![
                "--add-dir",
                workspace.to_str().expect("workspace path utf-8"),
                "-p",
                "hello from agy",
            ]
        );
    }

    #[tokio::test]
    async fn antigravity_runs_from_private_launch_dir() {
        let _lock = env_lock().lock().expect("env lock");
        let _reset_code_home = EnvReset::capture("CODE_HOME");
        let _reset_home = EnvReset::capture("HOME");

        let dir = tempdir().expect("tempdir");
        let code_home = dir.path().join("code-home");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&code_home).expect("create code home");
        std::fs::create_dir_all(&home).expect("create home");

        unsafe {
            std::env::set_var("CODE_HOME", &code_home);
            std::env::set_var("HOME", &home);
        }

        let agy = script_path(dir.path(), "agy");
        write_cwd_and_argv_script(&agy);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let cfg = AgentConfig {
            name: "antigravity".to_string(),
            command: agy.display().to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "antigravity",
            "hello from agy",
            true,
            Some(workspace.clone()),
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute antigravity agent");

        let expected_cwd = code_home
            .join("agent-cache")
            .join("antigravity")
            .canonicalize()
            .expect("canonical launch dir");
        let expected_cwd_line = format!("cwd={}", expected_cwd.display());
        let expected_args_line = format!(
            "args=--add-dir|{}|-p|hello from agy",
            workspace.to_str().expect("workspace path utf-8")
        );
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some(expected_cwd_line.as_str()));
        assert_eq!(lines.next(), Some(expected_args_line.as_str()));
    }

    #[tokio::test]
    async fn gemini_selector_uses_antigravity_cli() {
        let _lock = env_lock().lock().expect("env lock");
        let _reset_path = EnvReset::capture("PATH");

        let dir = tempdir().expect("tempdir");
        let agy = script_path(dir.path(), "agy");
        write_argv_script(&agy);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        unsafe {
            std::env::set_var("PATH", prepend_path(dir.path()));
        }

        let output = execute_model_with_permissions(
            "agent-test",
            "gemini",
            "hello from google lane",
            true,
            Some(workspace.clone()),
            None,
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute gemini selector through antigravity");

        let args: Vec<&str> = output.trim().split('|').collect();
        assert_eq!(
            args,
            vec![
                "--add-dir",
                workspace.to_str().expect("workspace path utf-8"),
                "-p",
                "hello from google lane",
            ]
        );
    }

    #[tokio::test]
    async fn explicit_gemini_command_keeps_gemini_cli_args() {
        let _lock = env_lock().lock().expect("env lock");
        let _reset_path = EnvReset::capture("PATH");

        let dir = tempdir().expect("tempdir");
        let gemini = script_path(dir.path(), "gemini");
        write_argv_script(&gemini);
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        unsafe {
            std::env::set_var("PATH", prepend_path(dir.path()));
        }

        let cfg = AgentConfig {
            name: "corp-gemini".to_string(),
            command: "gemini".to_string(),
            args: Vec::new(),
            read_only: true,
            enabled: true,
            description: None,
            env: None,
            args_read_only: None,
            args_write: None,
            instructions: None,
        };

        let output = execute_model_with_permissions(
            "agent-test",
            "corp-gemini",
            "hello from gemini cli",
            true,
            Some(workspace),
            Some(cfg),
            ReasoningEffort::Low,
            None,
            None,
            None,
        )
        .await
        .expect("execute explicit gemini CLI config");

        let args: Vec<&str> = output.trim().split('|').collect();
        assert_eq!(args, vec!["-p", "hello from gemini cli"]);
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvReset {
        key: &'static str,
        value: Option<OsString>,
    }

    impl EnvReset {
        fn capture(key: &'static str) -> Self {
            let value = std::env::var_os(key);
            Self { key, value }
        }
    }

    impl Drop for EnvReset {
        fn drop(&mut self) {
            unsafe {
                match &self.value {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn prepend_path(dir: &Path) -> OsString {
        let original = std::env::var_os("PATH");
        let mut parts: Vec<OsString> = Vec::new();
        parts.push(dir.as_os_str().to_os_string());
        if let Some(orig) = original {
            parts.extend(std::env::split_paths(&orig).map(|p| p.into_os_string()));
        }
        std::env::join_paths(parts).expect("join PATH")
    }

    #[cfg(target_os = "windows")]
    fn script_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.cmd"))
    }

    #[cfg(not(target_os = "windows"))]
    fn script_path(dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    #[cfg(target_os = "windows")]
    fn write_script(path: &Path, marker: &str) {
        let script = format!("@echo off\r\necho {marker}\r\nexit /b 0\r\n");
        std::fs::write(path, script).expect("write cmd");
    }

    #[cfg(not(target_os = "windows"))]
    fn write_script(path: &Path, marker: &str) {
        let script = format!("#!/bin/sh\necho {marker}\nexit 0\n");
        std::fs::write(path, script).expect("write script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[cfg(target_os = "windows")]
    fn write_argv_script(path: &Path) {
        let script = "@echo off\r\nsetlocal enabledelayedexpansion\r\nset out=\r\n:loop\r\nif \"%~1\"==\"\" goto done\r\nif defined out (set out=!out!|%~1) else (set out=%~1)\r\nshift\r\ngoto loop\r\n:done\r\necho %out%\r\nexit /b 0\r\n";
        std::fs::write(path, script).expect("write argv cmd");
    }

    #[cfg(not(target_os = "windows"))]
    fn write_argv_script(path: &Path) {
        let script = "#!/bin/sh\nprintf '%s' \"$1\"\nshift\nfor arg in \"$@\"; do\n  printf '|%s' \"$arg\"\ndone\nprintf '\\n'\nexit 0\n";
        std::fs::write(path, script).expect("write argv script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[cfg(target_os = "windows")]
    fn write_cwd_and_argv_script(path: &Path) {
        let script = "@echo off\r\nsetlocal enabledelayedexpansion\r\necho cwd=%CD%\r\nset out=\r\n:loop\r\nif \"%~1\"==\"\" goto done\r\nif defined out (set out=!out!|%~1) else (set out=%~1)\r\nshift\r\ngoto loop\r\n:done\r\necho args=%out%\r\nexit /b 0\r\n";
        std::fs::write(path, script).expect("write cwd argv cmd");
    }

    #[cfg(not(target_os = "windows"))]
    fn write_cwd_and_argv_script(path: &Path) {
        let script = "#!/bin/sh\nprintf 'cwd=%s\\n' \"$PWD\"\nprintf 'args=%s' \"$1\"\nshift\nfor arg in \"$@\"; do\n  printf '|%s' \"$arg\"\ndone\nprintf '\\n'\nexit 0\n";
        std::fs::write(path, script).expect("write cwd argv script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[cfg(unix)]
    fn write_stdin_mode_script(path: &Path) {
        let script = r#"#!/bin/sh
python3 -c 'import os, stat; print("fifo" if stat.S_ISFIFO(os.fstat(0).st_mode) else "detached")'
exit 0
"#;
        std::fs::write(path, script).expect("write stdin mode script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[cfg(unix)]
    fn write_large_prompt_probe_script(path: &Path) {
        let script = r#"#!/bin/sh
prompt_arg=""
for arg in "$@"; do
  prompt_arg="$arg"
done
stdin_len=$(wc -c | awk '{print $1}')
printf 'prompt_arg=%s stdin_len=%s\n' "$prompt_arg" "$stdin_len"
exit 0
"#;
        std::fs::write(path, script).expect("write prompt probe script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[cfg(unix)]
    fn write_stdout_before_stdin_probe_script(path: &Path) {
        let script = r#"#!/bin/sh
python3 -c 'print("o" * 200000)'
stdin_len=$(wc -c | awk '{print $1}')
printf 'stdin_len=%s\n' "$stdin_len"
exit 0
"#;
        std::fs::write(path, script).expect("write output probe script");
        let mut perms = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod script");
    }

    #[test]
    fn gemini_config_dir_is_injected_when_missing_api_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gem_dir = tmp.path().join(".gemini");
        std::fs::create_dir_all(&gem_dir).expect("create .gemini");

        let mut env: HashMap<String, String> = HashMap::new();
        maybe_set_gemini_config_dir(&mut env, Some(tmp.path().to_string_lossy().to_string()));

        assert_eq!(
            env.get("GEMINI_CONFIG_DIR"),
            Some(&gem_dir.to_string_lossy().to_string())
        );
    }

    #[test]
    fn gemini_config_dir_not_overwritten_when_api_key_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("GEMINI_API_KEY".to_string(), "abc".to_string());

        maybe_set_gemini_config_dir(&mut env, Some(tmp.path().to_string_lossy().to_string()));

        assert!(!env.contains_key("GEMINI_CONFIG_DIR"));
    }

    #[test]
    fn prune_terminal_agents_caps_completed_state() {
        let mut manager = AgentManager::new();
        let now = chrono::Utc::now();
        let total = MAX_TRACKED_TERMINAL_AGENTS + 64;

        for idx in 0..total {
            let id = format!("agent-{idx}");
            manager.agents.insert(
                id.clone(),
                Agent {
                    id,
                    owner_session_id: None,
                    batch_id: Some("batch-1".to_string()),
                    model: "code-gpt-5.5".to_string(),
                    name: Some("Prune Test".to_string()),
                    prompt: "prompt".repeat(256),
                    context: Some("ctx".repeat(256)),
                    output_goal: Some("goal".repeat(256)),
                    files: vec!["a".repeat(256)],
                    context_files: vec!["context".repeat(128)],
                    context_budget_tokens: Some(10_000),
                    read_only: false,
                    status: AgentStatus::Completed,
                    result: Some("result".repeat(1024)),
                    error: None,
                    token_count: None,
                    retry: AgentRetryMetadata {
                        original_model: "code-gpt-5.5".to_string(),
                        final_model: "code-gpt-5.5".to_string(),
                        ..AgentRetryMetadata::default()
                    },
                    created_at: now,
                    started_at: Some(now),
                    completed_at: Some(now + chrono::Duration::seconds(idx as i64)),
                    progress: vec!["progress".repeat(1024)],
                    worktree_path: Some("/tmp/wt".to_string()),
                    branch_name: Some("code-branch".to_string()),
                    worktree_base: None,
                    workspace_root: None,
                    source_kind: None,
                    log_tag: None,
                    config: None,
                    reasoning_effort: ReasoningEffort::Low,
                    last_activity: now,
                },
            );
        }

        manager.prune_terminal_agents();

        assert!(manager.agents.len() <= MAX_TRACKED_TERMINAL_AGENTS);
        assert!(
            manager
                .agents
                .values()
                .all(|agent| !agent.worktree_path.as_deref().unwrap_or_default().is_empty()),
            "worktree metadata should be retained for remaining terminal agents"
        );
    }

    #[test]
    fn finalize_terminal_agent_compacts_heavy_fields_and_keeps_worktree() {
        let mut manager = AgentManager::new();
        let now = chrono::Utc::now();
        let agent_id = "agent-finalize".to_string();

        manager.agents.insert(
            agent_id.clone(),
            Agent {
                id: agent_id.clone(),
                owner_session_id: None,
                batch_id: Some("batch-compact".to_string()),
                model: "code-gpt-5.5".to_string(),
                name: Some("Finalize".to_string()),
                prompt: "prompt".repeat(1024),
                context: Some("context".repeat(1024)),
                output_goal: Some("goal".repeat(1024)),
                files: vec!["file".repeat(1024)],
                context_files: vec!["context-file".repeat(1024)],
                context_budget_tokens: Some(10_000),
                read_only: false,
                status: AgentStatus::Completed,
                result: Some("result".repeat(32 * 1024)),
                error: None,
                token_count: None,
                retry: AgentRetryMetadata {
                    original_model: "code-gpt-5.5".to_string(),
                    final_model: "code-gpt-5.5".to_string(),
                    ..AgentRetryMetadata::default()
                },
                created_at: now,
                started_at: Some(now),
                completed_at: Some(now),
                progress: (0..(MAX_AGENT_PROGRESS_ENTRIES + 20))
                    .map(|idx| format!("progress-entry-{idx}-{}", "x".repeat(512)))
                    .collect(),
                worktree_path: Some("/tmp/wt-stays".to_string()),
                branch_name: Some("branch-stays".to_string()),
                worktree_base: None,
                workspace_root: None,
                source_kind: None,
                log_tag: None,
                config: None,
                reasoning_effort: ReasoningEffort::Low,
                last_activity: now,
            },
        );

        manager.finalize_terminal_agent(&agent_id);

        let agent = manager
            .agents
            .get(&agent_id)
            .expect("agent should still be tracked");
        assert!(agent.prompt.is_empty());
        assert!(agent.context.is_none());
        assert!(agent.output_goal.is_none());
        assert!(agent.files.is_empty());
        assert!(agent.context_files.is_empty());
        assert!(agent.context_budget_tokens.is_none());
        assert_eq!(agent.worktree_path.as_deref(), Some("/tmp/wt-stays"));
        assert_eq!(agent.branch_name.as_deref(), Some("branch-stays"));
        assert!(agent.progress.len() <= MAX_AGENT_PROGRESS_ENTRIES);

        let result_len = agent
            .result
            .as_ref()
            .map(String::len)
            .expect("result retained");
        assert!(
            result_len <= MAX_AGENT_RESULT_BYTES + 4,
            "result should be compacted to bounded size"
        );
    }

    #[test]
    fn pruned_terminal_agent_remains_queryable_for_session() {
        let mut manager = AgentManager::new();
        let now = chrono::Utc::now();
        let batch_id = "batch-session".to_string();
        let total = MAX_TRACKED_TERMINAL_AGENTS + 8;

        for idx in 0..total {
            let id = format!("agent-{idx}");
            manager.agents.insert(
                id.clone(),
                Agent {
                    id,
                    owner_session_id: None,
                    batch_id: Some(batch_id.clone()),
                    model: "code-gpt-5.5".to_string(),
                    name: Some("Archive Test".to_string()),
                    prompt: "prompt".to_string(),
                    context: None,
                    output_goal: None,
                    files: Vec::new(),
                    context_files: Vec::new(),
                    context_budget_tokens: None,
                    read_only: false,
                    status: AgentStatus::Completed,
                    result: Some("ok".to_string()),
                    error: None,
                    token_count: None,
                    retry: AgentRetryMetadata {
                        original_model: "code-gpt-5.5".to_string(),
                        final_model: "code-gpt-5.5".to_string(),
                        ..AgentRetryMetadata::default()
                    },
                    created_at: now,
                    started_at: Some(now),
                    completed_at: Some(now + chrono::Duration::seconds(idx as i64)),
                    progress: vec!["progress".to_string()],
                    worktree_path: Some(format!("/tmp/worktree-{idx}")),
                    branch_name: Some(format!("code-branch-{idx}")),
                    worktree_base: None,
                    workspace_root: None,
                    source_kind: None,
                    log_tag: None,
                    config: None,
                    reasoning_effort: ReasoningEffort::Low,
                    last_activity: now,
                },
            );
        }

        manager.prune_terminal_agents();

        let archived_id = "agent-0";
        assert!(manager.agents.get(archived_id).is_none());
        let archived = manager
            .get_agent(archived_id)
            .expect("pruned agent should remain queryable");
        assert_eq!(archived.worktree_path.as_deref(), Some("/tmp/worktree-0"));
        assert_eq!(archived.branch_name.as_deref(), Some("code-branch-0"));

        let listed = manager.list_agents(None, Some(batch_id), false);
        assert!(listed.iter().any(|agent| {
            agent.id == archived_id
                && agent.worktree_path.as_deref() == Some("/tmp/worktree-0")
                && agent.branch_name.as_deref() == Some("code-branch-0")
        }));
    }

    #[test]
    fn classifies_retryable_agent_provider_failures() {
        for error in [
            "API Error: Overloaded",
            "HTTP 429: too many requests",
            "request timed out while waiting for provider",
            "upstream service unavailable",
            "stream disconnected: connection reset",
            "broken pipe while reading response",
        ] {
            assert_eq!(
                classify_agent_provider_failure(error),
                AgentProviderFailureClass::Retryable,
                "expected retryable: {error}"
            );
        }
    }

    #[test]
    fn classifies_non_retryable_agent_provider_failures() {
        for error in [
            "unauthorized: invalid token",
            "permission denied by provider",
            "Agent 'claude' could not be found.",
            "invalid config: missing command",
            "run cancelled by user",
            "policy violation",
        ] {
            assert_eq!(
                classify_agent_provider_failure(error),
                AgentProviderFailureClass::NonRetryable,
                "expected non-retryable: {error}"
            );
        }
    }

    #[test]
    fn retry_delay_is_bounded_exponential_backoff() {
        assert_eq!(agent_retry_delay(0), StdDuration::from_secs(2));
        assert_eq!(agent_retry_delay(1), StdDuration::from_secs(4));
        assert_eq!(agent_retry_delay(2), StdDuration::from_secs(8));
        assert_eq!(agent_retry_delay(8), AGENT_PROVIDER_RETRY_MAX_DELAY);
    }

    #[test]
    fn agent_tool_models_description_guides_specialized_delegation() {
        let tool = create_agent_tool(&[
            "code-gpt-5.5".to_string(),
            "claude-sonnet-4.6".to_string(),
            "claude-fable-5".to_string(),
            "antigravity".to_string(),
        ]);

        let function = match tool {
            OpenAiTool::Function(function) => function,
            _ => panic!("agent tool should be a function"),
        };
        let JsonSchema::Object { properties, .. } = function.parameters else {
            panic!("agent tool should have object parameters");
        };
        let create_schema = properties.get("create").expect("create schema");
        let JsonSchema::Object { properties: create_properties, .. } = create_schema else {
            panic!("create schema should be an object");
        };
        let models_schema = create_properties.get("models").expect("models schema");
        let JsonSchema::Array { items, description } = models_schema else {
            panic!("models schema should be an array");
        };
        let description = description.as_deref().expect("models description");
        assert!(description.contains("diverse families"));
        assert!(description.contains("proactively use `antigravity` for Google/Gemini-family perspective"));
        assert!(description.contains("release/workflow, infrastructure, security, or product-risk"));
        assert!(description.contains("unless there is a clear reason to skip it"));
        assert!(description.contains("AGY uses its configured model"));
        assert!(description.contains("Fable restriction"));
        assert!(description.contains(FABLE_AGENT_GUIDANCE));

        let JsonSchema::String { allowed_values, .. } = items.as_ref() else {
            panic!("models items should be strings");
        };
        let values = allowed_values.as_ref().expect("allowed models");
        assert!(values.contains(&"antigravity".to_string()));
        assert!(values.contains(&"claude-fable-5".to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn agent_provider_retry_wrapper_recovers_from_transient_failure() {
        let agent_id = "retry-success".to_string();
        let owner_session_id = Uuid::new_v4();
        let mut manager = AgentManager::new();
        manager.agents.insert(
            agent_id.clone(),
            test_agent(
                &agent_id,
                owner_session_id,
                "batch-retry",
                AgentStatus::Running,
            ),
        );

        let old_manager = {
            let mut global = AGENT_MANAGER.write().await;
            std::mem::replace(&mut *global, manager)
        };

        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_run = attempts.clone();
        let result = execute_agent_provider_with_retries(&agent_id, "code-gpt-5.5", || {
            let attempts_for_run = attempts_for_run.clone();
            async move {
                let attempt = attempts_for_run.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    Err("API Error: Overloaded".to_string())
                } else {
                    Ok(AgentExecutionOutput::new("ok".to_string(), Some(123)))
                }
            }
        })
        .await;

        let output = result.expect("retry should eventually succeed");
        assert_eq!(output.output, "ok");
        assert_eq!(output.token_count, Some(123));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        let manager = {
            let mut global = AGENT_MANAGER.write().await;
            std::mem::replace(&mut *global, old_manager)
        };
        let agent = manager
            .agents
            .get(&agent_id)
            .expect("agent should remain tracked");
        assert_eq!(agent.retry.retry_count, 1);
        assert_eq!(
            agent.retry.last_retryable_error.as_deref(),
            Some("API Error: Overloaded")
        );
        assert!(
            agent
                .progress
                .iter()
                .any(|line| line.contains("Retrying code-gpt-5.5"))
        );
    }

    #[tokio::test]
    async fn agent_provider_retry_wrapper_does_not_retry_non_retryable_failure() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let attempts_for_run = attempts.clone();
        let result = execute_agent_provider_with_retries("missing-agent", "claude", || {
            let attempts_for_run = attempts_for_run.clone();
            async move {
                attempts_for_run.fetch_add(1, Ordering::SeqCst);
                Err("Agent 'claude' could not be found.".to_string())
            }
        })
        .await;

        assert_eq!(
            result.as_ref().map(|output| output.output.as_str()).map_err(String::as_str),
            Err("Agent 'claude' could not be found.")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckAgentStatusParams {
    pub agent_id: String,
    pub batch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetAgentResultParams {
    pub agent_id: String,
    pub batch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAgentParams {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitForAgentParams {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub return_all: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAgentsParams {
    pub status_filter: Option<String>,
    pub batch_id: Option<String>,
    pub recent_only: Option<bool>,
}
