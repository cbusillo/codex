use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::gh_run_wait_spec::DEFAULT_POLL_INTERVAL_SECONDS;
use crate::tools::handlers::gh_run_wait_spec::DEFAULT_TIMEOUT_SECONDS;
use crate::tools::handlers::gh_run_wait_spec::GH_RUN_WAIT_TOOL_NAME;
use crate::tools::handlers::gh_run_wait_spec::MAX_TIMEOUT_SECONDS;
use crate::tools::handlers::gh_run_wait_spec::create_gh_run_wait_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_utils_pty::process_group::kill_child_process_group;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const MAX_DIAGNOSTIC_LENGTH: usize = 1_000;

pub struct GhRunWaitHandler;

#[derive(Debug, Deserialize)]
struct GhRunWaitArgs {
    #[serde(default)]
    run_id: Option<Value>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    head_sha: Option<String>,
    #[serde(default)]
    interval_seconds: Option<u64>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Debug)]
struct PreparedWait {
    run_id: Option<String>,
    repo: Option<String>,
    workflow: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
    interval_seconds: u64,
    timeout_seconds: u64,
}

struct WaitOutcome {
    text: String,
    success: bool,
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

enum CommandFailure {
    Cancelled,
    TimedOut,
    Execution(String),
}

impl ToolHandler for GhRunWaitHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(GH_RUN_WAIT_TOOL_NAME)
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(create_gh_run_wait_tool())
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            turn,
            payload,
            cancellation_token,
            ..
        } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "gh_run_wait handler received unsupported payload".to_string(),
                ));
            }
        };
        let prepared = prepare_wait(parse_arguments(&arguments)?)?;
        let outcome = wait_for_github_run(&turn.cwd, prepared, &cancellation_token).await?;
        Ok(FunctionToolOutput::from_text(
            outcome.text,
            Some(outcome.success),
        ))
    }
}

