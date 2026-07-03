use std::time::Duration;

use crate::code_bridge::BridgeControlAction;
use crate::code_bridge::BridgeControlOutcome;
use crate::code_bridge::BridgeControlRequest;
use crate::code_bridge::SUBSCRIPTION_FILE;
use crate::code_bridge::collect_events;
use crate::code_bridge::discover_bridge_targets;
use crate::code_bridge::format_events_transcript;
use crate::code_bridge::load_subscription;
use crate::code_bridge::request_control;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::code_bridge_spec::CODE_BRIDGE_TOOL_NAME;
use crate::tools::handlers::code_bridge_spec::create_code_bridge_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use chrono::Utc;
use codex_protocol::models::DEFAULT_IMAGE_DETAIL;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::openai_models::InputModality;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use tokio::fs;

const DEFAULT_CONTROL_TIMEOUT_MS: u64 = 5_000;
const CONTROL_RESULT_TIMEOUT_BUFFER_MS: u64 = 1_000;
const DEFAULT_COLLECT_TIMEOUT_MS: u64 = 2_000;
const DEFAULT_MAX_EVENTS: usize = 10;
const MAX_EVENTS_LIMIT: usize = 100;

pub struct CodeBridgeHandler;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodeBridgeAction {
    Subscribe,
    Collect,
    Navigate,
    Screenshot,
    Javascript,
}

#[derive(Debug, Deserialize)]
struct CodeBridgeArgs {
    action: CodeBridgeAction,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_events: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CodeBridgeToolOutput {
    ok: bool,
    message: String,
    delivered: Option<usize>,
    result: Option<Value>,
    screenshot: Option<CodeBridgeScreenshotOutput>,
}

#[derive(Debug, Serialize)]
struct CodeBridgeScreenshotOutput {
    mime: String,
    data_len: usize,
    #[serde(skip)]
    data: Option<String>,
}

impl ToolHandler for CodeBridgeHandler {
    type Output = FunctionToolOutput;

    fn tool_name(&self) -> ToolName {
        ToolName::plain(CODE_BRIDGE_TOOL_NAME)
    }

    fn spec(&self) -> Option<ToolSpec> {
        Some(create_code_bridge_tool())
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation { turn, payload, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "code_bridge handler received unsupported payload".to_string(),
                ));
            }
        };
        let args: CodeBridgeArgs = parse_arguments(&arguments)?;

        let supports_image = turn.model_info.input_modalities.contains(&InputModality::Image);
        let output = match args.action {
            CodeBridgeAction::Subscribe => subscribe(&turn.cwd, args.level).await?,
            CodeBridgeAction::Collect => {
                collect(&turn.cwd, args.max_events, args.timeout_ms).await?
            }
            CodeBridgeAction::Navigate => {
                let Some(url) = args
                    .url
                    .map(|url| url.trim().to_string())
                    .filter(|url| !url.is_empty())
                else {
                    return Err(FunctionCallError::RespondToModel(
                        "code_bridge navigate action requires non-empty `url`".to_string(),
                    ));
                };
                control(
                    &turn.cwd,
                    BridgeControlAction::Navigate,
                    Some(url),
                    None,
                    args.timeout_ms,
                    supports_image,
                )
                .await?
            }
            CodeBridgeAction::Screenshot => {
                control(
                    &turn.cwd,
                    BridgeControlAction::Screenshot,
                    None,
                    None,
                    args.timeout_ms,
                    supports_image,
                )
                .await?
            }
            CodeBridgeAction::Javascript => {
                let Some(code) = args.code.filter(|code| !code.trim().is_empty()) else {
                    return Err(FunctionCallError::RespondToModel(
                        "code_bridge javascript action requires non-empty `code`".to_string(),
                    ));
                };
                control(
                    &turn.cwd,
                    BridgeControlAction::Javascript,
                    None,
                    Some(code),
                    args.timeout_ms,
                    supports_image,
                )
                .await?
            }
        };

        Ok(output.into_function_output()?)
    }
}

