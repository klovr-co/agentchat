use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use agentchat_core::*;
use async_trait::async_trait;
use tokio::sync::{Mutex, Semaphore};

fn incoming(id: &str, conversation: &str) -> Message {
    Message {
        id: id.into(),
        conversation_id: conversation.into(),
        channel: "fake".into(),
        sender: Sender {
            id: "alice".into(),
            display_name: None,
        },
        text: id.into(),
        role: Role::User,
        metadata: Default::default(),
        thread_id: None,
        addressed: true,
        attachments: vec![],
    }
}

#[derive(Default)]
struct FakeChannel {
    calls: AtomicUsize,
    sent: Mutex<Vec<String>>,
    uncertain: bool,
    history: Option<Vec<Message>>,
    history_fails: bool,
    statuses: Mutex<Vec<String>>,
    status_supported: bool,
    status_fails: bool,
}

#[async_trait]
impl Channel for FakeChannel {
    fn name(&self) -> &str {
        "fake"
    }

    async fn set_status(&self, _source: &Message, status: &str) -> Result<bool> {
        if !self.status_supported {
            return Ok(false);
        }
        self.statuses.lock().await.push(status.to_owned());
        if self.status_fails {
            return Err(Error::Transport("Status unavailable".into()));
        }
        Ok(true)
    }

    async fn reply(&self, source: &Message, content: &str) -> Result<Message> {
        let sequence = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.sent.lock().await.push(content.to_owned());
        if self.uncertain {
            return Err(Error::Transport("Delivery outcome is unknown".into()));
        }
        Ok(Message {
            id: format!("reply:{sequence}").into(),
            sender: Sender {
                id: "bot".into(),
                display_name: None,
            },
            text: content.into(),
            role: Role::Assistant,
            ..source.clone()
        })
    }

    async fn thread_history(
        &self,
        _source: &Message,
        _limit: usize,
    ) -> Result<Option<Vec<Message>>> {
        if self.history_fails {
            return Err(Error::Transport("History unavailable".into()));
        }
        Ok(self.history.clone())
    }
}

#[test]
fn typed_turn_enforces_each_effect_and_keeps_empty_replies() {
    let mut turn = Turn::new();
    assert_eq!(turn.effect(), &TurnEffect::Claim);
    assert!(turn.resolve(TurnResult::ReplySent).is_err());
    assert_eq!(turn.effect(), &TurnEffect::Claim);
    turn.resolve(TurnResult::Claimed(true)).unwrap();
    assert_eq!(turn.effect(), &TurnEffect::AcquireConversation);
    turn.resolve(TurnResult::ConversationAcquired).unwrap();
    turn.resolve(TurnResult::IncomingAppended).unwrap();
    turn.resolve(TurnResult::HandlerCompleted(Some(String::new())))
        .unwrap();
    assert_eq!(turn.effect(), &TurnEffect::SendReply(String::new()));
    turn.resolve(TurnResult::ReplySent).unwrap();
    turn.resolve(TurnResult::ReplyAppended).unwrap();
    assert_eq!(turn.effect(), &TurnEffect::Complete);
    assert!(turn.resolve(TurnResult::Claimed(true)).is_err());
}

#[test]
fn duplicate_finishes_without_any_other_effect() {
    let mut turn = Turn::new();
    turn.resolve(TurnResult::Claimed(false)).unwrap();
    assert_eq!(turn.effect(), &TurnEffect::Complete);
}

#[test]
fn history_selection_preserves_root_and_current_turn() {
    assert_eq!(thread_history_indices(5, 3).unwrap(), vec![0, 3, 4]);
    assert_eq!(thread_history_indices(1, 3).unwrap(), vec![0]);
    assert!(thread_history_indices(5, 1).is_err());
    assert!(thread_history_indices(5, 101).is_err());
}