async fn wait_for_github_run(
    cwd: &Path,
    prepared: PreparedWait,
    cancellation_token: &CancellationToken,
) -> Result<WaitOutcome, FunctionCallError> {
    let deadline = Instant::now() + Duration::from_secs(prepared.timeout_seconds);
    let mut messages = Vec::new();
    let branch = if prepared.run_id.is_none() {
        Some(
            resolve_branch(
                cwd,
                prepared.branch.as_deref(),
                deadline,
                cancellation_token,
            )
            .await?,
        )
    } else {
        prepared.branch.clone()
    };
    let run_id = if let Some(run_id) = prepared.run_id.clone() {
        run_id
    } else {
        match select_run(
            cwd,
            prepared.repo.as_deref(),
            prepared.workflow.as_deref(),
            branch.as_deref().unwrap_or("main"),
            prepared.head_sha.as_deref(),
            deadline,
            cancellation_token,
        )
        .await
        {
            Ok(run_id) => run_id,
            Err(CommandFailure::TimedOut) => {
                return Ok(timeout_outcome(prepared.timeout_seconds, None));
            }
            Err(CommandFailure::Cancelled) => return Ok(cancelled_outcome()),
            Err(CommandFailure::Execution(message)) => {
                return Err(FunctionCallError::RespondToModel(message));
            }
        }
    };

    messages.push(format!(
        "Waiting for GitHub Actions run {run_id} (timeout: {} seconds).",
        prepared.timeout_seconds
    ));
    if prepared.run_id.is_none() {
        let workflow = prepared.workflow.as_deref().unwrap_or("latest workflow");
        messages.push(format!(
            "Selected {workflow} on branch '{}'.",
            branch.as_deref().unwrap_or("main")
        ));
    }

    let mut last_status = String::new();
    let mut last_jobs_snapshot = String::new();
    let mut last_run_url = String::new();

    loop {
        let run_view_args = vec![
            "run".to_string(),
            "view".to_string(),
            run_id.clone(),
            "--json".to_string(),
            "status,conclusion,displayTitle,workflowName,headBranch,url,jobs".to_string(),
        ];
        let output = match run_gh(
            cwd,
            prepared.repo.as_deref(),
            &run_view_args,
            deadline,
            cancellation_token,
        )
        .await
        {
            Ok(output) => output,
            Err(CommandFailure::TimedOut) => {
                return Ok(timeout_outcome_with_url(
                    prepared.timeout_seconds,
                    Some(&run_id),
                    &last_run_url,
                ));
            }
            Err(CommandFailure::Cancelled) => return Ok(cancelled_outcome()),
            Err(CommandFailure::Execution(message)) => {
                return Err(FunctionCallError::RespondToModel(message));
            }
        };

        if !output.status.success() {
            messages.push(format!(
                "Failed to fetch run {run_id}; retrying. {}",
                bounded_diagnostic(&output.stderr)
            ));
            match sleep_until_next_poll(
                deadline,
                prepared.interval_seconds,
                cancellation_token,
            )
            .await
            {
                PollSleep::Continue => continue,
                PollSleep::TimedOut => {
                    return Ok(timeout_outcome_with_url(
                        prepared.timeout_seconds,
                        Some(&run_id),
                        &last_run_url,
                    ));
                }
                PollSleep::Cancelled => return Ok(cancelled_outcome()),
            }
        }

        let run_payload: Value = serde_json::from_str(&output.stdout).map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "gh_run_wait could not parse GitHub run {run_id}: {error}"
            ))
        })?;
        let status = json_string(&run_payload, "status").unwrap_or_else(|| "unknown".to_string());
        let conclusion = json_string(&run_payload, "conclusion").unwrap_or_default();
        let workflow_name = json_string(&run_payload, "workflowName")
            .unwrap_or_else(|| "unknown workflow".to_string());
        let display_title = json_string(&run_payload, "displayTitle")
            .unwrap_or_else(|| "no title".to_string());
        let branch_name = json_string(&run_payload, "headBranch")
            .unwrap_or_else(|| "unknown branch".to_string());
        let run_url = json_string(&run_payload, "url").unwrap_or_default();
        if !run_url.is_empty() {
            last_run_url.clone_from(&run_url);
        }

        if status != last_status {
            let conclusion_suffix = if conclusion.is_empty() {
                String::new()
            } else {
                format!(", conclusion: {conclusion}")
            };
            messages.push(format!(
                "[{workflow_name}] {display_title} on branch '{branch_name}' -> status: {status}{conclusion_suffix}"
            ));
            if !run_url.is_empty() {
                messages.push(run_url.clone());
            }
            last_status.clone_from(&status);
        }

        if status == "waiting" {
            append_waiting_diagnostics(
                cwd,
                prepared.repo.as_deref(),
                &run_id,
                &run_url,
                deadline,
                cancellation_token,
                &mut messages,
            )
            .await;
            return Ok(WaitOutcome {
                text: messages.join("\n"),
                success: false,
            });
        }

        let jobs = run_payload
            .get("jobs")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let jobs_snapshot = jobs
            .iter()
            .map(job_snapshot)
            .collect::<Vec<_>>()
            .join("\n");
        if jobs_snapshot != last_jobs_snapshot {
            if !jobs.is_empty() {
                messages.push("Job summary:".to_string());
                messages.extend(jobs.iter().map(|job| format!("  - {}", job_summary(job))));
            }
            last_jobs_snapshot = jobs_snapshot;
        }

        let failing_jobs = jobs
            .iter()
            .filter(|job| job_failed(job))
            .map(|job| json_string(job, "name").unwrap_or_else(|| "unnamed job".to_string()))
            .collect::<Vec<_>>();
        if !failing_jobs.is_empty() {
            messages.push(format!(
                "Run {run_id} has failing job(s): {}.",
                failing_jobs.join(", ")
            ));
            return Ok(WaitOutcome {
                text: messages.join("\n"),
                success: false,
            });
        }

        if status == "completed" {
            if conclusion == "success" {
                messages.push(format!("Run {run_id} succeeded."));
                return Ok(WaitOutcome {
                    text: messages.join("\n"),
                    success: true,
                });
            }
            messages.push(format!(
                "Run {run_id} finished with conclusion '{}'.",
                if conclusion.is_empty() {
                    "unknown"
                } else {
                    &conclusion
                }
            ));
            return Ok(WaitOutcome {
                text: messages.join("\n"),
                success: false,
            });
        }

        match sleep_until_next_poll(deadline, prepared.interval_seconds, cancellation_token).await {
            PollSleep::Continue => {}
            PollSleep::TimedOut => {
                return Ok(timeout_outcome_with_url(
                    prepared.timeout_seconds,
                    Some(&run_id),
                    &last_run_url,
                ));
            }
            PollSleep::Cancelled => return Ok(cancelled_outcome()),
        }
    }
}

