//! Forms asked in chat (see `agentchat_core::FormSession`). While a form is open,
//! the runtime gives it the messages of the person filling it in; the values reach
//! the channel's `on_view(callback_id)` handler like a modal submission.
use crate::host::await_py;
use crate::runtime::{call, copy};
use crate::{apps, convert};
use agentchat_core::{Form, FormSession, FormStep, Question, Route};
use pyo3::{exceptions::PyValueError, prelude::*, types::PyDict};
use serde_json::{json, Value};

type Object = Py<PyAny>;

/// Open forms by conversation and person: `{"channel", "source", "session",
/// "expires", "sent", "prompt"}`; `session` is the `FormSession` as JSON.
fn forms<'py>(app: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    apps::memory(app, "_forms")
}

fn key(message: &Bound<'_, PyAny>) -> PyResult<String> {
    let conversation = message.getattr("conversation_id")?.extract::<String>()?;
    let sender = message
        .getattr("sender")?
        .getattr("id")?
        .extract::<String>()?;
    Ok(format!("{conversation}\n{sender}"))
}

fn now() -> f64 {
    std::time::UNIX_EPOCH
        .elapsed()
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

fn invalid(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

/// Whether a form is waiting for this message's sender in its conversation.
pub fn is_open(app: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>) -> PyResult<bool> {
    forms(app)?.contains(key(message)?)
}

/// Start asking `form` of `source`'s sender in `source`'s conversation, replacing
/// a form already open for them. The channel needs an `on_view` handler for the
/// form's `callback_id`.
pub async fn open(app: Object, channel: Object, source: Object, form: Object) -> PyResult<()> {
    let (session, question) = Python::attach(|py| -> PyResult<_> {
        let form: Form = serde_json::from_value(convert::to_json(form.bind(py))?)
            .map_err(|e| invalid(format!("Invalid form: {e}")))?;
        let route = Route::View {
            callback_id: form.callback_id.clone(),
        };
        if apps::find(channel.bind(py), &route)?.is_none() {
            return Err(PyValueError::new_err(format!(
                "Register @channel.on_view({:?}) to receive this form",
                form.callback_id
            )));
        }
        FormSession::start(form).map_err(invalid)
    })?;
    ask(&app, &channel, &source, session, question).await
}

/// Show `question` and keep `session` waiting for its answer.
async fn ask(
    app: &Object,
    channel: &Object,
    source: &Object,
    mut session: FormSession,
    question: Question,
) -> PyResult<()> {
    // Store the session before sending, so an answer to the question is recognized.
    let (key, stored, prompt, text) = Python::attach(|py| -> PyResult<_> {
        let interactive = channel.bind(py).is_instance(
            &py.import("agentchat.channels.base")?
                .getattr("InteractiveChannel")?,
        )?;
        let mut prompt = None;
        if interactive && question.buttons() {
            let shown =
                agentchat_core::Prompt::new(question.text.clone(), question.choices.clone())
                    .map_err(invalid)?;
            session.prompt = Some(shown.id.clone());
            prompt = Some(python_prompt(py, &shown)?);
        }
        let stored = serde_json::to_string(&session).map_err(invalid)?;
        let key = key(source.bind(py))?;
        let entry = PyDict::new(py);
        entry.set_item("channel", channel.bind(py))?;
        entry.set_item("source", source.bind(py))?;
        entry.set_item("session", &stored)?;
        entry.set_item("expires", now() + session.form.timeout)?;
        entry.set_item("sent", py.None())?;
        entry.set_item("prompt", prompt.as_ref().map(|p| p.bind(py)))?;
        forms(app.bind(py))?.set_item(&key, entry)?;
        Ok((key, stored, prompt, question.numbered()))
    })?;
    let sent = match prompt {
        Some(prompt) => {
            call(
                copy(channel),
                "send_prompt",
                vec![copy(source), prompt],
                vec![],
            )
            .await
        }
        None => {
            let text = Python::attach(|py| text.into_pyobject(py).map(|t| t.into_any().unbind()))?;
            call(copy(channel), "reply", vec![copy(source), text], vec![]).await
        }
    };
    Python::attach(|py| -> PyResult<()> {
        let open = forms(app.bind(py))?;
        let current = open.get_item(&key)?;
        let ours = match &current {
            Some(entry) => entry.get_item("session")?.extract::<String>()? == stored,
            None => false,
        };
        match (&sent, ours, current) {
            (Ok(sent), true, Some(entry)) => entry.set_item("sent", sent.bind(py)),
            // The question may not have been shown: stop waiting for an answer.
            (Err(_), true, _) => open.del_item(&key),
            _ => Ok(()),
        }
    })?;
    sent.map(drop)
}

fn python_prompt(py: Python<'_>, prompt: &agentchat_core::Prompt) -> PyResult<Object> {
    let models = py.import("agentchat.models")?;
    let choices = prompt
        .choices
        .iter()
        .map(|choice| {
            models
                .getattr("Choice")?
                .call1((choice.id.as_str(), choice.label.as_str()))
        })
        .collect::<PyResult<Vec<_>>>()?;
    Ok(models
        .getattr("Prompt")?
        .call1((
            prompt.id.as_str(),
            prompt.text.as_str(),
            pyo3::types::PyTuple::new(py, choices)?,
        ))?
        .unbind())
}

/// Give `message` to the form open for its sender, if any. Returns whether the
/// form took it. A form that has timed out is closed and takes nothing.
pub async fn intercept(
    app: &Object,
    channel: &Object,
    message: &Object,
    press: Option<&(String, String)>,
    stop: bool,
) -> PyResult<bool> {
    if stop {
        return Ok(false);
    }
    let found = Python::attach(|py| -> PyResult<_> {
        let key = key(message.bind(py))?;
        let open = forms(app.bind(py))?;
        let Some(entry) = open.get_item(&key)? else {
            return Ok(None);
        };
        if !entry.get_item("channel")?.is(channel.bind(py)) {
            return Ok(None);
        }
        let session: FormSession =
            serde_json::from_str(&entry.get_item("session")?.extract::<String>()?)
                .map_err(invalid)?;
        let expired = entry.get_item("expires")?.extract::<f64>()? < now();
        let sent = entry.get_item("sent")?.unbind();
        let prompt = entry.get_item("prompt")?.unbind();
        Ok(Some((key, session, expired, sent, prompt)))
    })?;
    let Some((key, mut session, expired, sent, prompt)) = found else {
        return Ok(false);
    };
    let remove = || Python::attach(|py| forms(app.bind(py))?.del_item(&key));
    if expired {
        remove()?;
        close_buttons(channel, message, &sent, &prompt, None).await;
        let payload = closed(&session, message, "timeout")?;
        deliver(channel, message, &session, payload).await?;
        return Ok(false);
    }
    let step = match press {
        Some((prompt_id, choice)) => match session.answer_choice(prompt_id, choice) {
            Some(step) => step,
            // Another prompt's button, such as ask() from a running turn.
            None => return Ok(false),
        },
        None => {
            let text = Python::attach(|py| message.bind(py).getattr("text")?.extract::<String>())?;
            session.answer_text(&text)
        }
    };
    let answered = press.map(|(_, choice)| choice.as_str());
    match step {
        FormStep::Ask(question) => {
            close_buttons(channel, message, &sent, &prompt, answered).await;
            let source = Python::attach(|py| message.clone_ref(py));
            ask(app, channel, &source, session, question).await?;
        }
        FormStep::Submit(values) => {
            remove()?;
            close_buttons(channel, message, &sent, &prompt, answered).await;
            let payload = payload(
                &session,
                message,
                "form_submission",
                "values",
                Value::Object(values),
            )?;
            deliver(channel, message, &session, payload).await?;
        }
        FormStep::Cancel => {
            remove()?;
            close_buttons(channel, message, &sent, &prompt, None).await;
            let payload = closed(&session, message, "cancelled")?;
            deliver(channel, message, &session, payload).await?;
        }
    }
    Ok(true)
}

/// Take the buttons off an answered or abandoned question; best effort.
async fn close_buttons(
    channel: &Object,
    message: &Object,
    sent: &Object,
    prompt: &Object,
    choice: Option<&str>,
) {
    let pending = Python::attach(|py| -> PyResult<Option<Object>> {
        if sent.is_none(py) || prompt.is_none(py) || !channel.bind(py).hasattr("close_prompt")? {
            return Ok(None);
        }
        let answer = match choice {
            Some(choice) => {
                let label = prompt
                    .bind(py)
                    .getattr("choices")?
                    .try_iter()?
                    .filter_map(Result::ok)
                    .find(|c| {
                        c.getattr("id")
                            .and_then(|id| id.extract::<String>())
                            .ok()
                            .as_deref()
                            == Some(choice)
                    })
                    .map(|c| c.getattr("label"))
                    .transpose()?;
                let Some(label) = label else {
                    return Ok(None);
                };
                py.import("agentchat.models")?
                    .getattr("Answer")?
                    .call1((
                        prompt.bind(py).getattr("id")?,
                        choice,
                        label,
                        message.bind(py).getattr("sender")?,
                    ))?
                    .unbind()
            }
            None => py.None(),
        };
        let source = message.bind(py);
        Ok(Some(
            channel
                .bind(py)
                .call_method1(
                    "close_prompt",
                    (source, sent.bind(py), prompt.bind(py), answer),
                )?
                .unbind(),
        ))
    });
    if let Ok(Some(pending)) = pending {
        let _ = await_py(pending).await;
    }
}

fn payload(
    session: &FormSession,
    message: &Object,
    kind: &str,
    name: &str,
    value: Value,
) -> PyResult<Value> {
    Python::attach(|py| {
        let message = message.bind(py);
        Ok(json!({
            "type": kind,
            "callback_id": session.form.callback_id,
            "user_id": message.getattr("sender")?.getattr("id")?.extract::<String>()?,
            "conversation_id": message.getattr("conversation_id")?.extract::<String>()?,
            name: value,
        }))
    })
}

fn closed(session: &FormSession, message: &Object, reason: &str) -> PyResult<Value> {
    payload(session, message, "form_closed", "reason", json!(reason))
}

/// Run the form's `on_view` handler; a text answer is sent as a reply.
async fn deliver(
    channel: &Object,
    message: &Object,
    session: &FormSession,
    payload: Value,
) -> PyResult<()> {
    let pending = Python::attach(|py| -> PyResult<Option<Object>> {
        let route = Route::View {
            callback_id: session.form.callback_id.clone(),
        };
        let Some(handler) = apps::find(channel.bind(py), &route)? else {
            py.import("logging")?
                .call_method1("getLogger", ("agentchat",))?
                .call_method1(
                    "warning",
                    (
                        "No on_view handler for form %r",
                        session.form.callback_id.as_str(),
                    ),
                )?;
            return Ok(None);
        };
        Ok(Some(
            handler
                .bind(py)
                .call1((convert::from_json(py, &payload)?,))?
                .unbind(),
        ))
    })?;
    let Some(pending) = pending else {
        return Ok(());
    };
    let answer = await_py(pending).await?;
    let text = Python::attach(|py| apps::reply_text(answer.bind(py)))?;
    if let Some(text) = text {
        let text = Python::attach(|py| text.into_pyobject(py).map(|t| t.into_any().unbind()))?;
        call(copy(channel), "reply", vec![copy(message), text], vec![]).await?;
    }
    Ok(())
}