#[test]
fn memory_values_preserve_identity_and_zero_limit_matches_python() {
    let mut store = MemoryStore::new();
    let value = Arc::new(vec![1, 2, 3]);
    let conversation = ConversationId::from("example");
    assert!(store.claim(&MessageId::from("one")));
    assert!(!store.claim(&MessageId::from("one")));
    store.append(&conversation, value.clone());
    assert!(Arc::ptr_eq(
        &store.history(&conversation, Some(0))[0],
        &value
    ));
    assert!(store
        .history(&ConversationId::from("missing"), None)
        .is_empty());
}

#[test]
fn portable_ids_are_strings_and_optional_message_fields_have_defaults() {
    let message: Message = serde_json::from_value(serde_json::json!({
        "id": "discord:999999999999999999", "conversation_id": "slack:T:C:1.000001",
        "channel": "custom", "sender": {"id": "person"}, "text": "hello", "role": "user"
    }))
    .unwrap();
    assert!(message.addressed);
    assert_eq!(message.id.as_ref(), "discord:999999999999999999");
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value["id"], "discord:999999999999999999");
    assert_eq!(value["role"], "user");
}

#[test]
fn file_debug_output_does_not_include_bytes() {
    let content = b"private attachment".to_vec();
    let upload = UploadFile {
        filename: "report.txt".into(),
        content: content.clone(),
        title: None,
    };
    let download = DownloadedFile {
        id: "file".into(),
        name: "report.txt".into(),
        content: content.clone(),
        media_type: None,
    };
    for debug in [format!("{upload:?}"), format!("{download:?}")] {
        assert!(debug.contains("content_length: 18"));
        assert!(!debug.contains("private attachment"));
        assert!(!debug.contains(&format!("{content:?}")));
    }
}