async fn select_run(
    cwd: &Path,
    repo: Option<&str>,
    workflow: Option<&str>,
    branch: &str,
    head_sha: Option<&str>,
    deadline: Instant,
    cancellation_token: &CancellationToken,
) -> Result<String, CommandFailure> {
    let args = build_run_list_args(workflow, branch, head_sha);
    let output = run_gh(cwd, repo, &args, deadline, cancellation_token).await?;
    if !output.status.success() {
        return Err(CommandFailure::Execution(format!(
            "gh_run_wait failed to list GitHub Actions runs: {}",
            bounded_diagnostic(&output.stderr)
        )));
    }
    let payload: Value = serde_json::from_str(&output.stdout).map_err(|error| {
        CommandFailure::Execution(format!(
            "gh_run_wait could not parse the GitHub Actions run list: {error}"
        ))
    })?;
    let run = payload
        .as_array()
        .and_then(|runs| runs.first())
        .ok_or_else(|| {
            let workflow_description = workflow.unwrap_or("any workflow");
            let sha_description = head_sha
                .map(|sha| format!(" at commit '{sha}'"))
                .unwrap_or_default();
            CommandFailure::Execution(format!(
                "gh_run_wait found no {workflow_description} runs on branch '{branch}'{sha_description}."
            ))
        })?;
    if let Some(expected_sha) = head_sha {
        let actual_sha = json_string(run, "headSha").unwrap_or_default();
        if !actual_sha.eq_ignore_ascii_case(expected_sha) {
            return Err(CommandFailure::Execution(format!(
                "gh_run_wait selected a run whose commit did not match '{expected_sha}'."
            )));
        }
    }
    json_identifier(run.get("databaseId")).ok_or_else(|| {
        CommandFailure::Execution(
            "gh_run_wait selected a run without a valid numeric run id.".to_string(),
        )
    })
}

fn build_run_list_args(
    workflow: Option<&str>,
    branch: &str,
    head_sha: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["run".to_string(), "list".to_string()];
    if let Some(workflow) = workflow {
        args.extend(["--workflow".to_string(), workflow.to_string()]);
    }
    args.extend(["--branch".to_string(), branch.to_string()]);
    if let Some(head_sha) = head_sha {
        args.extend(["--commit".to_string(), head_sha.to_string()]);
    }
    args.extend([
        "--limit".to_string(),
        "1".to_string(),
        "--json".to_string(),
        "databaseId,workflowName,displayTitle,headBranch,headSha".to_string(),
    ]);
    args
}

async fn resolve_branch(
    cwd: &Path,
    requested_branch: Option<&str>,
    deadline: Instant,
    cancellation_token: &CancellationToken,
) -> Result<String, FunctionCallError> {
    if let Some(branch) = requested_branch {
        return Ok(branch.to_string());
    }
    for args in [
        vec![
            "rev-parse".to_string(),
            "--abbrev-ref".to_string(),
            "HEAD".to_string(),
        ],
        vec![
            "symbolic-ref".to_string(),
            "--quiet".to_string(),
            "--short".to_string(),
            "refs/remotes/origin/HEAD".to_string(),
        ],
    ] {
        let output = match run_command("git", &args, cwd, deadline, cancellation_token).await {
            Ok(output) => output,
            Err(CommandFailure::TimedOut) => {
                return Err(FunctionCallError::RespondToModel(
                    "gh_run_wait timed out while resolving the current branch.".to_string(),
                ));
            }
            Err(CommandFailure::Cancelled) => {
                return Err(FunctionCallError::RespondToModel(
                    "gh_run_wait was cancelled while resolving the current branch.".to_string(),
                ));
            }
            Err(CommandFailure::Execution(_)) => continue,
        };
        if output.status.success() {
            let branch = output.stdout.trim().trim_start_matches("origin/");
            if !branch.is_empty() && branch != "HEAD" {
                return Ok(branch.to_string());
            }
        }
    }
    Ok("main".to_string())
}

