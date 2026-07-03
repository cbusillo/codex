use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RemoteControlConnectionStatus;
use codex_app_server_protocol::RemoteControlStatusReadResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_bridge_control_does_not_enter_desktop_remote_control_path() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    for method in ["codeBridge/control", "code_bridge/control"] {
        let request_id = mcp
            .send_raw_request(
                method,
                Some(json!({
                    "id": "bridge-control-1",
                    "action": "ping",
                    "expectResult": false,
                })),
            )
            .await?;

        let error = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
        )
        .await??;
        assert_eq!(error.error.code, -32600);
        assert!(error.error.message.contains("Invalid request"));
        assert!(error.error.message.contains(method));
    }

    let request_id = mcp
        .send_raw_request("remoteControl/status/read", Some(json!({})))
        .await?;
    let response: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let payload: RemoteControlStatusReadResponse = to_response(response)?;

    assert_eq!(payload.status, RemoteControlConnectionStatus::Disabled);
    assert_eq!(payload.environment_id, None);
    Ok(())
}