#[tokio::test]
async fn nested_reply_and_history_do_not_reacquire_the_conversation_lock() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel::default());
    tokio::time::timeout(
        Duration::from_secs(1),
        app.receive(
            channel.clone(),
            incoming("one", "chat"),
            |context| async move {
                assert_eq!(context.history(30).await?.len(), 1);
                context.reply("manual reply").await?;
                assert_eq!(context.conversation.history(None).await?.len(), 2);
                Ok(None)
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(channel.calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn uncertain_send_never_appends_success_or_retries_claimed_input() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel {
        uncertain: true,
        ..Default::default()
    });
    let message = incoming("one", "chat");
    let result = app
        .receive(channel.clone(), message.clone(), |_| async {
            Ok(Some("answer".into()))
        })
        .await;
    assert!(matches!(result, Err(Error::Transport(_))));
    app.receive(channel.clone(), message, |_| async {
        panic!("a claimed message must not run again")
    })
    .await
    .unwrap();
    assert_eq!(channel.calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn handler_failure_keeps_input_and_claim_and_releases_permit() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel::default());
    assert!(app
        .receive(channel.clone(), incoming("one", "chat"), |_| async {
            Err(Error::Handler("failed".into()))
        })
        .await
        .is_err());
    app.receive(channel.clone(), incoming("one", "chat"), |_| async {
        panic!("duplicate")
    })
    .await
    .unwrap();
    app.receive(channel, incoming("two", "chat"), |_| async { Ok(None) })
        .await
        .unwrap();
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn queued_conversations_serialize_independently() {
    let app = Arc::new(Runtime::new().with_busy(Busy::Queue));
    let channel = Arc::new(FakeChannel::default());
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let app1 = app.clone();
    let channel1 = channel.clone();
    let started1 = started.clone();
    let release1 = release.clone();
    let first = tokio::spawn(async move {
        app1.receive(channel1, incoming("one", "chat"), |_| {
            let (started1, release1) = (started1.clone(), release1.clone());
            async move {
                started1.add_permits(1);
                release1.acquire().await.unwrap().forget();
                Ok(Some("first".into()))
            }
        })
        .await
    });
    started.acquire().await.unwrap().forget();
    let app2 = app.clone();
    let channel2 = channel.clone();
    let second = tokio::spawn(async move {
        app2.receive(channel2, incoming("two", "chat"), |context| async move {
            assert_eq!(context.conversation.history(None).await?.len(), 3);
            Ok(Some("second".into()))
        })
        .await
    });
    tokio::time::timeout(
        Duration::from_secs(1),
        app.receive(channel.clone(), incoming("other", "separate"), |_| async {
            Ok(None)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    release.add_permits(1);
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(*channel.sent.lock().await, vec!["first", "second"]);
}

#[tokio::test]
async fn cancellation_releases_lock_without_releasing_claim() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel::default());
    let started = Arc::new(Semaphore::new(0));
    let app1 = app.clone();
    let channel1 = channel.clone();
    let started1 = started.clone();
    let task = tokio::spawn(async move {
        app1.receive(channel1, incoming("cancelled", "chat"), |_| {
            let started1 = started1.clone();
            async move {
                started1.add_permits(1);
                std::future::pending::<Result<Option<String>>>().await
            }
        })
        .await
    });
    started.acquire().await.unwrap().forget();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    app.receive(channel.clone(), incoming("cancelled", "chat"), |_| async {
        panic!("duplicate")
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        app.receive(channel, incoming("next", "chat"), |_| async { Ok(None) }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn native_history_keeps_root_and_tail_without_replaying_past_turns() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel {
        history: Some(vec![
            incoming("root", "chat"),
            incoming("middle", "chat"),
            incoming("last", "chat"),
        ]),
        ..Default::default()
    });
    let message = Message {
        thread_id: Some("thread".into()),
        ..incoming("current", "chat")
    };
    app.receive(channel, message, |context| async move {
        let history = context.history(3).await?;
        assert_eq!(
            history.iter().map(|m| m.id.as_ref()).collect::<Vec<_>>(),
            vec!["root", "last", "current"]
        );
        assert_eq!(context.conversation.history(None).await?.len(), 1);
        Ok(None)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_history_failure_does_not_fall_back_to_local_history() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel {
        history_fails: true,
        ..Default::default()
    });
    let message = Message {
        thread_id: Some("thread".into()),
        ..incoming("current", "chat")
    };
    let result = app
        .receive(channel.clone(), message, |context| async move {
            context.history(30).await?;
            Ok(Some("must not send".into()))
        })
        .await;
    assert!(matches!(result, Err(Error::Transport(_))));
    assert_eq!(channel.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn optional_operations_fail_without_sending_or_inventing_history() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel::default());
    let message = incoming("one", "chat");
    assert!(!app
        .set_status(channel.clone(), &message, "working")
        .await
        .unwrap());
    assert!(matches!(
        app.upload_files(channel.clone(), &message, &[]).await,
        Err(Error::Unsupported("file uploads"))
    ));
    assert!(matches!(
        app.mirror(channel.clone(), &message, &message, "web").await,
        Err(Error::Unsupported("mirroring"))
    ));
    assert!(app
        .history(&message.conversation_id, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(channel.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn activity_refreshes_latest_label_and_explicit_close_clears_once() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel {
        status_supported: true,
        ..Default::default()
    });
    let context = MessageContext::new(app.clone(), channel.clone(), incoming("status", "chat"));
    let mut working = context
        .working("working", Duration::from_secs(10))
        .await
        .unwrap();
    assert!(working.update("finishing").await);
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    working.close().await;
    working.close().await;
    drop(working);
    assert_eq!(
        *channel.statuses.lock().await,
        vec!["working", "finishing", "finishing", ""]
    );
    assert!(app.history(&"chat".into(), None).await.unwrap().is_empty());
}

#[tokio::test]
async fn activity_transport_failure_does_not_prevent_an_answer() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel {
        status_supported: true,
        status_fails: true,
        ..Default::default()
    });
    app.receive(
        channel.clone(),
        incoming("status", "chat"),
        |context| async move {
            let mut working = context.working("working", Duration::from_secs(60)).await?;
            assert!(!working.update("still working").await);
            working.close().await;
            Ok(Some("answer".into()))
        },
    )
    .await
    .unwrap();
    assert_eq!(channel.calls.load(Ordering::SeqCst), 1);
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn activity_drop_schedules_best_effort_clear() {
    let channel = Arc::new(FakeChannel {
        status_supported: true,
        ..Default::default()
    });
    let working = WorkingStatus::start(
        channel.clone(),
        incoming("status", "chat"),
        "working".into(),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    drop(working);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if channel
                .statuses
                .lock()
                .await
                .last()
                .is_some_and(String::is_empty)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(*channel.statuses.lock().await, vec!["working", ""]);
}

/// A channel with buttons and streaming that records what it showed.
#[derive(Default)]
struct LiveChannel {
    prompts: std::sync::Mutex<Vec<Prompt>>,
    closed: std::sync::Mutex<Vec<(String, Option<String>)>>,
    streamed: std::sync::Mutex<Vec<String>>,
    replies: std::sync::Mutex<Vec<String>>,
    limit: Option<usize>,
    buttons: bool,
}

impl LiveChannel {
    fn with_buttons() -> Self {
        Self {
            buttons: true,
            ..Default::default()
        }
    }
    fn shown(&self) -> Vec<Prompt> {
        self.prompts.lock().unwrap().clone()
    }
    fn closed(&self) -> Vec<(String, Option<String>)> {
        self.closed.lock().unwrap().clone()
    }
    fn sent(&self, source: &Message, id: &str, text: &str) -> Message {
        Message {
            id: id.into(),
            sender: Sender {
                id: "bot".into(),
                display_name: None,
            },
            text: text.into(),
            role: Role::Assistant,
            ..source.clone()
        }
    }
}

#[async_trait]
impl Channel for LiveChannel {
    fn name(&self) -> &str {
        "live"
    }
    async fn reply(&self, source: &Message, content: &str) -> Result<Message> {
        let mut replies = self.replies.lock().unwrap();
        replies.push(content.to_owned());
        Ok(self.sent(source, &format!("reply:{}", replies.len()), content))
    }
    fn streaming(&self) -> Option<StreamOptions> {
        Some(StreamOptions {
            interval: Duration::ZERO,
            limit: self.limit,
        })
    }
    async fn start_stream(&self, _source: &Message, text: &str) -> Result<StreamHandle> {
        self.streamed.lock().unwrap().push(format!("start:{text}"));
        Ok(serde_json::json!({"n": 1}))
    }
    async fn append_stream(
        &self,
        _source: &Message,
        stream: StreamHandle,
        text: &str,
    ) -> Result<StreamHandle> {
        self.streamed.lock().unwrap().push(format!("edit:{text}"));
        Ok(serde_json::json!({"n": stream["n"].as_u64().unwrap() + 1}))
    }
    async fn stop_stream(
        &self,
        source: &Message,
        _stream: StreamHandle,
        text: &str,
    ) -> Result<Message> {
        let mut streamed = self.streamed.lock().unwrap();
        streamed.push(format!("stop:{text}"));
        Ok(self.sent(source, &format!("stream:{}", streamed.len()), text))
    }
    fn supports_prompts(&self) -> bool {
        self.buttons
    }
    async fn send_prompt(&self, source: &Message, prompt: &Prompt) -> Result<Message> {
        self.prompts.lock().unwrap().push(prompt.clone());
        Ok(self.sent(source, &format!("prompt:{}", prompt.id), &prompt.text))
    }
    async fn close_prompt(
        &self,
        _source: &Message,
        _sent: &Message,
        prompt: &Prompt,
        answer: Option<&Answer>,
    ) -> Result<()> {
        self.closed
            .lock()
            .unwrap()
            .push((prompt.id.clone(), answer.map(|a| a.choice.clone())));
        Ok(())
    }
}

fn press(id: &str, conversation: &str, sender: &str, prompt: &Prompt, choice: &str) -> Message {
    let mut message = incoming(id, conversation);
    message.sender.id = sender.into();
    message.metadata.insert(
        ANSWER_METADATA.into(),
        answer_metadata(&prompt.token(&prompt.choices[0]).unwrap())
            .map(|mut value| {
                value["choice"] = choice.into();
                value
            })
            .unwrap(),
    );
    message
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

type Seen = Arc<std::sync::Mutex<Vec<String>>>;

/// Start a handler for `first` that waits for `release`, recording what it sees.
fn blocked_turn(
    app: &Arc<Runtime>,
    channel: Arc<LiveChannel>,
    seen: Seen,
    release: Arc<Semaphore>,
    read_steered: bool,
) -> tokio::task::JoinHandle<Result<()>> {
    let app = app.clone();
    tokio::spawn(async move {
        app.receive(channel, incoming("first", "chat"), move |context| {
            let (seen, release) = (seen.clone(), release.clone());
            async move {
                seen.lock()
                    .unwrap()
                    .push(format!("run:{}", context.message.text));
                if context.message.text == "first" {
                    release.acquire().await.unwrap().forget();
                    if read_steered {
                        for message in context.steered() {
                            seen.lock()
                                .unwrap()
                                .push(format!("steered:{}", message.text));
                        }
                    }
                }
                Ok(None)
            }
        })
        .await
    })
}

#[tokio::test]
async fn steer_is_the_default_and_hands_messages_to_the_running_handler() {
    let app = Arc::new(Runtime::new());
    assert_eq!(app.busy(), Busy::Steer);
    let channel = Arc::new(LiveChannel::default());
    let seen = Seen::default();
    let release = Arc::new(Semaphore::new(0));
    let first = blocked_turn(&app, channel.clone(), seen.clone(), release.clone(), true);
    until(|| app.running(&"chat".into()).is_some()).await;
    app.receive(channel.clone(), incoming("second", "chat"), |_| async {
        panic!("a steered message must not start its own turn")
    })
    .await
    .unwrap();
    release.add_permits(1);
    first.await.unwrap().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["run:first", "steered:second"]);
    let history = app.history(&"chat".into(), None).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(app.running(&"chat".into()).is_none());
}

#[tokio::test]
async fn unread_steered_messages_run_afterwards_as_their_own_turns() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::default());
    let seen = Seen::default();
    let release = Arc::new(Semaphore::new(0));
    let first = blocked_turn(&app, channel.clone(), seen.clone(), release.clone(), false);
    until(|| app.running(&"chat".into()).is_some()).await;
    for id in ["second", "third"] {
        app.receive(channel.clone(), incoming(id, "chat"), |_| async {
            Ok(None)
        })
        .await
        .unwrap();
    }
    release.add_permits(1);
    first.await.unwrap().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["run:first", "run:second", "run:third"]
    );
    // Each message is in history once, in arrival order.
    let history = app.history(&"chat".into(), None).await.unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn reject_drops_messages_while_busy() {
    let app = Arc::new(Runtime::new().with_busy(Busy::Reject));
    let channel = Arc::new(LiveChannel::default());
    let seen = Seen::default();
    let release = Arc::new(Semaphore::new(0));
    let first = blocked_turn(&app, channel.clone(), seen.clone(), release.clone(), true);
    until(|| app.running(&"chat".into()).is_some()).await;
    app.receive(channel.clone(), incoming("second", "chat"), |_| async {
        panic!("rejected")
    })
    .await
    .unwrap();
    release.add_permits(1);
    first.await.unwrap().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["run:first"]);
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn ask_policy_lets_the_sender_choose_and_undecided_messages_queue() {
    let app = Arc::new(Runtime::new().with_busy(Busy::Ask));
    let channel = Arc::new(LiveChannel::with_buttons());
    let seen = Seen::default();
    let release = Arc::new(Semaphore::new(0));
    let first = blocked_turn(&app, channel.clone(), seen.clone(), release.clone(), true);
    until(|| app.running(&"chat".into()).is_some()).await;
    let noop = |_| async { Ok(None) };
    app.receive(channel.clone(), incoming("second", "chat"), noop)
        .await
        .unwrap();
    app.receive(channel.clone(), incoming("third", "chat"), noop)
        .await
        .unwrap();
    let shown = channel.shown();
    assert_eq!(shown.len(), 2);
    assert_eq!(
        shown[0]
            .choices
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        vec!["steer", "queue"]
    );
    // Only the sender may decide.
    app.receive(
        channel.clone(),
        press("p1", "chat", "mallory", &shown[0], "steer"),
        noop,
    )
    .await
    .unwrap();
    app.receive(
        channel.clone(),
        press("p2", "chat", "alice", &shown[0], "steer"),
        noop,
    )
    .await
    .unwrap();
    release.add_permits(1);
    first.await.unwrap().unwrap();
    // "second" was steered; nobody decided on "third", so it ran next.
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["run:first", "steered:second", "run:third"]
    );
    assert_eq!(
        channel.closed(),
        vec![
            (shown[0].id.clone(), Some("steer".into())),
            (shown[1].id.clone(), None)
        ]
    );
    // Presses are never history.
    let history = app.history(&"chat".into(), None).await.unwrap();
    assert_eq!(
        history.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        vec!["first", "second", "third"]
    );
}

#[tokio::test]
async fn ask_without_buttons_steers() {
    let app = Arc::new(Runtime::new().with_busy(Busy::Ask));
    let channel = Arc::new(LiveChannel::default());
    let seen = Seen::default();
    let release = Arc::new(Semaphore::new(0));
    let first = blocked_turn(&app, channel.clone(), seen.clone(), release.clone(), true);
    until(|| app.running(&"chat".into()).is_some()).await;
    app.receive(channel.clone(), incoming("second", "chat"), |_| async {
        Ok(None)
    })
    .await
    .unwrap();
    release.add_permits(1);
    first.await.unwrap().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["run:first", "steered:second"]);
}

#[tokio::test]
async fn prompts_accept_one_allowed_answer_from_their_conversation() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::with_buttons());
    let source = incoming("question", "chat");
    let asking = tokio::spawn({
        let (app, channel, source) = (app.clone(), channel.clone(), source.clone());
        async move {
            app.ask(
                channel,
                &source,
                "Delete it?",
                vec![
                    Choice::new("yes", "Delete").styled(ChoiceStyle::Danger),
                    Choice::new("no", "Keep"),
                ],
                AskOptions::default(),
            )
            .await
        }
    });
    until(|| !channel.shown().is_empty()).await;
    let prompt = channel.shown()[0].clone();
    let noop = |_| async { panic!("a press never runs the handler") };
    for ignored in [
        press("a", "chat", "mallory", &prompt, "yes"),
        press("b", "elsewhere", "alice", &prompt, "yes"),
        press("c", "chat", "alice", &prompt, "maybe"),
    ] {
        app.receive(channel.clone(), ignored, noop).await.unwrap();
    }
    assert!(!asking.is_finished());
    app.receive(
        channel.clone(),
        press("d", "chat", "alice", &prompt, "no"),
        noop,
    )
    .await
    .unwrap();
    app.receive(
        channel.clone(),
        press("e", "chat", "alice", &prompt, "yes"),
        noop,
    )
    .await
    .unwrap();
    let answer = asking.await.unwrap().unwrap();
    assert_eq!(
        (answer.choice.as_str(), answer.label.as_str()),
        ("no", "Keep")
    );
    assert_eq!(answer.sender.id, "alice");
    assert_eq!(answer.closed_text(&prompt), "Delete it?\n\n✓ Keep — alice");
    assert_eq!(
        channel.closed(),
        vec![(prompt.id.clone(), Some("no".into()))]
    );
    assert!(app.history(&"chat".into(), None).await.unwrap().is_empty());
}

#[tokio::test]
async fn allowed_widens_who_may_answer() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::with_buttons());
    let source = incoming("question", "chat");
    let asking = tokio::spawn({
        let (app, channel, source) = (app.clone(), channel.clone(), source.clone());
        async move {
            let options = AskOptions {
                allowed: Allowed::Senders(vec!["bob".into()]),
                ..Default::default()
            };
            app.ask(
                channel,
                &source,
                "Ship?",
                vec![Choice::new("yes", "Yes")],
                options,
            )
            .await
        }
    });
    until(|| !channel.shown().is_empty()).await;
    let prompt = channel.shown()[0].clone();
    let noop = |_| async { Ok(None) };
    app.receive(
        channel.clone(),
        press("a", "chat", "alice", &prompt, "yes"),
        noop,
    )
    .await
    .unwrap();
    app.receive(
        channel.clone(),
        press("b", "chat", "bob", &prompt, "yes"),
        noop,
    )
    .await
    .unwrap();
    assert_eq!(asking.await.unwrap().unwrap().sender.id, "bob");
}

#[tokio::test(start_paused = true)]
async fn unanswered_and_unsupported_prompts_fail_closed() {
    let app = Arc::new(Runtime::new());
    let plain = Arc::new(LiveChannel::default());
    let source = incoming("question", "chat");
    let choices = || vec![Choice::new("yes", "Yes")];
    assert!(matches!(
        app.ask(plain, &source, "Run?", choices(), AskOptions::default())
            .await,
        Err(Error::Unsupported("prompts"))
    ));
    let channel = Arc::new(LiveChannel::with_buttons());
    let options = AskOptions {
        timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let result = app
        .ask(channel.clone(), &source, "Run?", choices(), options)
        .await;
    assert!(matches!(result, Err(Error::Timeout(_))));
    let prompt = channel.shown()[0].clone();
    assert_eq!(channel.closed(), vec![(prompt.id.clone(), None)]);
    // A late press does nothing.
    app.receive(
        channel.clone(),
        press("late", "chat", "alice", &prompt, "yes"),
        |_| async { panic!("late press") },
    )
    .await
    .unwrap();
    assert_eq!(channel.closed().len(), 1);
}

#[tokio::test]
async fn stop_reaches_the_running_handler_by_api_and_button() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::with_buttons());
    assert!(!app.stop(&"chat".into()));
    let running = tokio::spawn({
        let (app, channel) = (app.clone(), channel.clone());
        async move {
            app.receive(channel, incoming("work", "chat"), |context| async move {
                assert!(!context.stopped());
                let offered = context.offer_stop(StopOptions::default()).await?;
                assert!(offered.is_some());
                context.wait_stopped().await;
                assert!(context.stopped());
                Ok(Some("stopped".into()))
            })
            .await
        }
    });
    until(|| !channel.shown().is_empty()).await;
    let prompt = channel.shown()[0].clone();
    assert_eq!(prompt.choices[0].style, ChoiceStyle::Danger);
    app.receive(
        channel.clone(),
        press("p", "chat", "alice", &prompt, "stop"),
        |_| async { Ok(None) },
    )
    .await
    .unwrap();
    running.await.unwrap().unwrap();
    assert_eq!(*channel.replies.lock().unwrap(), vec!["stopped"]);
    assert_eq!(
        channel.closed(),
        vec![(prompt.id.clone(), Some("stop".into()))]
    );

    // By API, and the Stop button goes away when the turn ends unanswered.
    let release = Arc::new(Semaphore::new(0));
    let running = tokio::spawn({
        let (app, channel, release) = (app.clone(), channel.clone(), release.clone());
        async move {
            app.receive(channel, incoming("again", "chat"), move |context| {
                let release = release.clone();
                async move {
                    context.offer_stop(StopOptions::default()).await?;
                    let stopped = context.control().unwrap().until_stopped();
                    release.add_permits(1);
                    assert!(stopped.await);
                    Ok(None)
                }
            })
            .await
        }
    });
    release.acquire().await.unwrap().forget();
    assert!(app.stop(&"chat".into()));
    running.await.unwrap().unwrap();
    let last = channel.shown().last().unwrap().id.clone();
    assert_eq!(channel.closed().last().unwrap(), &(last, None));
    assert!(!app.stop(&"chat".into()));
}