async fn append_waiting_diagnostics(
    cwd: &Path,
    requested_repo: Option<&str>,
    run_id: &str,
    run_url: &str,
    deadline: Instant,
    cancellation_token: &CancellationToken,
    messages: &mut Vec<String>,
) {
    messages.push(format!(
        "Run {run_id} requires attention: GitHub reports status 'waiting'."
    ));
    if !run_url.is_empty() {
        messages.push(run_url.to_string());
    }

    let repo = if let Some(repo) = requested_repo {
        Some(repo.to_string())
    } else {
        let args = vec![
            "repo".to_string(),
            "view".to_string(),
            "--json".to_string(),
            "nameWithOwner".to_string(),
            "--jq".to_string(),
            ".nameWithOwner".to_string(),
        ];
        match run_command("gh", &args, cwd, deadline, cancellation_token).await {
            Ok(output) if output.status.success() && !output.stdout.trim().is_empty() => {
                Some(output.stdout.trim().to_string())
            }
            _ => None,
        }
    };

    if let Some(repo) = repo {
        let args = vec![
            "api".to_string(),
            format!("repos/{repo}/actions/runs/{run_id}/pending_deployments"),
        ];
        match run_command("gh", &args, cwd, deadline, cancellation_token).await {
            Ok(output) if output.status.success() => {
                append_pending_deployments(&output.stdout, run_id, &repo, messages);
            }
            Ok(output) => messages.push(format!(
                "Unable to read pending deployments for exact run {run_id} in {repo}: {}",
                bounded_diagnostic(&output.stderr)
            )),
            Err(CommandFailure::TimedOut) => messages.push(
                "Timed out while reading pending deployments for the exact run.".to_string(),
            ),
            Err(CommandFailure::Cancelled) => {
                messages.push("Pending-deployment inspection was cancelled.".to_string())
            }
            Err(CommandFailure::Execution(message)) => messages.push(message),
        }
    } else {
        messages.push(
            "Unable to resolve the repository, so pending deployments could not be inspected."
                .to_string(),
        );
    }

    messages.push("gh_run_wait does not approve protected environments.".to_string());
    messages.push(format!(
        "Use the exact-run workflow babysitter for run {run_id} with separate automation-dispatch and human-review identities."
    ));
}

