mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use agentchat_core::{
    AgentChat, Channel, Error, Message, RichReply, TaskStatus, TaskUpdate, STOP_METADATA,
};
use agentchat_slack::{events, Regex, Slack, ViewTarget};
use axum::http::StatusCode;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use support::{envelope, event, MockSlack, Reply};

fn interactive(id: &str, payload: Value) -> Value {
    json!({"type": "interactive", "envelope_id": id, "payload": payload})
}

fn button(action_id: &str) -> Value {
    json!({
        "type": "block_actions", "team": {"id": "T1"}, "trigger_id": "1.2.trigger",
        "user": {"id": "U1", "team_id": "T1"}, "channel": {"id": "C1"},
        "message": {"ts": "9.000001"},
        "actions": [{"action_id": action_id, "value": "plain", "action_ts": "5.1"}],
    })
}

fn submission(callback_id: &str) -> Value {
    json!({
        "type": "view_submission", "team": {"id": "T1"}, "user": {"id": "U1", "team_id": "T1"},
        "view": {"id": "V1", "callback_id": callback_id, "state": {"values": {}}},
    })
}

/// Run Socket Mode with a receiver that forwards messages to the returned channel.
fn run(
    slack: &Slack,
) -> (
    tokio::task::JoinHandle<agentchat_core::Result<()>>,
    mpsc::UnboundedReceiver<Message>,
) {
    let (received, messages) = mpsc::unbounded_channel();
    let run = tokio::spawn({
        let slack = slack.clone();
        async move {
            slack
                .run(move |message| {
                    let received = received.clone();
                    async move {
                        received.send(message).unwrap();
                        Ok(())
                    }
                })
                .await
        }
    });
    (run, messages)
}

async fn authenticated(mock: &MockSlack) -> (Slack, Message) {
    let slack = mock.slack();
    slack.authenticate().await.unwrap();
    let source = events::normalize(&event("1.000001"), "BOT", "T1").unwrap();
    (slack, source)
}