#[tokio::test]
async fn offer_stop_without_buttons_or_turn() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::default());
    let message = incoming("work", "chat");
    assert!(app
        .offer_stop(channel.clone(), &message, StopOptions::default())
        .await
        .is_err());
    app.receive(channel, message, |context| async move {
        assert!(context.offer_stop(StopOptions::default()).await?.is_none());
        Ok(None)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_turn_unregisters_so_later_messages_run() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::default());
    let task = tokio::spawn({
        let (app, channel) = (app.clone(), channel.clone());
        async move {
            app.receive(channel, incoming("stuck", "chat"), |_| async {
                std::future::pending::<Result<Option<String>>>().await
            })
            .await
        }
    });
    until(|| app.running(&"chat".into()).is_some()).await;
    task.abort();
    let _ = task.await;
    assert!(app.running(&"chat".into()).is_none());
    app.receive(channel.clone(), incoming("next", "chat"), |_| async {
        Ok(Some("ran".into()))
    })
    .await
    .unwrap();
    assert_eq!(*channel.replies.lock().unwrap(), vec!["ran"]);
}

#[tokio::test]
async fn streams_edit_in_place_split_long_answers_and_confirm_history() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel {
        limit: Some(13),
        ..Default::default()
    });
    let source = incoming("ask", "chat");
    let mut stream = app.stream(channel.clone(), &source, None);
    stream.append("hello").await.unwrap();
    stream.append(" world").await.unwrap();
    stream.append(" and more text").await.unwrap();
    let last = stream.finish().await.unwrap().unwrap();
    assert_eq!(
        *channel.streamed.lock().unwrap(),
        vec![
            "start:hello",
            "edit:hello world",
            "stop:hello world ",
            "start:and more text",
            "stop:and more text",
        ]
    );
    assert_eq!(stream.text(), "hello world and more text");
    assert_eq!(stream.messages().len(), 2);
    assert_eq!(last.text, "and more text");
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 2);
    assert!(stream.append("more").await.is_err());
    assert!(stream.finish().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn streams_wait_for_the_interval_and_abort_without_history() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(LiveChannel::default());
    let source = incoming("ask", "chat");
    let mut stream = app.stream(channel.clone(), &source, Some(Duration::from_secs(2)));
    stream.append("a").await.unwrap();
    stream.append("b").await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    stream.append("c").await.unwrap();
    stream.abort().await.unwrap();
    assert_eq!(
        *channel.streamed.lock().unwrap(),
        vec!["start:a", "edit:abc", "stop:abc"]
    );
    assert!(app.history(&"chat".into(), None).await.unwrap().is_empty());
}

#[tokio::test]
async fn channels_without_streaming_get_one_reply() {
    let app = Arc::new(Runtime::new());
    let channel = Arc::new(FakeChannel::default());
    let source = incoming("ask", "chat");
    let mut stream = app.stream(channel.clone(), &source, None);
    stream.append("one ").await.unwrap();
    stream.append("reply").await.unwrap();
    assert!(channel.sent.lock().await.is_empty());
    assert_eq!(stream.finish().await.unwrap().unwrap().text, "one reply");
    assert_eq!(*channel.sent.lock().await, vec!["one reply"]);
    let mut empty = app.stream(channel.clone(), &source, None);
    assert!(empty.finish().await.unwrap().is_none());
    assert_eq!(app.history(&"chat".into(), None).await.unwrap().len(), 1);
}