fn append_pending_deployments(
    payload: &str,
    run_id: &str,
    repo: &str,
    messages: &mut Vec<String>,
) {
    let Ok(Value::Array(pending_deployments)) = serde_json::from_str::<Value>(payload) else {
        messages.push(format!(
            "GitHub returned an unexpected pending-deployments response for exact run {run_id} in {repo}."
        ));
        return;
    };
    if pending_deployments.is_empty() {
        messages.push(
            "GitHub reported no pending deployments; the wait may be a concurrency, queue, or custom protection-rule gate."
                .to_string(),
        );
        return;
    }
    messages.push("Pending protected environment deployment(s):".to_string());
    for pending in pending_deployments {
        let environment_name = pending
            .get("environment")
            .and_then(|environment| json_string(environment, "name"))
            .unwrap_or_else(|| "unnamed environment".to_string());
        let can_approve = match pending.get("current_user_can_approve").and_then(Value::as_bool) {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        messages.push(format!(
            "  - {}: current GitHub CLI identity can approve: {can_approve}",
            single_line(&environment_name)
        ));
    }
}

async fn run_gh(
    cwd: &Path,
    repo: Option<&str>,
    args: &[String],
    deadline: Instant,
    cancellation_token: &CancellationToken,
) -> Result<CommandOutput, CommandFailure> {
    let mut gh_args = Vec::new();
    if let Some(repo) = repo {
        gh_args.extend(["-R".to_string(), repo.to_string()]);
    }
    gh_args.extend_from_slice(args);
    run_command("gh", &gh_args, cwd, deadline, cancellation_token).await
}

async fn run_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    deadline: Instant,
    cancellation_token: &CancellationToken,
) -> Result<CommandOutput, CommandFailure> {
    if Instant::now() >= deadline {
        return Err(CommandFailure::TimedOut);
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|error| {
        CommandFailure::Execution(format!("gh_run_wait could not start {program}: {error}"))
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| {
        CommandFailure::Execution(format!("gh_run_wait could not capture {program} stdout"))
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        CommandFailure::Execution(format!("gh_run_wait could not capture {program} stderr"))
    })?;
    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });

    enum CommandExit {
        Completed(Result<ExitStatus, std::io::Error>),
        TimedOut,
        Cancelled,
    }

    let command_exit = tokio::select! {
        status = child.wait() => CommandExit::Completed(status),
        _ = tokio::time::sleep_until(deadline) => CommandExit::TimedOut,
        _ = cancellation_token.cancelled() => CommandExit::Cancelled,
    };
    let interruption = match command_exit {
        CommandExit::Completed(_) => None,
        CommandExit::TimedOut => Some(CommandFailure::TimedOut),
        CommandExit::Cancelled => Some(CommandFailure::Cancelled),
    };
    if interruption.is_some() {
        let _ = kill_child_process_group(&mut child);
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    let stdout = stdout_task
        .await
        .map_err(|error| CommandFailure::Execution(format!("stdout task failed: {error}")))?
        .map_err(|error| CommandFailure::Execution(format!("stdout read failed: {error}")))?;
    let stderr = stderr_task
        .await
        .map_err(|error| CommandFailure::Execution(format!("stderr task failed: {error}")))?
        .map_err(|error| CommandFailure::Execution(format!("stderr read failed: {error}")))?;
    if let Some(interruption) = interruption {
        return Err(interruption);
    }
    let CommandExit::Completed(status) = command_exit else {
        unreachable!("interrupted commands return before status handling");
    };
    let status = status.map_err(|error| {
        CommandFailure::Execution(format!("gh_run_wait could not wait for {program}: {error}"))
    })?;
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

enum PollSleep {
    Continue,
    TimedOut,
    Cancelled,
}

async fn sleep_until_next_poll(
    deadline: Instant,
    interval_seconds: u64,
    cancellation_token: &CancellationToken,
) -> PollSleep {
    let now = Instant::now();
    if now >= deadline {
        return PollSleep::TimedOut;
    }
    let next_poll = std::cmp::min(
        deadline,
        now + Duration::from_secs(interval_seconds),
    );
    tokio::select! {
        _ = tokio::time::sleep_until(next_poll) => {
            if Instant::now() >= deadline {
                PollSleep::TimedOut
            } else {
                PollSleep::Continue
            }
        }
        _ = cancellation_token.cancelled() => PollSleep::Cancelled,
    }
}

fn prepare_wait(args: GhRunWaitArgs) -> Result<PreparedWait, FunctionCallError> {
    let interval_seconds = args
        .interval_seconds
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
    let timeout_seconds = args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    if interval_seconds == 0 {
        return Err(FunctionCallError::RespondToModel(
            "gh_run_wait interval_seconds must be greater than zero".to_string(),
        ));
    }
    if timeout_seconds == 0 || timeout_seconds > MAX_TIMEOUT_SECONDS {
        return Err(FunctionCallError::RespondToModel(format!(
            "gh_run_wait timeout_seconds must be greater than zero and at most {MAX_TIMEOUT_SECONDS}"
        )));
    }
    if interval_seconds > timeout_seconds {
        return Err(FunctionCallError::RespondToModel(
            "gh_run_wait interval_seconds must not exceed timeout_seconds".to_string(),
        ));
    }

    let repo = normalize_optional_string(args.repo);
    if let Some(repo) = repo.as_deref()
        && !valid_repo(repo)
    {
        return Err(FunctionCallError::RespondToModel(
            "gh_run_wait repo must use OWNER/REPO format".to_string(),
        ));
    }
    Ok(PreparedWait {
        run_id: normalize_run_id(args.run_id)?,
        repo,
        workflow: normalize_optional_string(args.workflow),
        branch: normalize_optional_string(args.branch),
        head_sha: normalize_optional_string(args.head_sha),
        interval_seconds,
        timeout_seconds,
    })
}

fn normalize_run_id(value: Option<Value>) -> Result<Option<String>, FunctionCallError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let run_id = match value {
        Value::String(value) => value.trim().to_string(),
        Value::Number(value) => value.as_u64().map(|value| value.to_string()).ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "gh_run_wait run_id must be a positive integer".to_string(),
            )
        })?,
        Value::Null => return Ok(None),
        _ => {
            return Err(FunctionCallError::RespondToModel(
                "gh_run_wait run_id must be a string or positive integer".to_string(),
            ));
        }
    };
    if run_id.is_empty() {
        return Ok(None);
    }
    if !run_id.bytes().all(|byte| byte.is_ascii_digit()) || run_id.starts_with('0') {
        return Err(FunctionCallError::RespondToModel(
            "gh_run_wait run_id must be a positive integer".to_string(),
        ));
    }
    Ok(Some(run_id))
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty()
                && !name.is_empty()
                && !owner.chars().any(char::is_whitespace)
                && !name.chars().any(char::is_whitespace)
    )
}

fn json_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|field| match field {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn json_identifier(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::Number(value)) => value.as_u64().map(|value| value.to_string()),
        Some(Value::String(value))
            if !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && !value.starts_with('0') =>
        {
            Some(value.clone())
        }
        _ => None,
    }
}

