mod support;

use std::sync::Arc;
use std::time::Duration;

use agentchat_core::{
    AgentChat, AskOptions, Channel, Choice, ChoiceStyle, Error, Message, ANSWER_METADATA,
};
use agentchat_slack::{events, Slack};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use support::{envelope, event, MockSlack};

fn press(value: &str, user: &str, ts: &str) -> Value {
    json!({
        "type": "interactive",
        "envelope_id": format!("press-{ts}"),
        "payload": {
            "type": "block_actions",
            "team": {"id": "T1"},
            "user": {"id": user, "name": "alice"},
            "channel": {"id": "C1"},
            "container": {"type": "message", "message_ts": "9.000001", "channel_id": "C1"},
            "message": {"ts": "9.000001", "thread_ts": "1.000001"},
            "actions": [{"action_id": "agentchat_x", "value": value, "action_ts": ts,
                "text": {"type": "plain_text", "text": "Approve"}}],
        },
    })
}

async fn authenticated(mock: &MockSlack) -> (Slack, Message) {
    let slack = mock.slack();
    slack.authenticate().await.unwrap();
    let source = events::normalize(&event("1.000001"), "BOT", "T1").unwrap();
    (slack, source)
}

#[tokio::test]
async fn interactive_envelopes_are_acknowledged_and_become_presses() {
    let mock = MockSlack::start().await;
    let mut config = mock.config();
    config.ack_emoji = Some("eyes".into());
    let slack = Slack::new(config).unwrap();
    let (received, mut messages) = mpsc::unbounded_channel();
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
    mock.connected(1).await;
    mock.send(press("ac:0123456789abcdef:approve", "U1", "5.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "press-5.000001");
    let message = messages.recv().await.unwrap();
    assert_eq!(
        message.metadata[ANSWER_METADATA],
        json!({"prompt": "0123456789abcdef", "choice": "approve"})
    );
    assert_eq!(message.conversation_id.as_ref(), "slack:T1:C1:1.000001");
    assert_eq!(message.sender.id, "U1");
    // Other interactions are acknowledged but never reach the receiver.
    mock.send(press("not-a-prompt", "U1", "6.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "press-6.000001");
    mock.send(envelope("event", "7.000001"));
    assert_eq!(mock.ack().await["envelope_id"], "event");
    assert_eq!(
        messages.recv().await.unwrap().id.as_ref(),
        "slack:T1:C1:7.000001"
    );
    // A press is not an incoming request, so it gets no acknowledgement reaction.
    let reactions = mock.calls("reactions.add").await;
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].payload["timestamp"], "7.000001");
    slack.close().await.unwrap();
    run.await.unwrap().unwrap();
}

#[tokio::test]
async fn host_ingress_accepts_presses_from_this_workspace_only() {
    let mock = MockSlack::start().await;
    let (slack, _) = authenticated(&mock).await;
    let payload = press("ac:0123456789abcdef:approve", "U1", "5.000001")["payload"].clone();
    assert!(slack.handle_event(&payload).await.unwrap().is_some());
    let mut foreign = payload.clone();
    foreign["team"]["id"] = json!("T2");
    assert!(slack.handle_event(&foreign).await.unwrap().is_none());
    let mut own = payload;
    own["user"]["id"] = json!("BOT");
    assert!(slack.handle_event(&own).await.unwrap().is_none());
}

#[tokio::test]
async fn streams_in_threads_use_native_stream_methods() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    mock.override_reply(
        "chat.startStream",
        support::Reply::Json(
            axum::http::StatusCode::OK,
            json!({"ok": true, "channel": "C1", "ts": "8.000001"}),
        ),
    )
    .await;
    let handle = slack.start_stream(&source, "Hel").await.unwrap();
    let handle = slack.append_stream(&source, handle, "Hello").await.unwrap();
    let handle = slack.append_stream(&source, handle, "Hello").await.unwrap();
    let message = slack.stop_stream(&source, handle, "Hello!").await.unwrap();
    let start = &mock.calls("chat.startStream").await[0].payload;
    assert_eq!(
        *start,
        json!({"channel": "C1", "thread_ts": "1.000001", "markdown_text": "Hel",
            "recipient_user_id": "U1", "recipient_team_id": "T1"})
    );
    // Only new text is appended, and an unchanged update sends nothing.
    let appends = mock.calls("chat.appendStream").await;
    assert_eq!(appends.len(), 1);
    assert_eq!(
        appends[0].payload,
        json!({"channel": "C1", "ts": "8.000001", "markdown_text": "lo"})
    );
    assert_eq!(
        mock.calls("chat.stopStream").await[0].payload,
        json!({"channel": "C1", "ts": "8.000001", "markdown_text": "!"})
    );
    assert_eq!(message.text, "Hello!");
    assert_eq!(message.metadata["message_timestamp"], "8.000001");
    // A stream can only grow.
    let handle = json!({"channel": "C1", "ts": "8.000001", "sent": "Hello"});
    assert!(slack.append_stream(&source, handle, "Bye").await.is_err());
}

#[tokio::test]
async fn plain_dms_post_the_streamed_answer_once() {
    let mock = MockSlack::start().await;
    let slack = mock.slack();
    slack.authenticate().await.unwrap();
    let mut payload = event("1.000001");
    payload["event"]["type"] = json!("message");
    payload["event"]["channel_type"] = json!("im");
    payload["event"]["channel"] = json!("D1");
    let dm = events::normalize(&payload, "BOT", "T1").unwrap();
    let app = Arc::new(AgentChat::new());
    let channel: Arc<dyn Channel> = Arc::new(slack.clone());
    let mut stream = app.stream(channel, &dm, Some(Duration::ZERO));
    stream.append("one ").await.unwrap();
    stream.append("answer").await.unwrap();
    stream.finish().await.unwrap();
    assert!(mock.calls("chat.startStream").await.is_empty());
    let posts = mock.calls("chat.postMessage").await;
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].payload["text"], "one answer");
    assert_eq!(
        app.history(&dm.conversation_id, None).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn prompts_show_buttons_and_close_with_the_answer() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    let slack = Arc::new(slack);
    let app = Arc::new(AgentChat::new());
    let channel: Arc<dyn Channel> = slack.clone();
    let asking = tokio::spawn({
        let (app, channel, source) = (app.clone(), channel.clone(), source.clone());
        async move {
            app.ask(
                channel,
                &source,
                "Deploy to production?",
                vec![
                    Choice::new("yes", "Deploy").styled(ChoiceStyle::Primary),
                    Choice::new("no", "Cancel"),
                ],
                AskOptions::default(),
            )
            .await
        }
    });
    let post = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(post) = mock.calls("chat.postMessage").await.pop() {
                return post.payload;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(post["thread_ts"], "1.000001");
    let buttons = post["blocks"].as_array().unwrap().last().unwrap()["elements"].clone();
    assert_eq!(buttons[0]["style"], "primary");
    assert!(buttons[1].get("style").is_none());
    let token = buttons[0]["value"].as_str().unwrap().to_owned();

    // Someone else's press is ignored; the sender's answers.
    for user in ["U2", "U1"] {
        let press = slack
            .handle_event(&press(&token, user, "5.000001")["payload"])
            .await
            .unwrap()
            .unwrap();
        app.receive(channel.clone(), press, |_| async {
            panic!("a press never runs the handler")
        })
        .await
        .unwrap();
    }
    let answer = asking.await.unwrap().unwrap();
    assert_eq!(
        (answer.choice.as_str(), answer.sender.id.as_str()),
        ("yes", "U1")
    );
    let update = &mock.calls("chat.update").await[0].payload;
    assert_eq!(update["ts"], "9.000001");
    assert_eq!(update["text"], "Deploy to production?\n\n✓ Deploy — alice");
    assert!(app
        .history(&source.conversation_id, None)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn unanswered_slack_prompts_time_out_and_lose_their_buttons() {
    let mock = MockSlack::start().await;
    let (slack, source) = authenticated(&mock).await;
    let app = Arc::new(AgentChat::new());
    let options = AskOptions {
        timeout: Duration::from_millis(200),
        ..Default::default()
    };
    let result = app
        .ask(
            Arc::new(slack),
            &source,
            "Approve?",
            vec![Choice::new("yes", "Yes")],
            options,
        )
        .await;
    assert!(matches!(result, Err(Error::Timeout(_))));
    let update = &mock.calls("chat.update").await[0].payload;
    assert_eq!(update["text"], "Approve?");
    assert!(update["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|block| block["type"] != "actions"));
}
