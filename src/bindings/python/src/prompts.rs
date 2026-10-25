//! Questions with buttons, and the presses that answer them.
//!
//! A prompt is answered once, only from its own conversation, only with one of
//! its choices, and only by someone allowed to answer it: by default the person
//! whose message it is about. Anything else is ignored. A prompt nobody answers
//! fails closed: `ask()` raises `TimeoutError` rather than returning a choice.
use crate::control::RunningTurn;
use crate::host::await_py;
use crate::runtime::{call, copy, none};
use pyo3::{
    exceptions::{PyTimeoutError, PyTypeError, PyValueError},
    prelude::*,
    types::{PyDict, PyList, PyTuple},
    PyTraverseError, PyVisit,
};
use serde_json::Value;

type Object = Py<PyAny>;

/// What a prompt is for, and what an answer to it does.
enum Purpose {
    /// `ask()`: the answer resolves this future.
    Ask(Object),
    /// A Stop button: the answer asks this turn to stop.
    Stop(Py<RunningTurn>),
    /// "Queue or steer?" for a message that arrived while this turn ran.
    Busy(Object, Py<RunningTurn>),
}

#[pyclass(module = "agentchat._native")]
pub struct PendingPrompt {
    prompt: Object,
    conversation: String,
    /// (id, label) of each choice.
    choices: Vec<(String, String)>,
    /// Sender IDs that may answer; None means anyone in the conversation.
    allowed: Option<Vec<String>>,
    channel: Object,
    source: Object,
    sent: Option<Object>,
    purpose: Purpose,
}

#[pymethods]
impl PendingPrompt {
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.prompt)?;
        visit.call(&self.channel)?;
        visit.call(&self.source)?;
        visit.call(&self.sent)?;
        match &self.purpose {
            Purpose::Ask(future) => visit.call(future),
            Purpose::Stop(turn) => visit.call(turn),
            Purpose::Busy(message, turn) => {
                visit.call(message)?;
                visit.call(turn)
            }
        }
    }
}