fn job_snapshot(job: &Value) -> String {
    format!(
        "{}|{}|{}",
        json_string(job, "name").unwrap_or_else(|| "unnamed job".to_string()),
        json_string(job, "status").unwrap_or_default(),
        json_string(job, "conclusion").unwrap_or_default()
    )
}

fn job_summary(job: &Value) -> String {
    let name = json_string(job, "name").unwrap_or_else(|| "unnamed job".to_string());
    let status = json_string(job, "status").unwrap_or_else(|| "unknown".to_string());
    let conclusion = json_string(job, "conclusion").unwrap_or_default();
    if status == "completed" && !conclusion.is_empty() {
        format!("{name}: {status} ({conclusion})")
    } else {
        format!("{name}: {status}")
    }
}

fn job_failed(job: &Value) -> bool {
    if json_string(job, "status").as_deref() != Some("completed") {
        return false;
    }
    !matches!(
        json_string(job, "conclusion").as_deref(),
        None | Some("") | Some("success") | Some("skipped") | Some("neutral")
    )
}

fn timeout_outcome(timeout_seconds: u64, run_id: Option<&str>) -> WaitOutcome {
    timeout_outcome_with_url(timeout_seconds, run_id, "")
}

fn timeout_outcome_with_url(
    timeout_seconds: u64,
    run_id: Option<&str>,
    run_url: &str,
) -> WaitOutcome {
    let subject = run_id
        .map(|run_id| format!("run {run_id}"))
        .unwrap_or_else(|| "run selection".to_string());
    let mut text = format!(
        "gh_run_wait timed out after {timeout_seconds} seconds while waiting for {subject}."
    );
    if !run_url.is_empty() {
        text.push('\n');
        text.push_str(run_url);
    }
    text.push_str("\nRetry the same exact run or use the exact-run workflow babysitter.");
    WaitOutcome {
        text,
        success: false,
    }
}

fn cancelled_outcome() -> WaitOutcome {
    WaitOutcome {
        text: "gh_run_wait was cancelled; its active child process was terminated and reaped."
            .to_string(),
        success: false,
    }
}

fn bounded_diagnostic(value: &str) -> String {
    let normalized = single_line(value.trim());
    if normalized.is_empty() {
        return "No diagnostic was returned.".to_string();
    }
    let mut characters = normalized.chars();
    let bounded = characters
        .by_ref()
        .take(MAX_DIAGNOSTIC_LENGTH)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\r' | '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prepare_wait_preserves_exact_run_and_timeout() {
        let args: GhRunWaitArgs = serde_json::from_value(json!({
            "run_id": 4242,
            "repo": "example/repo",
            "head_sha": "abc123",
            "interval_seconds": 3,
            "timeout_seconds": 30
        }))
        .expect("valid args");
        let prepared = prepare_wait(args).expect("prepared args");
        assert_eq!(prepared.run_id.as_deref(), Some("4242"));
        assert_eq!(prepared.repo.as_deref(), Some("example/repo"));
        assert_eq!(prepared.head_sha.as_deref(), Some("abc123"));
        assert_eq!(prepared.interval_seconds, 3);
        assert_eq!(prepared.timeout_seconds, 30);
    }

    #[test]
    fn prepare_wait_rejects_unbounded_timeout() {
        let args: GhRunWaitArgs = serde_json::from_value(json!({
            "timeout_seconds": MAX_TIMEOUT_SECONDS + 1
        }))
        .expect("valid JSON");
        assert!(prepare_wait(args).is_err());
    }

    #[test]
    fn exact_sha_selection_uses_github_commit_filter() {
        let args = build_run_list_args(Some("CI"), "main", Some("abc123"));
        assert_eq!(
            args,
            vec![
                "run",
                "list",
                "--workflow",
                "CI",
                "--branch",
                "main",
                "--commit",
                "abc123",
                "--limit",
                "1",
                "--json",
                "databaseId,workflowName,displayTitle,headBranch,headSha",
            ]
        );
    }

    #[test]
    fn pending_deployment_diagnostics_include_approval_capability() {
        let mut messages = Vec::new();
        append_pending_deployments(
            r#"[{"environment":{"name":"production"},"current_user_can_approve":false}]"#,
            "4242",
            "example/repo",
            &mut messages,
        );
        assert_eq!(
            messages,
            vec![
                "Pending protected environment deployment(s):",
                "  - production: current GitHub CLI identity can approve: no",
            ]
        );
    }
}
