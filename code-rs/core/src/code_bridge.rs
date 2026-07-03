use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

pub const META_FILE: &str = "code-bridge.json";
pub const SUBSCRIPTION_FILE: &str = "code-bridge.subscription.json";
pub const HEARTBEAT_STALE_MS: i64 = 20_000;

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_FORWARDED_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMeta {
    pub url: String,
    pub secret: String,
    pub port: Option<u16>,
    pub workspace_path: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: Option<DateTime<Utc>>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeTarget {
    pub meta_path: PathBuf,
    pub meta: BridgeMeta,
    pub stale: bool,
    pub heartbeat_age_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSubscription {
    #[serde(default = "default_levels")]
    pub levels: Vec<String>,
    #[serde(default = "default_capabilities")]
    pub capabilities: Vec<String>,
    #[serde(default = "default_filter", alias = "llm_filter")]
    pub llm_filter: String,
}

impl Default for BridgeSubscription {
    fn default() -> Self {
        Self {
            levels: default_levels(),
            capabilities: default_capabilities(),
            llm_filter: default_filter(),
        }
    }
}

impl BridgeSubscription {
    pub fn normalized(mut self) -> Self {
        self.levels = normalize_unique(self.levels);
        self.capabilities = normalize_unique(self.capabilities);
        self.llm_filter = self.llm_filter.trim().to_lowercase();
        if self.llm_filter.is_empty() {
            self.llm_filter = default_filter();
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum BridgeClientMessage {
    Auth {
        role: BridgeRole,
        secret: String,
        #[serde(rename = "clientId")]
        client_id: String,
    },
    Subscribe {
        levels: Vec<String>,
        capabilities: Vec<String>,
        #[serde(rename = "llmFilter")]
        llm_filter: String,
    },
    ControlRequest(BridgeControlRequest),
}

impl BridgeClientMessage {
    pub fn is_desktop_remote_control_message(&self) -> bool {
        // Structural boundary check: bridge messages should never gain the
        // JSON-RPC `method` discriminator used by Desktop remote-control.
        serde_json::to_value(self)
            .ok()
            .and_then(|value| {
                value
                    .get("method")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|method| method.starts_with("remoteControl/"))
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BridgeRole {
    Consumer,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeControlRequest {
    pub id: String,
    pub action: BridgeControlAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_result: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BridgeControlAction {
    Ping,
    Navigate,
    Screenshot,
    Javascript,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum BridgeServerMessage {
    AuthSuccess,
    SubscribeAck,
    Event(BridgeEvent),
    ControlForwarded {
        id: String,
        delivered: usize,
    },
    ControlResult {
        id: String,
        ok: bool,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
    },
    Screenshot {
        id: String,
        mime: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BridgeControlOutcome {
    pub delivered: usize,
    pub ok: bool,
    pub result: Option<Value>,
    pub screenshot: Option<BridgeScreenshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeScreenshot {
    pub mime: String,
    pub data_len: usize,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum BridgeEventData {
    Console {
        level: String,
        message: String,
    },
    Error {
        message: String,
        #[serde(default)]
        stack: Option<String>,
    },
    Pageview {
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
    Screenshot {
        mime: String,
        data_len: usize,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEvent {
    pub page_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub data: BridgeEventData,
}

pub async fn discover_bridge_targets(cwd: &Path) -> Result<Vec<BridgeTarget>> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = Some(cwd);

    while let Some(dir) = current {
        let candidate = dir.join(".code").join(META_FILE);
        if seen.insert(candidate.clone()) && candidate.exists() {
            let raw = fs::read_to_string(&candidate)
                .await
                .with_context(|| format!("read bridge metadata at {}", candidate.display()))?;
            let meta: BridgeMeta = serde_json::from_str(&raw)
                .with_context(|| format!("parse bridge metadata at {}", candidate.display()))?;
            let (stale, heartbeat_age_ms) = compute_staleness(&meta, &candidate).await;
            targets.push(BridgeTarget {
                meta_path: candidate,
                meta,
                stale,
                heartbeat_age_ms,
            });
        }
        current = dir.parent();
    }

    Ok(targets)
}

pub async fn load_subscription(workspace: &Path) -> Result<BridgeSubscription> {
    let path = workspace.join(".code").join(SUBSCRIPTION_FILE);
    if !path.exists() {
        return Ok(BridgeSubscription::default().normalized());
    }

    let raw = fs::read_to_string(&path)
        .await
        .with_context(|| format!("read bridge subscription at {}", path.display()))?;
    let subscription: BridgeSubscription = serde_json::from_str(&raw)
        .with_context(|| format!("parse bridge subscription at {}", path.display()))?;
    Ok(subscription.normalized())
}

pub async fn request_control(
    target: &BridgeTarget,
    subscription: &BridgeSubscription,
    request: BridgeControlRequest,
    result_timeout: Duration,
) -> Result<BridgeControlOutcome> {
    let (mut ws, _) = connect_async(&target.meta.url)
        .await
        .with_context(|| format!("connect to bridge {}", target.meta.url))?;

    send_auth_and_subscribe(&mut ws, target, subscription).await?;

    let id = request.id.clone();
    let waits_for_screenshot = request.action == BridgeControlAction::Screenshot;
    let control = BridgeClientMessage::ControlRequest(request);
    if control.is_desktop_remote_control_message() {
        bail!("bridge control request used Desktop remote-control wire shape");
    }
    send_json(&mut ws, &control)
        .await
        .context("send bridge control request")?;

    let forwarded = wait_for_server_message(
        &mut ws,
        |message| {
            matches!(
                message,
                BridgeServerMessage::ControlForwarded {
                    id: forwarded_id,
                    ..
                } if forwarded_id == &id
            )
        },
        CONTROL_FORWARDED_TIMEOUT,
    )
    .await
    .context("wait for bridge control forwarded")?;
    let delivered = match forwarded {
        BridgeServerMessage::ControlForwarded { delivered, .. } => delivered,
        _ => 0,
    };

    let mut result = None;
    let mut ok = None;
    let mut screenshot = None;
    timeout(result_timeout, async {
        while ok.is_none() || (waits_for_screenshot && screenshot.is_none()) {
            let Some(message) = next_server_message(&mut ws).await? else {
                bail!("bridge connection closed before control result was received");
            };
            match message {
                BridgeServerMessage::ControlResult {
                    id: result_id,
                    ok: result_ok,
                    result: value,
                    error,
                } if result_id == id => {
                    if !result_ok {
                        let message = error
                            .as_ref()
                            .and_then(|value| value.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("bridge control request failed");
                        bail!("{message}");
                    }
                    ok = Some(true);
                    result = value;
                }
                BridgeServerMessage::Screenshot {
                    id: screenshot_id,
                    mime,
                    data,
                } if screenshot_id == id => {
                    let data_len = decoded_data_len(&data);
                    screenshot = Some(BridgeScreenshot {
                        mime,
                        data_len,
                        data: Some(data),
                    });
                }
                _ => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("timed out waiting for bridge control result")??;

    Ok(BridgeControlOutcome {
        delivered,
        ok: ok.unwrap_or(false),
        result,
        screenshot,
    })
}

pub async fn collect_events(
    target: &BridgeTarget,
    subscription: &BridgeSubscription,
    max_events: usize,
    event_timeout: Duration,
) -> Result<Vec<BridgeEvent>> {
    let (mut ws, _) = connect_async(&target.meta.url)
        .await
        .with_context(|| format!("connect to bridge {}", target.meta.url))?;

    send_auth_and_subscribe(&mut ws, target, subscription).await?;

    let mut events = Vec::new();
    let collect_result = timeout(event_timeout, async {
        while events.len() < max_events {
            let Some(message) = next_server_message(&mut ws).await? else {
                break;
            };
            if let BridgeServerMessage::Event(event) = message {
                events.push(event);
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;

    if let Ok(result) = collect_result {
        result?;
    }

    Ok(events)
}

pub fn format_events_transcript(events: &[BridgeEvent]) -> String {
    events
        .iter()
        .map(format_event_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_event_line(event: &BridgeEvent) -> String {
    let timestamp = event.timestamp.to_rfc3339();
    match &event.data {
        BridgeEventData::Console { level, message } => format!(
            "[{timestamp}] console page={} level={} message={}",
            quote_for_transcript(&event.page_id),
            level,
            quote_for_transcript(message)
        ),
        BridgeEventData::Error { message, stack } => {
            let mut line = format!(
                "[{timestamp}] error page={} message={}",
                quote_for_transcript(&event.page_id),
                quote_for_transcript(message)
            );
            if let Some(stack) = stack.as_ref().filter(|stack| !stack.trim().is_empty()) {
                line.push_str(" stack=");
                line.push_str(&quote_for_transcript(stack));
            }
            line
        }
        BridgeEventData::Pageview { url, title } => {
            let mut line = format!(
                "[{timestamp}] pageview page={} url={}",
                quote_for_transcript(&event.page_id),
                quote_for_transcript(url)
            );
            if let Some(title) = title.as_ref().filter(|title| !title.trim().is_empty()) {
                line.push_str(" title=");
                line.push_str(&quote_for_transcript(title));
            }
            line
        }
        BridgeEventData::Screenshot { mime, data_len } => format!(
            "[{timestamp}] screenshot page={} mime={} bytes={}",
            quote_for_transcript(&event.page_id),
            mime,
            data_len
        ),
    }
}

fn quote_for_transcript(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn default_levels() -> Vec<String> {
    vec!["errors".to_string()]
}

fn default_capabilities() -> Vec<String> {
    vec![
        "console".to_string(),
        "control".to_string(),
        "error".to_string(),
        "pageview".to_string(),
        "screenshot".to_string(),
    ]
}

fn default_filter() -> String {
    "off".to_string()
}

fn normalize_unique(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn decoded_data_len(data: &str) -> usize {
    BASE64_STANDARD
        .decode(data)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

async fn compute_staleness(meta: &BridgeMeta, path: &Path) -> (bool, Option<i64>) {
    if let Some(heartbeat_at) = meta.heartbeat_at {
        let age = Utc::now().signed_duration_since(heartbeat_at);
        let age_ms = age.num_milliseconds();
        return (age_ms > HEARTBEAT_STALE_MS, Some(age_ms));
    }

    if let Ok(metadata) = fs::metadata(path).await {
        if let Ok(modified) = metadata.modified() {
            let modified: DateTime<Utc> = modified.into();
            let age = Utc::now().signed_duration_since(modified);
            let age_ms = age.num_milliseconds();
            return (age_ms > HEARTBEAT_STALE_MS, Some(age_ms));
        }
    }

    (false, None)
}

fn client_id_for(target: &BridgeTarget) -> String {
    let workspace_name = target
        .meta
        .workspace_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    format!("code-bridge-{workspace_name}")
}

async fn send_json<S>(ws: &mut S, value: &impl Serialize) -> Result<()>
where
    S: futures::SinkExt<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let text = serde_json::to_string(value).context("serialize bridge message")?;
    ws.send(Message::Text(text.into())).await?;
    Ok(())
}

async fn send_auth_and_subscribe<S>(
    ws: &mut S,
    target: &BridgeTarget,
    subscription: &BridgeSubscription,
) -> Result<()>
where
    S: futures::SinkExt<Message>
        + futures::StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let auth = BridgeClientMessage::Auth {
        role: BridgeRole::Consumer,
        secret: target.meta.secret.clone(),
        client_id: client_id_for(target),
    };
    send_json(ws, &auth).await.context("send bridge auth")?;
    wait_for_server_message(
        ws,
        |message| matches!(message, BridgeServerMessage::AuthSuccess),
        AUTH_TIMEOUT,
    )
    .await
    .context("wait for bridge auth success")?;

    let subscription = subscription.clone().normalized();
    let subscribe = BridgeClientMessage::Subscribe {
        levels: subscription.levels,
        capabilities: subscription.capabilities,
        llm_filter: subscription.llm_filter,
    };
    send_json(ws, &subscribe)
        .await
        .context("send bridge subscribe")?;
    wait_for_server_message(
        ws,
        |message| matches!(message, BridgeServerMessage::SubscribeAck),
        SUBSCRIBE_TIMEOUT,
    )
    .await
    .context("wait for bridge subscribe ack")?;

    Ok(())
}

async fn wait_for_server_message<S, F>(
    ws: &mut S,
    mut matches: F,
    duration: Duration,
) -> Result<BridgeServerMessage>
where
    S: futures::StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    F: FnMut(&BridgeServerMessage) -> bool,
{
    timeout(duration, async {
        while let Some(message) = next_server_message(ws).await? {
            if matches(&message) {
                return Ok(message);
            }
        }
        bail!("bridge stream closed before expected message")
    })
    .await
    .context("timed out waiting for bridge message")?
}

async fn next_server_message<S>(ws: &mut S) -> Result<Option<BridgeServerMessage>>
where
    S: futures::StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(message) = ws.next().await {
        match message? {
            Message::Text(text) => {
                let message = serde_json::from_str::<BridgeServerMessage>(&text)
                    .with_context(|| format!("parse bridge message `{text}`"))?;
                return Ok(Some(message));
            }
            Message::Close(_) => return Ok(None),
            Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::Duration as ChronoDuration;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn discovery_finds_parent_metadata_and_marks_fresh() -> Result<()> {
        let workspace = TempDir::new()?;
        let code_dir = workspace.path().join(".code");
        fs::create_dir(&code_dir)?;
        fs::write(
            code_dir.join(META_FILE),
            serde_json::to_string(&json!({
                "url": "ws://127.0.0.1:1",
                "secret": "secret",
                "workspacePath": workspace.path(),
                "heartbeatAt": Utc::now(),
            }))?,
        )?;
        let child = workspace.path().join("a").join("b");
        fs::create_dir_all(&child)?;

        let targets = discover_bridge_targets(&child).await?;

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].meta.secret, "secret");
        assert!(!targets[0].stale);
        Ok(())
    }

    #[tokio::test]
    async fn discovery_marks_stale_heartbeat() -> Result<()> {
        let workspace = TempDir::new()?;
        let code_dir = workspace.path().join(".code");
        fs::create_dir(&code_dir)?;
        fs::write(
            code_dir.join(META_FILE),
            serde_json::to_string(&json!({
                "url": "ws://127.0.0.1:1",
                "secret": "secret",
                "heartbeatAt": Utc::now() - ChronoDuration::milliseconds(HEARTBEAT_STALE_MS + 1_000),
            }))?,
        )?;

        let targets = discover_bridge_targets(workspace.path()).await?;

        assert_eq!(targets.len(), 1);
        assert!(targets[0].stale);
        assert!(targets[0].heartbeat_age_ms.unwrap_or_default() >= HEARTBEAT_STALE_MS);
        Ok(())
    }

    #[tokio::test]
    async fn subscription_defaults_and_overrides_are_normalized() -> Result<()> {
        let workspace = TempDir::new()?;

        let defaults = load_subscription(workspace.path()).await?;
        assert_eq!(defaults.levels, vec!["errors"]);
        assert_eq!(
            defaults.capabilities,
            vec!["console", "control", "error", "pageview", "screenshot"]
        );
        assert_eq!(defaults.llm_filter, "off");

        let code_dir = workspace.path().join(".code");
        fs::create_dir(&code_dir)?;
        fs::write(
            code_dir.join(SUBSCRIPTION_FILE),
            r#"{
                "levels": [" Warn ", "errors", "WARN", ""],
                "capabilities": [" Control", "console", "control"],
                "llm_filter": " TRACE "
            }"#,
        )?;

        let override_subscription = load_subscription(workspace.path()).await?;
        assert_eq!(override_subscription.levels, vec!["errors", "warn"]);
        assert_eq!(override_subscription.capabilities, vec!["console", "control"]);
        assert_eq!(override_subscription.llm_filter, "trace");
        Ok(())
    }

    #[test]
    fn bridge_control_message_is_not_desktop_remote_control_shape() -> Result<()> {
        let message = BridgeClientMessage::ControlRequest(BridgeControlRequest {
            id: "request-1".to_string(),
            action: BridgeControlAction::Javascript,
            url: None,
            code: Some("location.href".to_string()),
            timeout_ms: Some(1_000),
            expect_result: Some(true),
        });

        let wire = serde_json::to_value(&message)?;
        assert_eq!(wire.get("type").and_then(Value::as_str), Some("control_request"));
        assert!(wire.get("method").is_none());
        assert!(!message.is_desktop_remote_control_message());
        Ok(())
    }

    #[test]
    fn format_events_transcript_renders_all_event_kinds() -> Result<()> {
        let timestamp = DateTime::parse_from_rfc3339("2026-06-06T12:00:00Z")?.with_timezone(&Utc);
        let events = vec![
            BridgeEvent {
                page_id: "page-1".to_string(),
                timestamp,
                data: BridgeEventData::Pageview {
                    url: "https://example.test/app".to_string(),
                    title: Some("Example App".to_string()),
                },
            },
            BridgeEvent {
                page_id: "page-1".to_string(),
                timestamp,
                data: BridgeEventData::Console {
                    level: "info".to_string(),
                    message: "ready".to_string(),
                },
            },
            BridgeEvent {
                page_id: "page-1".to_string(),
                timestamp,
                data: BridgeEventData::Error {
                    message: "TypeError: boom".to_string(),
                    stack: Some("at app.js:1".to_string()),
                },
            },
            BridgeEvent {
                page_id: "page-1".to_string(),
                timestamp,
                data: BridgeEventData::Screenshot {
                    mime: "image/png".to_string(),
                    data_len: 68,
                },
            },
        ];

        let transcript = format_events_transcript(&events);

        assert!(transcript.contains("pageview page=\"page-1\""));
        assert!(transcript.contains("url=\"https://example.test/app\""));
        assert!(transcript.contains("console page=\"page-1\" level=info message=\"ready\""));
        assert!(transcript.contains("error page=\"page-1\" message=\"TypeError: boom\""));
        assert!(transcript.contains("stack=\"at app.js:1\""));
        assert!(transcript.contains("screenshot page=\"page-1\" mime=image/png bytes=68"));
        Ok(())
    }

    #[test]
    fn format_events_transcript_empty_is_empty() {
        assert_eq!(format_events_transcript(&[]), "");
    }
}