#[tokio::test]
async fn app_payloads_reach_their_handlers_and_everything_is_acknowledged() {
    let mock = MockSlack::start().await;
    let slack = mock.slack();
    let (seen, mut handled) = mpsc::unbounded_channel::<(String, Value)>();
    for kind in ["app_home_opened", "member_joined_channel", "message"] {
        let seen = seen.clone();
        slack.on_event(kind, move |payload| {
            let seen = seen.clone();
            async move {
                seen.send((format!("event:{kind}"), payload)).unwrap();
                Ok(None)
            }
        });
    }
    let exact = seen.clone();
    slack.on_action("retry", move |payload| {
        let seen = exact.clone();
        async move {
            seen.send(("action:retry".into(), payload)).unwrap();
            Ok(None)
        }
    });
    let pattern = seen.clone();
    slack.on_action(Regex::new("^open_").unwrap(), move |payload| {
        let seen = pattern.clone();
        async move {
            seen.send(("action:open".into(), payload)).unwrap();
            Ok(None)
        }
    });
    let raw = seen.clone();
    slack.on_raw(move |payload| {
        let seen = raw.clone();
        async move {
            seen.send(("raw".into(), payload)).unwrap();
            Ok(None)
        }
    });
    let (run, mut messages) = run(&slack);
    mock.connected(1).await;

    mock.send(
        json!({"type": "events_api", "envelope_id": "home", "payload": {
        "team_id": "T1", "event_id": "Ev1",
        "event": {"type": "app_home_opened", "user": "U1", "tab": "home"}}}),
    );
    assert_eq!(mock.ack().await, json!({"envelope_id": "home"}));
    let (kind, payload) = handled.recv().await.unwrap();
    assert_eq!(kind, "event:app_home_opened");
    assert_eq!(payload["event"]["tab"], "home");

    mock.send(json!({"type": "events_api", "envelope_id": "joined", "payload": {
        "team_id": "T1", "event": {"type": "member_joined_channel", "user": "BOT", "channel": "C9"}}}));
    assert_eq!(mock.ack().await["envelope_id"], "joined");
    assert_eq!(
        handled.recv().await.unwrap().0,
        "event:member_joined_channel"
    );

    // A message AgentChat ignores, such as another bot's, reaches `on_event("message")`.
    mock.send(
        json!({"type": "events_api", "envelope_id": "bot", "payload": {
        "team_id": "T1", "event": {"type": "message", "channel": "C1", "bot_id": "B2",
        "ts": "3.000001", "text": "from a bot", "channel_type": "channel"}}}),
    );
    assert_eq!(mock.ack().await["envelope_id"], "bot");
    assert_eq!(handled.recv().await.unwrap().0, "event:message");

    mock.send(interactive("retry", button("retry")));
    assert_eq!(mock.ack().await["envelope_id"], "retry");
    let (kind, payload) = handled.recv().await.unwrap();
    assert_eq!(kind, "action:retry");
    assert_eq!(payload["trigger_id"], "1.2.trigger");

    mock.send(interactive("open", button("open_artifact_3")));
    assert_eq!(mock.ack().await["envelope_id"], "open");
    assert_eq!(handled.recv().await.unwrap().0, "action:open");

    mock.send(interactive(
        "shortcut",
        json!({"type": "shortcut", "team": {"id": "T1"},
        "callback_id": "new_task", "trigger_id": "1.2.3"}),
    ));
    assert_eq!(mock.ack().await["envelope_id"], "shortcut");
    assert_eq!(handled.recv().await.unwrap().0, "raw");

    // Another workspace's payloads are acknowledged and dropped.
    let mut foreign = button("retry");
    foreign["team"]["id"] = json!("T2");
    foreign["user"]["team_id"] = json!("T2");
    mock.send(interactive("foreign", foreign));
    assert_eq!(mock.ack().await["envelope_id"], "foreign");

    // Chat messages still reach the runtime, not the handlers.
    mock.send(envelope("chat", "7.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "chat");
    assert_eq!(
        messages.recv().await.unwrap().id.as_ref(),
        "slack:T1:C1:7.000001"
    );
    slack.close().await.unwrap();
    run.await.unwrap().unwrap();
    assert!(handled.try_recv().is_err());
}

#[tokio::test]
async fn view_submissions_and_commands_answer_in_the_acknowledgement() {
    let mock = MockSlack::start().await;
    let slack = mock.slack();
    slack.on_view("settings", |payload| async move {
        assert_eq!(payload["view"]["id"], "V1");
        Ok(Some(
            json!({"response_action": "errors", "errors": {"name": "Required"}}),
        ))
    });
    slack.on_view("slow", |_| async move {
        tokio::time::sleep(Duration::from_secs(4)).await;
        Ok(Some(json!({"response_action": "clear"})))
    });
    slack.on_command("/deploy", |payload| async move {
        Ok(Some(
            json!({"text": format!("Deploying {}", payload["text"].as_str().unwrap())}),
        ))
    });
    let (run, _messages) = run(&slack);
    mock.connected(1).await;

    mock.send(interactive("submit", submission("settings")));
    assert_eq!(
        mock.ack().await,
        json!({"envelope_id": "submit", "payload":
            {"response_action": "errors", "errors": {"name": "Required"}}})
    );

    mock.send(
        json!({"type": "slash_commands", "envelope_id": "cmd", "payload": {
        "team_id": "T1", "command": "/deploy", "text": "api", "user_id": "U1",
        "trigger_id": "1.2.3"}}),
    );
    assert_eq!(
        mock.ack().await,
        json!({"envelope_id": "cmd", "payload": {"text": "Deploying api"}})
    );

    // A submission without a handler is acknowledged without a body.
    mock.send(interactive("unknown", submission("nobody")));
    assert_eq!(mock.ack().await, json!({"envelope_id": "unknown"}));

    // A handler that misses the deadline gets an empty acknowledgement in time.
    let started = Instant::now();
    mock.send(interactive("late", submission("slow")));
    assert_eq!(mock.ack().await, json!({"envelope_id": "late"}));
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(2_400) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
    slack.close().await.unwrap();
    let _ = run.await.unwrap();
}

#[tokio::test]
async fn host_ingress_returns_the_message_and_the_acknowledgement() {
    let mock = MockSlack::start().await;
    let (slack, _) = authenticated(&mock).await;
    slack.on_view(Regex::new("^form").unwrap(), |_| async move {
        Ok(Some(json!({"response_action": "clear"})))
    });
    let handled = slack.handle_payload(&submission("form_1")).await.unwrap();
    assert!(handled.message.is_none());
    assert_eq!(handled.ack, Some(json!({"response_action": "clear"})));
    let handled = slack.handle_payload(&event("2.000001")).await.unwrap();
    assert_eq!(handled.message.unwrap().id.as_ref(), "slack:T1:C1:2.000001");
    assert!(handled.ack.is_none());
}

#[tokio::test]
async fn slack_native_stop_stops_the_turn_of_the_person_who_asked() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    let stopped_event = Arc::new(tokio::sync::Notify::new());
    let notify = stopped_event.clone();
    slack.on_event("agent_session_stopped", move |_| {
        let notify = notify.clone();
        async move {
            notify.notify_one();
            Ok(None)
        }
    });
    let app = Arc::new(AgentChat::new());
    let channel: Arc<dyn Channel> = Arc::new(slack.clone());
    let (started, mut running) = mpsc::unbounded_channel();
    let turn = tokio::spawn({
        let (app, channel, source) = (app.clone(), channel.clone(), source.clone());
        async move {
            app.receive(channel, source, move |context| {
                let started = started.clone();
                async move {
                    started.send(()).unwrap();
                    context.wait_stopped().await;
                    Ok(Some("Stopped.".into()))
                }
            })
            .await
        }
    });
    running.recv().await.unwrap();
    let stop = |user: &str, ts: &str| {
        json!({"type": "events_api", "envelope_id": format!("stop-{ts}"), "payload": {
            "team_id": "T1", "event": {"type": "agent_session_stopped", "channel": "C1",
            "thread_ts": "1.000001", "user": user, "event_ts": ts, "streaming_message_ts": []}}})
    };
    // Socket delivery: the stop request goes to the receiver, the event to the handler,
    // and the session status is cleared.
    let app_for_socket = app.clone();
    let socket_channel = channel.clone();
    let run = tokio::spawn({
        let slack = slack.clone();
        async move {
            slack
                .run(move |message| {
                    let (app, channel) = (app_for_socket.clone(), socket_channel.clone());
                    async move { app.receive(channel, message, |_| async { Ok(None) }).await }
                })
                .await
        }
    });
    mock.connected(1).await;
    mock.send(stop("U2", "2.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "stop-2.000001");
    stopped_event.notified().await;
    assert!(!app.running(&source.conversation_id).unwrap().stopped());
    mock.send(stop("U1", "3.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "stop-3.000001");
    stopped_event.notified().await;
    tokio::time::timeout(Duration::from_secs(2), turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let cleared = mock.calls("assistant.threads.setStatus").await;
    assert_eq!(cleared.len(), 2);
    assert_eq!(
        cleared[1].payload,
        json!({"channel_id": "C1", "thread_ts": "1.000001", "status": ""})
    );
    let history = app.history(&source.conversation_id, None).await.unwrap();
    assert!(history
        .iter()
        .all(|message| !message.metadata.contains_key(STOP_METADATA)));
    slack.close().await.unwrap();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn agent_sessions_report_a_lifecycle_status() {
    let mock = MockSlack::start().await;
    let mut config = mock.config();
    config.agent_sessions = true;
    let slack = Slack::new(config).unwrap();
    slack.authenticate().await.unwrap();
    let source = events::normalize(&event("1.000001"), "BOT", "T1").unwrap();
    assert!(slack.set_status(&source, "is reading…").await.unwrap());
    assert!(slack.set_status(&source, "").await.unwrap());
    let calls = mock.calls("agents.sessions.setStatus").await;
    assert_eq!(
        calls
            .iter()
            .map(|call| call.payload["status"].clone())
            .collect::<Vec<_>>(),
        [json!("processing"), json!("active")]
    );
    assert!(mock.calls("assistant.threads.setStatus").await.is_empty());
}

#[tokio::test]
async fn ephemeral_notices_go_to_one_person_and_never_into_history() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    mock.override_reply(
        "chat.postEphemeral",
        Reply::Json(
            StatusCode::OK,
            json!({"ok": true, "message_ts": "9.500001"}),
        ),
    )
    .await;
    let app = Arc::new(AgentChat::new());
    let channel: Arc<dyn Channel> = Arc::new(slack.clone());
    let content = RichReply {
        text: "Only you can see this".into(),
        blocks: vec![],
        attachments: vec![],
    };
    let sent = app
        .send_ephemeral(channel.clone(), &source, "U1", &content)
        .await
        .unwrap();
    assert_eq!(sent.metadata["ephemeral"], true);
    assert_eq!(sent.metadata["message_timestamp"], "9.500001");
    let request = &mock.calls("chat.postEphemeral").await[0].payload;
    assert_eq!(
        (&request["channel"], &request["user"], &request["thread_ts"]),
        (&json!("C1"), &json!("U1"), &json!("1.000001"))
    );
    assert!(app
        .history(&source.conversation_id, None)
        .await
        .unwrap()
        .is_empty());
    // An ephemeral notice cannot be deleted, and a failed one is never posted publicly.
    assert!(app.delete(channel.clone(), &sent).await.is_err());
    mock.override_reply(
        "chat.postEphemeral",
        Reply::Json(
            StatusCode::OK,
            json!({"ok": false, "error": "user_not_in_channel"}),
        ),
    )
    .await;
    assert!(app
        .send_ephemeral(channel, &source, "U1", &content)
        .await
        .is_err());
    assert!(mock.calls("chat.postMessage").await.is_empty());
}

#[tokio::test]
async fn views_and_delete_call_slack_and_surface_errors() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    let modal = json!({"type": "modal", "callback_id": "settings",
        "title": {"type": "plain_text", "text": "Settings"}, "blocks": []});
    for method in ["views.open", "views.push", "views.update", "views.publish"] {
        mock.override_reply(
            method,
            Reply::Json(StatusCode::OK, json!({"ok": true, "view": {"id": "V1"}})),
        )
        .await;
    }
    assert_eq!(slack.open_view("1.2.3", &modal).await.unwrap()["id"], "V1");
    assert_eq!(slack.push_view("1.2.4", &modal).await.unwrap()["id"], "V1");
    slack
        .update_view(&modal, &ViewTarget::Id("V1".into()), Some("h1"))
        .await
        .unwrap();
    let home = json!({"type": "home", "blocks": []});
    slack.publish_home("U1", &home, None).await.unwrap();
    assert_eq!(
        mock.calls("views.open").await[0].payload["trigger_id"],
        "1.2.3"
    );
    assert_eq!(mock.calls("views.update").await[0].payload["view_id"], "V1");
    assert_eq!(
        mock.calls("views.publish").await[0].payload["user_id"],
        "U1"
    );
    mock.override_reply(
        "views.open",
        Reply::Json(
            StatusCode::OK,
            json!({"ok": false, "error": "expired_trigger_id"}),
        ),
    )
    .await;
    let error = slack.open_view("1.2.3", &modal).await.unwrap_err();
    assert!(error.to_string().contains("expired_trigger_id"), "{error}");

    let reply = slack.reply(&source, "Working…").await.unwrap();
    slack.delete(&reply).await.unwrap();
    assert_eq!(
        mock.calls("chat.delete").await[0].payload,
        json!({"channel": "C1", "ts": "9.000001"})
    );
}

#[tokio::test]
async fn streams_show_tasks_and_plans_as_chunks() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    mock.override_reply(
        "chat.startStream",
        Reply::Json(StatusCode::OK, json!({"ok": true, "ts": "8.000001"})),
    )
    .await;
    let app = Arc::new(AgentChat::new());
    let mut stream = app.stream(Arc::new(slack), &source, Some(Duration::ZERO));
    stream.plan("Fixing the build").await.unwrap();
    stream
        .task(TaskUpdate::new(
            "t1",
            "Read the logs",
            TaskStatus::InProgress,
        ))
        .await
        .unwrap();
    stream.append("Found it.").await.unwrap();
    let mut done = TaskUpdate::new("t1", "Read the logs", TaskStatus::Complete);
    done.output = Some("1 error".into());
    stream.task(done).await.unwrap();
    stream.finish().await.unwrap();
    // The first update starts the stream in chunk mode; later ones are appended.
    let start = &mock.calls("chat.startStream").await[0].payload;
    assert_eq!(start["task_display_mode"], "plan");
    assert_eq!(
        start["chunks"],
        json!([{"type": "plan_update", "title": "Fixing the build"}])
    );
    assert!(start.get("markdown_text").is_none());
    let appends: Vec<Value> = mock
        .calls("chat.appendStream")
        .await
        .into_iter()
        .map(|call| call.payload["chunks"].clone())
        .collect();
    assert_eq!(
        appends,
        [
            json!([{"type": "task_update", "id": "t1", "title": "Read the logs",
                    "status": "in_progress"}]),
            json!([{"type": "markdown_text", "text": "Found it."}]),
            json!([{"type": "task_update", "id": "t1", "title": "Read the logs",
                    "status": "complete", "output": "1 error"}]),
        ]
    );
    assert_eq!(
        mock.calls("chat.stopStream").await[0].payload,
        json!({"channel": "C1", "ts": "8.000001"})
    );
    assert_eq!(stream.messages()[0].text, "Found it.");
}

#[tokio::test]
async fn refused_task_chunks_fall_back_to_plain_text_streaming() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    mock.override_reply(
        "chat.startStream",
        Reply::Json(
            StatusCode::OK,
            json!({"ok": false, "error": "invalid_arguments"}),
        ),
    )
    .await;
    mock.override_reply(
        "chat.startStream",
        Reply::Json(StatusCode::OK, json!({"ok": true, "ts": "8.000001"})),
    )
    .await;
    let app = Arc::new(AgentChat::new());
    let mut stream = app.stream(Arc::new(slack), &source, Some(Duration::ZERO));
    stream
        .task(TaskUpdate::new("t1", "Search", TaskStatus::InProgress))
        .await
        .unwrap();
    stream.append("Answer").await.unwrap();
    stream
        .task(TaskUpdate::new("t1", "Search", TaskStatus::Complete))
        .await
        .unwrap();
    stream.append(" done").await.unwrap();
    stream.finish().await.unwrap();
    let starts = mock.calls("chat.startStream").await;
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].payload["markdown_text"], "Answer");
    let appends = mock.calls("chat.appendStream").await;
    assert_eq!(appends.len(), 1);
    assert_eq!(appends[0].payload["markdown_text"], " done");

    // A refused task update never fails the answer; later updates are dropped.
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    mock.override_reply(
        "chat.startStream",
        Reply::Json(StatusCode::OK, json!({"ok": true, "ts": "8.000001"})),
    )
    .await;
    mock.override_reply(
        "chat.appendStream",
        Reply::Json(
            StatusCode::OK,
            json!({"ok": false, "error": "invalid_arguments"}),
        ),
    )
    .await;
    let mut stream = app.stream(Arc::new(slack), &source, Some(Duration::ZERO));
    stream.plan("Plan").await.unwrap();
    stream.append("").await.unwrap();
    stream
        .task(TaskUpdate::new("t1", "Search", TaskStatus::Complete))
        .await
        .unwrap();
    stream
        .task(TaskUpdate::new("t2", "Write", TaskStatus::Complete))
        .await
        .unwrap();
    stream.append("Text").await.unwrap();
    stream.finish().await.unwrap();
    let appends = mock.calls("chat.appendStream").await;
    assert_eq!(appends.len(), 2);
    assert_eq!(appends[1].payload["chunks"][0]["type"], "markdown_text");
}

#[tokio::test]
async fn channels_without_tasks_ignore_them_and_ephemeral_fails_closed() {
    struct Plain;
    #[async_trait::async_trait]
    impl Channel for Plain {
        fn name(&self) -> &str {
            "plain"
        }
        async fn reply(&self, source: &Message, content: &str) -> agentchat_core::Result<Message> {
            let mut reply = source.clone();
            reply.id = "reply".into();
            reply.text = content.into();
            Ok(reply)
        }
    }
    let source = events::normalize(&event("1.000001"), "BOT", "T1").unwrap();
    let app = Arc::new(AgentChat::new());
    let channel: Arc<dyn Channel> = Arc::new(Plain);
    let mut stream = app.stream(channel.clone(), &source, None);
    stream
        .task(TaskUpdate::new("t1", "Search", TaskStatus::InProgress))
        .await
        .unwrap();
    stream.plan("Plan").await.unwrap();
    stream.append("Answer").await.unwrap();
    assert_eq!(stream.finish().await.unwrap().unwrap().text, "Answer");
    let content = RichReply {
        text: "private".into(),
        blocks: vec![],
        attachments: vec![],
    };
    assert!(matches!(
        app.send_ephemeral(channel.clone(), &source, "U1", &content)
            .await,
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        app.delete(channel, &source).await,
        Err(Error::Unsupported(_))
    ));
}