impl CodeBridgeToolOutput {
    fn into_function_output(self) -> Result<FunctionToolOutput, FunctionCallError> {
        let success = Some(self.ok);
        let screenshot_data_url = self
            .screenshot
            .as_ref()
            .and_then(|screenshot| screenshot.data_url());
        let text = serde_json::to_string(&self).map_err(|err| {
            FunctionCallError::Fatal(format!("failed to serialize code_bridge output: {err}"))
        })?;
        if let Some(image_url) = screenshot_data_url {
            Ok(FunctionToolOutput::from_content(
                vec![
                    FunctionCallOutputContentItem::InputText { text },
                    FunctionCallOutputContentItem::InputImage {
                        image_url,
                        detail: Some(DEFAULT_IMAGE_DETAIL),
                    },
                ],
                success,
            ))
        } else {
            Ok(FunctionToolOutput::from_text(text, success))
        }
    }
}

async fn subscribe(
    cwd: &std::path::Path,
    level: Option<String>,
) -> Result<CodeBridgeToolOutput, FunctionCallError> {
    let workspace = bridge_workspace(cwd).await.map_err(respond_error)?;
    let mut subscription = load_subscription(&workspace).await.map_err(respond_error)?;
    if let Some(level) = level {
        let mut level = level.trim().to_lowercase();
        if level == "error" {
            level = "errors".to_string();
        }
        if !matches!(level.as_str(), "errors" | "warn" | "info" | "trace") {
            return Err(FunctionCallError::RespondToModel(format!(
                "unsupported code_bridge level `{level}`; expected errors, warn, info, trace, or error"
            )));
        }
        subscription.levels = vec![level];
    }
    subscription.capabilities = vec![
        "console".to_string(),
        "control".to_string(),
        "error".to_string(),
        "pageview".to_string(),
        "screenshot".to_string(),
    ];
    let subscription = subscription.normalized();
    let path = workspace.join(".code").join(SUBSCRIPTION_FILE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|err| respond_error(err.into()))?;
    }
    let text = serde_json::to_string_pretty(&subscription).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize code_bridge subscription: {err}"))
    })?;
    fs::write(&path, text)
        .await
        .map_err(|err| respond_error(err.into()))?;

    Ok(CodeBridgeToolOutput {
        ok: true,
        message: format!("subscribed to Code Bridge events at {}", path.display()),
        delivered: None,
        result: Some(json!({
            "levels": subscription.levels,
            "capabilities": subscription.capabilities,
            "llmFilter": subscription.llm_filter,
        })),
        screenshot: None,
    })
}

async fn collect(
    cwd: &std::path::Path,
    max_events: Option<usize>,
    timeout_ms: Option<u64>,
) -> Result<CodeBridgeToolOutput, FunctionCallError> {
    let (target, workspace) = fresh_target_and_workspace(cwd).await?;
    let subscription = load_subscription(&workspace).await.map_err(respond_error)?;
    let max_events = max_events.unwrap_or(DEFAULT_MAX_EVENTS).clamp(1, MAX_EVENTS_LIMIT);
    let timeout_ms = timeout_ms
        .unwrap_or(DEFAULT_COLLECT_TIMEOUT_MS)
        .clamp(1, 30_000);
    let events = collect_events(
        &target,
        &subscription,
        max_events,
        Duration::from_millis(timeout_ms),
    )
    .await
    .map_err(respond_error)?;
    let transcript = format_events_transcript(&events);
    let event_count = events.len();

    Ok(CodeBridgeToolOutput {
        ok: true,
        message: format!("collected {event_count} Code Bridge event(s)"),
        delivered: None,
        result: Some(json!({
            "count": event_count,
            "transcript": transcript,
        })),
        screenshot: None,
    })
}

