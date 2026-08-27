use std::sync::Arc;

use super::compact::{
    apply_emergency_compaction_fallback,
    is_context_overflow_error,
    perform_compaction,
    prune_orphan_tool_outputs,
    response_input_from_core_items,
    run_inline_auto_compact_task,
    sanitize_items_for_compact,
    send_compaction_checkpoint_warning,
};
use super::Session;
use super::TurnContext;
use crate::Prompt;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::error::RetryAfter;
use crate::protocol::AgentMessageEvent;
use crate::protocol::ErrorEvent;
use crate::protocol::EventMsg;
use crate::protocol::InputItem;
use crate::util::backoff;
use code_protocol::models::ResponseInputItem;
use code_protocol::models::ResponseItem;
use code_protocol::protocol::CompactedItem;
use code_protocol::protocol::RolloutItem;
use std::time::Duration;

const MAX_REMOTE_COMPACT_CONTEXT_OVERFLOW_RETRIES: usize = 1;
const REMOTE_COMPACT_OVERFLOW_RECENT_ITEM_LIMIT: usize = 64;
const MAX_REMOTE_COMPACT_USAGE_LIMIT_RETRIES: usize = 2;

fn is_remote_compact_unavailable_error(err: &CodexErr) -> bool {
    matches!(
        err,
        CodexErr::UnexpectedStatus(response)
            if matches!(
                response.status,
                reqwest::StatusCode::NOT_FOUND
                    | reqwest::StatusCode::METHOD_NOT_ALLOWED
                    | reqwest::StatusCode::NOT_IMPLEMENTED
            )
    )
}

pub(super) async fn run_inline_remote_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    extra_input: Vec<InputItem>,
) -> Vec<ResponseItem> {
    let sub_id = sess.next_internal_sub_id();
    match run_remote_compact_task_inner(&sess, &turn_context, &sub_id, extra_input).await {
        Ok(history) => history,
        Err(err) if is_remote_compact_unavailable_error(&err) => {
            tracing::warn!(
                "Remote compact endpoint is unavailable ({err}); falling back to local compaction"
            );
            run_inline_auto_compact_task(sess, turn_context).await
        }
        Err(err) => {
            let event = sess.make_event(
                &sub_id,
                EventMsg::Error(ErrorEvent {
                    message: format!("remote compact failed: {err}"),
                }),
            );
            sess.send_event(event).await;
            Vec::new()
        }
    }
}

pub(super) async fn run_remote_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    sub_id: String,
    extra_input: Vec<InputItem>,
) -> CodexResult<()> {
    match run_remote_compact_task_inner(
        &sess,
        &turn_context,
        &sub_id,
        extra_input.clone(),
    )
    .await
    {
        Ok(_history) => {
            // Mirror local compaction behaviour: clear the running task when the
            // compaction finished successfully so the UI can unblock.
            sess.remove_task(&sub_id);
            Ok(())
        }
        Err(err) if is_remote_compact_unavailable_error(&err) => {
            tracing::warn!(
                "Remote compact endpoint is unavailable ({err}); falling back to local compaction"
            );
            perform_compaction(sess, turn_context, sub_id, extra_input, true).await
        }
        Err(err) => {
            let event = sess.make_event(
                &sub_id,
                EventMsg::Error(ErrorEvent {
                    message: err.to_string(),
                }),
            );
            sess.send_event(event).await;
            Err(err)
        }
    }
}