/// Who may answer, from `allowed`: None (the source's sender), "anyone", or sender IDs.
fn allowed_senders(
    source: &Bound<'_, PyAny>,
    allowed: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<Vec<String>>> {
    match allowed {
        None => Ok(Some(vec![source
            .getattr("sender")?
            .getattr("id")?
            .extract()?])),
        Some(value) if value.extract::<String>().is_ok_and(|v| v == "anyone") => Ok(None),
        Some(value) if value.is_instance_of::<pyo3::types::PyString>() => Err(
            PyValueError::new_err("allowed must be 'anyone' or a sequence of sender IDs"),
        ),
        Some(value) => {
            let ids = value
                .try_iter()?
                .map(|id| id?.extract::<String>())
                .collect::<PyResult<Vec<_>>>()?;
            if ids.is_empty() {
                return Err(PyValueError::new_err(
                    "allowed must name at least one sender",
                ));
            }
            Ok(Some(ids))
        }
    }
}

/// Build and check a `Prompt`; plain strings become choices with IDs "1", "2", ….
fn build_prompt(
    py: Python<'_>,
    text: &str,
    choices: &Bound<'_, PyAny>,
) -> PyResult<(Object, Vec<(String, String)>)> {
    let models = py.import("agentchat.models")?;
    let choice_class = models.getattr("Choice")?;
    let mut built = Vec::new();
    let mut pairs = Vec::new();
    for (index, item) in choices.try_iter()?.enumerate() {
        let item = item?;
        let choice = if item.is_instance(&choice_class)? {
            item
        } else if let Ok(label) = item.extract::<String>() {
            choice_class.call1(((index + 1).to_string(), label))?
        } else {
            return Err(PyTypeError::new_err(
                "Choices must be Choice objects or labels",
            ));
        };
        pairs.push((
            choice.getattr("id")?.extract::<String>()?,
            choice.getattr("label")?.extract::<String>()?,
        ));
        built.push(choice);
    }
    agentchat_core::validate_prompt(text, pairs.iter().map(|(i, l)| (i.as_str(), l.as_str())))
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    let id: String = py
        .import("secrets")?
        .call_method1("token_hex", (8,))?
        .extract()?;
    let prompt = models
        .getattr("Prompt")?
        .call1((id, text, PyTuple::new(py, built)?))?;
    Ok((prompt.unbind(), pairs))
}

fn interactive(py: Python<'_>, channel: &Bound<'_, PyAny>) -> PyResult<bool> {
    channel.is_instance(
        &py.import("agentchat.channels.base")?
            .getattr("InteractiveChannel")?,
    )
}

fn prompt_id(py: Python<'_>, prompt: &Object) -> PyResult<String> {
    prompt.bind(py).getattr("id")?.extract()
}

/// The registry of an application's unanswered prompts, by prompt ID.
pub struct Prompts(pub Py<PyDict>);

impl Prompts {
    fn register(&self, py: Python<'_>, pending: PendingPrompt) -> PyResult<String> {
        let id = prompt_id(py, &pending.prompt)?;
        self.0.bind(py).set_item(&id, Py::new(py, pending)?)?;
        Ok(id)
    }
    fn take(&self, py: Python<'_>, id: &str) -> PyResult<Option<Py<PendingPrompt>>> {
        let dict = self.0.bind(py);
        let Some(item) = dict.get_item(id)? else {
            return Ok(None);
        };
        dict.del_item(id)?;
        Ok(Some(item.extract()?))
    }
    fn get(&self, py: Python<'_>, id: &str) -> PyResult<Option<Py<PendingPrompt>>> {
        Ok(match self.0.bind(py).get_item(id)? {
            Some(item) => Some(item.extract()?),
            None => None,
        })
    }
}

/// Show `prompt` and remember where it went; returns the shown message.
async fn send(
    prompts: &Prompts,
    id: &str,
    channel: &Object,
    source: &Object,
    prompt: &Object,
) -> PyResult<Object> {
    let sent = call(
        copy(channel),
        "send_prompt",
        vec![copy(source), copy(prompt)],
        vec![],
    )
    .await;
    let sent = match sent {
        Ok(sent) => sent,
        Err(error) => {
            Python::attach(|py| prompts.take(py, id))?;
            return Err(error);
        }
    };
    let still_open = Python::attach(|py| -> PyResult<bool> {
        match prompts.get(py, id)? {
            Some(pending) => {
                pending.borrow_mut(py).sent = Some(sent.clone_ref(py));
                Ok(true)
            }
            None => Ok(false),
        }
    })?;
    if !still_open {
        // Answered or ended while it was being sent; take the buttons away again.
        close(channel, source, &sent, prompt, None).await;
    }
    Ok(sent)
}

/// Remove a prompt's buttons; a failure only leaves stale buttons behind.
async fn close(
    channel: &Object,
    source: &Object,
    sent: &Object,
    prompt: &Object,
    answer: Option<Object>,
) {
    let answer = answer.unwrap_or_else(none);
    let _ = call(
        copy(channel),
        "close_prompt",
        vec![copy(source), copy(sent), copy(prompt), answer],
        vec![],
    )
    .await;
}

/// `ask()`: show a prompt and wait for its answer or the timeout.
pub async fn ask(
    prompts: Prompts,
    channel: Object,
    source: Object,
    text: String,
    choices: Object,
    timeout: f64,
    allowed: Option<Object>,
) -> PyResult<Object> {
    let (prompt, id, future) = Python::attach(|py| -> PyResult<_> {
        if !interactive(py, channel.bind(py))? {
            return Err(PyTypeError::new_err(
                "This channel does not support prompts",
            ));
        }
        if !(timeout.is_finite() && timeout > 0.0) {
            return Err(PyValueError::new_err(
                "Prompt timeout must be positive seconds",
            ));
        }
        let (prompt, pairs) = build_prompt(py, &text, choices.bind(py))?;
        let allowed = allowed_senders(source.bind(py), allowed.as_ref().map(|a| a.bind(py)))?;
        let future = py
            .import("asyncio")?
            .call_method0("get_running_loop")?
            .call_method0("create_future")?
            .unbind();
        let conversation = source.bind(py).getattr("conversation_id")?.extract()?;
        let id = prompts.register(
            py,
            PendingPrompt {
                prompt: prompt.clone_ref(py),
                conversation,
                choices: pairs,
                allowed,
                channel: channel.clone_ref(py),
                source: source.clone_ref(py),
                sent: None,
                purpose: Purpose::Ask(future.clone_ref(py)),
            },
        )?;
        Ok((prompt, id, future))
    })?;
    send(&prompts, &id, &channel, &source, &prompt).await?;
    let waited = Python::attach(|py| -> PyResult<Object> {
        let asyncio = py.import("asyncio")?;
        let shielded = asyncio.call_method1("shield", (future.bind(py),))?;
        Ok(asyncio
            .call_method1("wait_for", (shielded, timeout))?
            .unbind())
    })?;
    match await_py(waited).await {
        Ok(answer) => Ok(answer),
        Err(error) => {
            // Timed out or cancelled: nobody can answer it any more.
            let pending = Python::attach(|py| prompts.take(py, &id))?;
            if let Some(pending) = pending {
                let sent =
                    Python::attach(|py| pending.borrow(py).sent.as_ref().map(|s| s.clone_ref(py)));
                if let Some(sent) = sent {
                    close(&channel, &source, &sent, &prompt, None).await;
                }
            }
            if Python::attach(|py| error.is_instance_of::<PyTimeoutError>(py)) {
                return Err(PyTimeoutError::new_err(
                    "Nobody answered the prompt in time",
                ));
            }
            Err(error)
        }
    }
}

/// A Stop button for the running turn; None on channels without buttons.
pub async fn offer_stop(
    prompts: Prompts,
    channel: Object,
    source: Object,
    turn: Py<RunningTurn>,
    text: String,
    label: String,
    allowed: Option<Object>,
) -> PyResult<Object> {
    let registered = Python::attach(|py| -> PyResult<Option<(Object, String)>> {
        if !interactive(py, channel.bind(py))? {
            return Ok(None);
        }
        let choice = py
            .import("agentchat.models")?
            .getattr("Choice")?
            .call1(("stop", label, "danger"))?;
        let (prompt, pairs) = build_prompt(py, &text, PyList::new(py, [choice])?.as_any())?;
        let allowed = allowed_senders(source.bind(py), allowed.as_ref().map(|a| a.bind(py)))?;
        let conversation = source.bind(py).getattr("conversation_id")?.extract()?;
        let id = prompts.register(
            py,
            PendingPrompt {
                prompt: prompt.clone_ref(py),
                conversation,
                choices: pairs,
                allowed,
                channel: channel.clone_ref(py),
                source: source.clone_ref(py),
                sent: None,
                purpose: Purpose::Stop(turn.clone_ref(py)),
            },
        )?;
        turn.borrow_mut(py).prompts.push(id.clone());
        Ok(Some((prompt, id)))
    })?;
    match registered {
        Some((prompt, id)) => send(&prompts, &id, &channel, &source, &prompt).await,
        None => Ok(none()),
    }
}

/// Ask `message`'s sender whether it should wait for `turn` or steer it.
pub async fn offer_busy(
    prompts: Prompts,
    channel: Object,
    message: Object,
    turn: Py<RunningTurn>,
) -> PyResult<()> {
    let (prompt, id) = Python::attach(|py| -> PyResult<_> {
        let choice = py.import("agentchat.models")?.getattr("Choice")?;
        let choices = PyList::new(
            py,
            [
                choice.call1(("steer", "Add to current request", "primary"))?,
                choice.call1(("queue", "Answer after it"))?,
            ],
        )?;
        let (prompt, pairs) = build_prompt(
            py,
            "I'm still working on the previous request. What should I do with this message?",
            choices.as_any(),
        )?;
        let allowed = allowed_senders(message.bind(py), None)?;
        let conversation = message.bind(py).getattr("conversation_id")?.extract()?;
        let id = prompts.register(
            py,
            PendingPrompt {
                prompt: prompt.clone_ref(py),
                conversation,
                choices: pairs,
                allowed,
                channel: channel.clone_ref(py),
                source: message.clone_ref(py),
                sent: None,
                purpose: Purpose::Busy(message.clone_ref(py), turn.clone_ref(py)),
            },
        )?;
        turn.borrow_mut(py).prompts.push(id.clone());
        Ok((prompt, id))
    })?;
    send(&prompts, &id, &channel, &message, &prompt).await?;
    Ok(())
}

/// The prompt and choice an inbound message answers, if it is a button press.
pub fn answer_of(message: &Bound<'_, PyAny>) -> PyResult<Option<(String, String)>> {
    let metadata = message.getattr("metadata")?;
    let Some(answer) = metadata
        .call_method1("get", (agentchat_core::ANSWER_METADATA,))
        .ok()
        .filter(|a| !a.is_none())
    else {
        return Ok(None);
    };
    let answer: Value = crate::convert::to_json(&answer)?;
    Ok(
        match (answer["prompt"].as_str(), answer["choice"].as_str()) {
            (Some(prompt), Some(choice)) => Some((prompt.to_owned(), choice.to_owned())),
            _ => None,
        },
    )
}

/// Apply a button press; a press that does not match an open prompt is ignored.
pub async fn resolve(
    prompts: Prompts,
    state: Object,
    press: Object,
    prompt_id: String,
    choice: String,
) -> PyResult<()> {
    let accepted = Python::attach(|py| -> PyResult<Option<(Py<PendingPrompt>, Object)>> {
        let Some(pending) = prompts.get(py, &prompt_id)? else {
            return Ok(None);
        };
        let this = pending.borrow(py);
        let press = press.bind(py);
        let sender = press.getattr("sender")?;
        let sender_id: String = sender.getattr("id")?.extract()?;
        let conversation: String = press.getattr("conversation_id")?.extract()?;
        let label = this
            .choices
            .iter()
            .find(|(id, _)| *id == choice)
            .map(|(_, label)| label.clone());
        let allowed = this
            .allowed
            .as_ref()
            .is_none_or(|ids| ids.contains(&sender_id));
        let (Some(label), true, true) = (label, allowed, conversation == this.conversation) else {
            return Ok(None);
        };
        let answer = py
            .import("agentchat.models")?
            .getattr("Answer")?
            .call1((&prompt_id, &choice, label, sender))?;
        drop(this);
        prompts.take(py, &prompt_id)?;
        Ok(Some((pending, answer.unbind())))
    })?;
    let Some((pending, answer)) = accepted else {
        return Ok(());
    };
    let (channel, source, sent, prompt, held) = Python::attach(|py| -> PyResult<_> {
        let this = pending.borrow(py);
        let mut held = None;
        match &this.purpose {
            Purpose::Ask(future) => {
                let future = future.bind(py);
                if !future.call_method0("done")?.is_truthy()? {
                    future.call_method1("set_result", (answer.bind(py),))?;
                }
            }
            Purpose::Stop(turn) => turn.borrow(py).request_stop(py)?,
            Purpose::Busy(message, turn) => {
                let turn = turn.borrow(py);
                if choice == "steer" {
                    turn.steer(py, message.bind(py))?;
                } else {
                    turn.queue(py, message.bind(py))?;
                }
                held = Some(message.clone_ref(py));
            }
        }
        Ok((
            this.channel.clone_ref(py),
            this.source.clone_ref(py),
            this.sent.as_ref().map(|s| s.clone_ref(py)),
            this.prompt.clone_ref(py),
            held,
        ))
    })?;
    if let Some(message) = held {
        call(state, "append", vec![message], vec![]).await?;
    }
    if let Some(sent) = sent {
        close(&channel, &source, &sent, &prompt, Some(answer)).await;
    }
    Ok(())
}

/// Take the prompts a finished turn owned, so no press can reach the ended turn.
/// Call in the same step that unregisters the turn.
pub fn take_owned(
    py: Python<'_>,
    prompts: &Prompts,
    turn: &Py<RunningTurn>,
) -> PyResult<Vec<Py<PendingPrompt>>> {
    let owned = std::mem::take(&mut turn.borrow_mut(py).prompts);
    let mut taken = Vec::new();
    for id in owned {
        if let Some(pending) = prompts.take(py, &id)? {
            taken.push(pending);
        }
    }
    Ok(taken)
}

/// Close a finished turn's prompts. Returns held messages nobody decided on; they
/// run next, as if queued.
pub async fn close_owned(state: &Object, owned: Vec<Py<PendingPrompt>>) -> PyResult<Vec<Object>> {
    let mut undecided = Vec::new();
    for pending in owned {
        let (channel, source, sent, prompt, held) = Python::attach(|py| {
            let this = pending.borrow(py);
            (
                this.channel.clone_ref(py),
                this.source.clone_ref(py),
                this.sent.as_ref().map(|s| s.clone_ref(py)),
                this.prompt.clone_ref(py),
                match &this.purpose {
                    Purpose::Busy(message, _) => Some(message.clone_ref(py)),
                    _ => None,
                },
            )
        });
        if let Some(message) = held {
            call(copy(state), "append", vec![copy(&message)], vec![]).await?;
            undecided.push(message);
        }
        if let Some(sent) = sent {
            close(&channel, &source, &sent, &prompt, None).await;
        }
    }
    Ok(undecided)
}

/// A prompt's ID, text, and (id, label, style) choices, for channels to render.
pub(crate) struct PromptFields {
    pub id: String,
    pub text: String,
    pub choices: Vec<(String, String, String)>,
}

impl PromptFields {
    pub fn read(prompt: &Bound<'_, PyAny>) -> PyResult<Self> {
        let choices = prompt
            .getattr("choices")?
            .try_iter()?
            .map(|choice| {
                let choice = choice?;
                Ok((
                    choice.getattr("id")?.extract()?,
                    choice.getattr("label")?.extract()?,
                    choice.getattr("style")?.extract()?,
                ))
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            id: prompt.getattr("id")?.extract()?,
            text: prompt.getattr("text")?.extract()?,
            choices,
        })
    }
    pub fn pairs(&self) -> Vec<(String, String)> {
        self.choices
            .iter()
            .map(|(id, label, _)| (id.clone(), label.clone()))
            .collect()
    }
    /// The text to show once answered, or None when the prompt just expired.
    pub fn closed_text(&self, answer: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
        if answer.is_none() {
            return Ok(None);
        }
        let label: String = answer.getattr("label")?.extract()?;
        let sender = answer.getattr("sender")?;
        let who = sender
            .getattr("display_name")?
            .extract::<Option<String>>()?
            .filter(|name| !name.trim().is_empty())
            .map_or_else(|| sender.getattr("id")?.extract::<String>(), Ok)?;
        Ok(Some(agentchat_core::answered_text(
            &self.text, &label, &who,
        )))
    }
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PendingPrompt>()?;
    module.add_function(wrap_pyfunction!(prompt_token, module)?)
}

/// The value a button for `choice` of prompt `prompt` must carry.
#[pyfunction]
fn prompt_token(prompt: &str, choice: &str) -> PyResult<String> {
    agentchat_core::prompt_token(prompt, choice).map_err(|e| PyValueError::new_err(e.to_string()))
}