async fn control(
    cwd: &std::path::Path,
    action: BridgeControlAction,
    url: Option<String>,
    code: Option<String>,
    timeout_ms: Option<u64>,
    supports_image: bool,
) -> Result<CodeBridgeToolOutput, FunctionCallError> {
    let (target, workspace) = fresh_target_and_workspace(cwd).await?;
    let subscription = load_subscription(&workspace).await.map_err(respond_error)?;
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_CONTROL_TIMEOUT_MS).clamp(1, 30_000);
    let request = BridgeControlRequest {
        id: format!("code-bridge-{}", Utc::now().timestamp_millis()),
        action,
        url,
        code,
        timeout_ms: Some(timeout_ms),
        expect_result: Some(true),
    };
    let outcome = request_control(
        &target,
        &subscription,
        request,
        Duration::from_millis(timeout_ms.saturating_add(CONTROL_RESULT_TIMEOUT_BUFFER_MS)),
    )
    .await
    .map_err(respond_error)?;

    Ok(output_from_control(action, outcome, supports_image))
}

fn output_from_control(
    action: BridgeControlAction,
    outcome: BridgeControlOutcome,
    supports_image: bool,
) -> CodeBridgeToolOutput {
    CodeBridgeToolOutput {
        ok: outcome.ok,
        message: match action {
            BridgeControlAction::Ping => "Code Bridge ping completed".to_string(),
            BridgeControlAction::Navigate => "Code Bridge navigation completed".to_string(),
            BridgeControlAction::Screenshot => "Code Bridge screenshot completed".to_string(),
            BridgeControlAction::Javascript => "Code Bridge JavaScript completed".to_string(),
        },
        delivered: Some(outcome.delivered),
        result: outcome.result,
        screenshot: outcome
            .screenshot
            .map(|screenshot| CodeBridgeScreenshotOutput {
                mime: screenshot.mime,
                data_len: screenshot.data_len,
                data: screenshot.data.filter(|_| supports_image),
            }),
    }
}

impl CodeBridgeScreenshotOutput {
    fn data_url(&self) -> Option<String> {
        self.data
            .as_ref()
            .map(|data| format!("data:{};base64,{}", self.mime, data))
    }
}

fn respond_error(error: anyhow::Error) -> FunctionCallError {
    FunctionCallError::RespondToModel(error.to_string())
}

async fn bridge_workspace(cwd: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let targets = discover_bridge_targets(cwd).await?;
    Ok(targets
        .iter()
        .find(|target| !target.stale)
        .and_then(target_workspace)
        .unwrap_or_else(|| cwd.to_path_buf()))
}

async fn fresh_target_and_workspace(
    cwd: &std::path::Path,
) -> Result<(crate::code_bridge::BridgeTarget, std::path::PathBuf), FunctionCallError> {
    let targets = discover_bridge_targets(cwd).await.map_err(respond_error)?;
    let Some(target) = targets.into_iter().find(|target| !target.stale) else {
        return Err(FunctionCallError::RespondToModel(
            "no fresh Code Bridge metadata found under this workspace".to_string(),
        ));
    };
    let workspace = target_workspace(&target).ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "invalid Code Bridge metadata path {}",
            target.meta_path.display()
        ))
    })?;
    Ok((target, workspace))
}