async fn run_remote_compact_task_inner(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    sub_id: &str,
    extra_input: Vec<InputItem>,
) -> CodexResult<Vec<ResponseItem>> {
    let mut turn_items = sess.turn_input_with_history({
        if extra_input.is_empty() {
            Vec::new()
        } else {
            let response_input: ResponseInputItem = response_input_from_core_items(extra_input);
            vec![ResponseItem::from(response_input)]
        }
    });

    turn_items = sanitize_items_for_compact(turn_items);
    let mut overflow_retries = 0usize;
    let mut overflow_trimmed_count = 0usize;
    let max_retries = turn_context.client.get_provider().stream_max_retries();
    let mut retries = 0;
    let mut usage_limit_retries = 0usize;
    let new_history = loop {
        prune_orphan_tool_outputs(&mut turn_items);

        let mut prompt = Prompt::default();
        prompt.input = turn_items.clone();
        prompt.base_instructions_override = turn_context.base_instructions.clone();
        prompt.include_additional_instructions = false;
        prompt.log_tag = Some("codex/remote-compact".to_string());

        let _used_fallback_model_metadata = sess.apply_remote_model_overrides(&mut prompt).await;

        match turn_context
            .client
            .compact_conversation_history(&prompt)
            .await
        {
            Ok(history) => {
                if overflow_trimmed_count > 0 {
                    tracing::warn!(
                        "Context window exceeded during remote compact; retried after trimming {overflow_trimmed_count} item(s) from prompt"
                    );
                }
                break history;
            }
            Err(err) if is_context_overflow_error(&err) => {
                if overflow_retries < MAX_REMOTE_COMPACT_CONTEXT_OVERFLOW_RETRIES {
                    let removed = trim_remote_compact_input_after_overflow(&mut turn_items);
                    if removed == 0 {
                        let reason = "Remote compact failed: context overflow even with minimal input.";
                        return Ok(
                            apply_emergency_compaction_fallback(
                                sess,
                                turn_context.as_ref(),
                                sub_id,
                                reason,
                            )
                            .await,
                        );
                    }

                    overflow_retries = overflow_retries.saturating_add(1);
                    overflow_trimmed_count = overflow_trimmed_count.saturating_add(removed);
                    tracing::warn!(
                        "Context window exceeded while remote compacting; trimmed {removed} oldest item(s), retaining {} recent item(s)",
                        turn_items.len()
                    );
                    retries = 0;
                    usage_limit_retries = 0;
                    continue;
                }

                let reason = format!(
                    "Remote compact retried with reduced recent history but still exceeded the context window after trimming {overflow_trimmed_count} item(s)."
                );
                return Ok(
                    apply_emergency_compaction_fallback(
                        sess,
                        turn_context.as_ref(),
                        sub_id,
                        &reason,
                    )
                    .await,
                );
            }
            Err(CodexErr::UsageLimitReached(limit_err)) => {
                if usage_limit_retries >= MAX_REMOTE_COMPACT_USAGE_LIMIT_RETRIES {
                    let reason = "Remote compact hit persistent usage limits and cannot continue.";
                    return Ok(
                        apply_emergency_compaction_fallback(
                            sess,
                            turn_context.as_ref(),
                            sub_id,
                            reason,
                        )
                        .await,
                    );
                }
                usage_limit_retries = usage_limit_retries.saturating_add(1);
                let now = chrono::Utc::now();
                let retry_after = limit_err
                    .retry_after(now)
                    .unwrap_or_else(|| RetryAfter::from_duration(Duration::from_secs(5 * 60), now));
                let mut message = format!("{limit_err} Auto-retrying");
                message.push('…');
                sess.notify_stream_error(sub_id, message).await;
                tokio::time::sleep(retry_after.delay).await;
                retries = 0;
                continue;
            }
            Err(err) if is_remote_compact_unavailable_error(&err) => return Err(err),
            Err(err) => {
                if retries < max_retries {
                    retries += 1;
                    let delay = backoff(retries);
                    sess
                        .notify_stream_error(
                            sub_id,
                            format!(
                                "remote compact error: {err}; retrying {retries}/{max_retries} in {delay:?}…"
                            ),
                        )
                        .await;
                    tokio::time::sleep(delay).await;
                    continue;
                }

                return Err(err);
            }
        }
    };

    sess.replace_history(new_history.clone());
    {
        let mut state = sess.state.lock().unwrap();
        state.token_usage_info = None;
    }

    send_compaction_checkpoint_warning(sess, sub_id).await;

    let rollout_item = RolloutItem::Compacted(CompactedItem {
        message: "Conversation history compacted.".to_string(),
        replacement_history: None,
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    let event = sess.make_event(
        sub_id,
        EventMsg::AgentMessage(AgentMessageEvent {
            message: "Compact task completed".to_string(),
        }),
    );
    sess.send_event(event).await;

    Ok(new_history)
}

fn trim_remote_compact_input_after_overflow(turn_items: &mut Vec<ResponseItem>) -> usize {
    let before = turn_items.len();
    if before <= 1 {
        return 0;
    }

    let keep = (before / 2)
        .max(1)
        .min(REMOTE_COMPACT_OVERFLOW_RECENT_ITEM_LIMIT);
    let remove_count = before.saturating_sub(keep);
    turn_items.drain(0..remove_count);
    remove_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UnexpectedResponseError;
    use code_protocol::models::ContentItem;
    use reqwest::StatusCode;

    fn user_item(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn overflow_retry_trims_to_recent_bounded_history() {
        let mut items = (0..200)
            .map(|idx| user_item(&format!("item {idx}")))
            .collect::<Vec<_>>();

        let removed = trim_remote_compact_input_after_overflow(&mut items);

        assert_eq!(removed, 136);
        assert_eq!(items.len(), 64);
        assert!(matches!(
            &items[0],
            ResponseItem::Message { content, .. }
                if matches!(content.first(), Some(ContentItem::InputText { text }) if text == "item 136")
        ));
    }

    #[test]
    fn overflow_retry_does_not_trim_minimal_input() {
        let mut items = vec![user_item("current input")];

        let removed = trim_remote_compact_input_after_overflow(&mut items);

        assert_eq!(removed, 0);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn unavailable_remote_compact_endpoint_falls_back_without_retrying() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_IMPLEMENTED,
        ] {
            let error = CodexErr::UnexpectedStatus(UnexpectedResponseError {
                status,
                body: r#"{"detail":"Not Found"}"#.to_string(),
                request_id: None,
            });

            assert!(is_remote_compact_unavailable_error(&error));
        }
    }

    #[test]
    fn transient_remote_compact_errors_keep_existing_retry_behavior() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let error = CodexErr::UnexpectedStatus(UnexpectedResponseError {
                status,
                body: "temporary failure".to_string(),
                request_id: None,
            });

            assert!(!is_remote_compact_unavailable_error(&error));
        }
    }
}