fn target_workspace(target: &crate::code_bridge::BridgeTarget) -> Option<std::path::PathBuf> {
    target
        .meta_path
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_bridge::BridgeSubscription;
    use crate::code_bridge::META_FILE;
    use anyhow::Context;
    use anyhow::Result;
    use codex_protocol::models::FunctionCallOutputContentItem;
    use futures::SinkExt;
    use futures::StreamExt;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn subscribe_writes_normalized_subscription_file() -> Result<()> {
        let workspace = TempDir::new()?;
        let output = subscribe(workspace.path(), Some("error".to_string())).await?;

        assert!(output.ok);
        assert_eq!(output.delivered, None);
        assert!(output.screenshot.is_none());

        let subscription = load_subscription(workspace.path()).await?;
        assert_eq!(subscription.levels, vec!["errors".to_string()]);
        assert_eq!(
            subscription.capabilities,
            vec![
                "console".to_string(),
                "control".to_string(),
                "error".to_string(),
                "pageview".to_string(),
                "screenshot".to_string(),
            ]
        );
        assert_eq!(subscription.llm_filter, "off");
        Ok(())
    }

    #[tokio::test]
    async fn subscribe_from_subdirectory_uses_discovered_bridge_workspace() -> Result<()> {
        let workspace = TempDir::new()?;
        let code_dir = workspace.path().join(".code");
        fs::create_dir(&code_dir)?;
        fs::write(
            code_dir.join(META_FILE),
            serde_json::to_string(&json!({
                "url": "ws://127.0.0.1:9",
                "secret": "bridge-secret",
                "workspacePath": workspace.path(),
                "heartbeatAt": Utc::now(),
            }))?,
        )?;
        let nested = workspace.path().join("src").join("feature");
        fs::create_dir_all(&nested)?;

        let output = subscribe(&nested, Some("INFO".to_string())).await?;

        assert!(output.ok);
        assert!(!nested.join(".code").join(SUBSCRIPTION_FILE).exists());
        let subscription = load_subscription(workspace.path()).await?;
        assert_eq!(subscription.levels, vec!["info".to_string()]);
        Ok(())
    }

    #[tokio::test]
    async fn javascript_control_sends_code_bridge_wire_shape() -> Result<()> {
        let fake = FakeBridgeServer::start(FakeBridgeResponse::Javascript).await?;
        let workspace = workspace_with_bridge_meta(&fake)?;

        let output = control(
            workspace.path(),
            BridgeControlAction::Javascript,
            None,
            Some("window.location.href".to_string()),
            Some(2_000),
            false,
        )
        .await?;
        let observed = fake.wait().await?;

        assert!(output.ok);
        assert_eq!(output.delivered, Some(1));
        assert_eq!(output.result, Some(json!({ "href": "app://fixture" })));
        assert!(output.screenshot.is_none());
        assert_eq!(observed.control["type"], "control_request");
        assert_eq!(observed.control["action"], "javascript");
        assert_eq!(observed.control["code"], "window.location.href");
        assert_eq!(observed.control["timeoutMs"], 2_000);
        assert_eq!(observed.control["expectResult"], true);
        assert!(observed.control.get("method").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn navigate_control_sends_url_over_code_bridge_wire_shape() -> Result<()> {
        let fake = FakeBridgeServer::start(FakeBridgeResponse::Navigate).await?;
        let workspace = workspace_with_bridge_meta(&fake)?;
        let target_url = "http://127.0.0.1:4321/local-navigation".to_string();

        let output = control(
            workspace.path(),
            BridgeControlAction::Navigate,
            Some(target_url.clone()),
            None,
            Some(2_000),
            false,
        )
        .await?;
        let observed = fake.wait().await?;

        assert!(output.ok);
        assert_eq!(output.delivered, Some(1));
        assert_eq!(output.result, Some(json!({ "url": target_url })));
        assert!(output.screenshot.is_none());
        assert_eq!(observed.control["type"], "control_request");
        assert_eq!(observed.control["action"], "navigate");
        assert_eq!(observed.control["url"], target_url);
        assert!(observed.control.get("code").is_none());
        assert!(observed.control.get("method").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn screenshot_control_returns_screenshot_summary() -> Result<()> {
        let fake = FakeBridgeServer::start(FakeBridgeResponse::Screenshot).await?;
        let workspace = workspace_with_bridge_meta(&fake)?;

        let output = control(
            workspace.path(),
            BridgeControlAction::Screenshot,
            None,
            None,
            Some(2_000),
            false,
        )
        .await?;
        let observed = fake.wait().await?;

        assert!(output.ok);
        assert_eq!(output.delivered, Some(1));
        assert_eq!(observed.control["type"], "control_request");
        assert_eq!(observed.control["action"], "screenshot");
        let screenshot = output.screenshot.context("screenshot summary")?;
        assert_eq!(screenshot.mime, "image/png");
        assert_eq!(screenshot.data_len, 4);
        assert!(screenshot.data.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn screenshot_control_embeds_image_when_supported() -> Result<()> {
        let fake = FakeBridgeServer::start(FakeBridgeResponse::Screenshot).await?;
        let workspace = workspace_with_bridge_meta(&fake)?;

        let output = control(
            workspace.path(),
            BridgeControlAction::Screenshot,
            None,
            None,
            Some(2_000),
            true,
        )
        .await?
        .into_function_output()?;
        let observed = fake.wait().await?;

        assert_eq!(observed.control["action"], "screenshot");
        assert_eq!(output.success, Some(true));
        assert!(matches!(
            output.body.as_slice(),
            [
                FunctionCallOutputContentItem::InputText { .. },
                FunctionCallOutputContentItem::InputImage { image_url, .. }
            ] if image_url == "data:image/png;base64,AQIDBA=="
        ));
        Ok(())
    }

    #[tokio::test]
    async fn collect_returns_event_transcript() -> Result<()> {
        let fake = FakeBridgeServer::start(FakeBridgeResponse::Events).await?;
        let workspace = workspace_with_bridge_meta(&fake)?;

        let output = collect(workspace.path(), Some(2), Some(2_000)).await?;
        let observed = fake.wait().await?;

        assert!(observed.control.is_null());
        assert!(output.ok);
        let result = output.result.context("collect result")?;
        assert_eq!(result["count"], 2);
        let transcript = result["transcript"]
            .as_str()
            .context("event transcript")?;
        assert!(transcript.contains("pageview page=\"local-page\""));
        assert!(transcript.contains("console page=\"local-page\" level=info"));
        assert!(transcript.contains("local navigation ready"));
        Ok(())
    }

    #[test]
    fn code_bridge_tool_spec_uses_expected_action_schema() -> Result<()> {
        let ToolSpec::Function(spec) = create_code_bridge_tool() else {
            panic!("expected function tool spec");
        };
        assert_eq!(spec.name, CODE_BRIDGE_TOOL_NAME);
        assert_eq!(spec.strict, false);
        let action = spec
            .parameters
            .properties
            .as_ref()
            .and_then(|properties| properties.get("action"))
            .context("action schema")?;
        assert_eq!(
            action.enum_values.as_deref(),
            Some(
                [
                    json!("subscribe"),
                    json!("collect"),
                    json!("navigate"),
                    json!("screenshot"),
                    json!("javascript"),
                ]
                .as_slice()
            )
        );
        Ok::<(), anyhow::Error>(())
    }

    fn workspace_with_bridge_meta(fake: &FakeBridgeServer) -> Result<TempDir> {
        let workspace = TempDir::new()?;
        let code_dir = workspace.path().join(".code");
        fs::create_dir(&code_dir)?;
        fs::write(
            code_dir.join(META_FILE),
            serde_json::to_string(&json!({
                "url": fake.url,
                "secret": fake.secret,
                "workspacePath": workspace.path(),
                "heartbeatAt": Utc::now(),
            }))?,
        )?;
        fs::write(
            code_dir.join(SUBSCRIPTION_FILE),
            serde_json::to_string(&BridgeSubscription::default())?,
        )?;
        Ok(workspace)
    }

    struct FakeBridgeServer {
        url: String,
        secret: String,
        observed_rx: oneshot::Receiver<Result<ObservedBridgeMessages>>,
        task: JoinHandle<()>,
    }

    impl FakeBridgeServer {
        async fn start(response: FakeBridgeResponse) -> Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .context("bind fake bridge server")?;
            let url = format!("ws://{}", listener.local_addr()?);
            let (observed_tx, observed_rx) = oneshot::channel();
            let task = tokio::spawn(async move {
                let observed = accept_one(listener, response).await;
                let _ = observed_tx.send(observed);
            });

            Ok(Self {
                url,
                secret: "bridge-secret".to_string(),
                observed_rx,
                task,
            })
        }

        async fn wait(self) -> Result<ObservedBridgeMessages> {
            let observed = self
                .observed_rx
                .await
                .context("fake bridge server should send observed messages")??;
            self.task.await.context("fake bridge server task should join")?;
            Ok(observed)
        }
    }

    struct ObservedBridgeMessages {
        control: Value,
    }

    #[derive(Clone, Copy)]
    enum FakeBridgeResponse {
        Javascript,
        Navigate,
        Screenshot,
        Events,
    }

    async fn accept_one(
        listener: TcpListener,
        response: FakeBridgeResponse,
    ) -> Result<ObservedBridgeMessages> {
        let (stream, _) = listener.accept().await.context("accept fake bridge client")?;
        let mut websocket = accept_async(stream)
            .await
            .context("accept fake bridge websocket")?;

        let _auth = read_text_json(&mut websocket).await?;
        websocket
            .send(Message::Text(json!({ "type": "auth_success" }).to_string().into()))
            .await
            .context("send auth success")?;

        let _subscribe = read_text_json(&mut websocket).await?;
        websocket
            .send(Message::Text(json!({ "type": "subscribe_ack" }).to_string().into()))
            .await
            .context("send subscribe ack")?;

        if matches!(response, FakeBridgeResponse::Events) {
            send_local_page_events(&mut websocket).await?;
            return Ok(ObservedBridgeMessages {
                control: Value::Null,
            });
        }

        let control = read_text_json(&mut websocket).await?;
        let id = control
            .get("id")
            .and_then(Value::as_str)
            .context("control request should include id")?;
        websocket
            .send(Message::Text(
                json!({ "type": "control_forwarded", "id": id, "delivered": 1 })
                    .to_string()
                    .into(),
            ))
            .await
            .context("send control forwarded")?;

        match response {
            FakeBridgeResponse::Javascript => {
                websocket
                    .send(Message::Text(
                        json!({
                            "type": "control_result",
                            "id": id,
                            "ok": true,
                            "result": { "href": "app://fixture" }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .context("send javascript control result")?;
            }
            FakeBridgeResponse::Navigate => {
                let url = control
                    .get("url")
                    .and_then(Value::as_str)
                    .context("navigate control request should include url")?;
                websocket
                    .send(Message::Text(
                        json!({
                            "type": "control_result",
                            "id": id,
                            "ok": true,
                            "result": { "url": url }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .context("send navigate control result")?;
            }
            FakeBridgeResponse::Screenshot => {
                websocket
                    .send(Message::Text(
                        json!({ "type": "control_result", "id": id, "ok": true })
                            .to_string()
                            .into(),
                    ))
                    .await
                    .context("send screenshot control result")?;
                websocket
                    .send(Message::Text(
                        json!({
                            "type": "screenshot",
                            "id": id,
                            "mime": "image/png",
                            "data": "AQIDBA=="
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .context("send screenshot payload")?;
            }
            FakeBridgeResponse::Events => unreachable!("events mode handled before control read"),
        }

        Ok(ObservedBridgeMessages { control })
    }

    async fn send_local_page_events<S>(websocket: &mut S) -> Result<()>
    where
        S: SinkExt<Message> + Unpin,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        let timestamp = "2026-06-06T12:00:00Z";
        let events = [
            json!({
                "type": "event",
                "kind": "pageview",
                "pageId": "local-page",
                "timestamp": timestamp,
                "url": "http://127.0.0.1/local-navigation",
                "title": "Local Navigation Fixture"
            }),
            json!({
                "type": "event",
                "kind": "console",
                "pageId": "local-page",
                "timestamp": timestamp,
                "level": "info",
                "message": "local navigation ready"
            }),
        ];
        for event in events {
            websocket
                .send(Message::Text(event.to_string().into()))
                .await
                .context("send local page bridge event")?;
        }
        Ok(())
    }

    async fn read_text_json<S>(websocket: &mut S) -> Result<Value>
    where
        S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        while let Some(message) = websocket.next().await {
            match message? {
                Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                Message::Close(_) => anyhow::bail!("fake bridge websocket closed early"),
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        anyhow::bail!("fake bridge websocket ended before text message")
    }
}
