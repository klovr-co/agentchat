//! Slack policy exports. Interpretation is owned by agentchat-slack.
use super::slack_files::policy_error;
use crate::convert;
use agentchat_slack::{events, rendering};
use pyo3::{exceptions::PyValueError, prelude::*, types::PyTuple};
use serde_json::Value;
#[pyfunction]
#[pyo3(signature=(files,*,limit=5))]
fn slack_attachments(
    py: Python<'_>,
    files: &Bound<'_, PyAny>,
    limit: usize,
) -> PyResult<Py<PyAny>> {
    if !(1..=100).contains(&limit) {
        return Err(PyValueError::new_err(
            "Attachment limit must be between 1 and 100",
        ));
    }
    let files = convert::to_json(files)?;
    Ok(PyTuple::new(
        py,
        events::attachments_bounded(&files, limit)
            .iter()
            .map(|file| convert::to_attachment(py, file))
            .collect::<PyResult<Vec<_>>>()?,
    )?
    .into_any()
    .unbind())
}
#[pyfunction]
fn slack_filename(value: &str) -> String {
    events::filename(value)
}
#[pyfunction]
#[pyo3(signature=(payload,event,*,bot_user_id,workspace_id,channel_name="slack"))]
fn slack_to_message(
    py: Python<'_>,
    payload: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    bot_user_id: Option<&str>,
    workspace_id: Option<&str>,
    channel_name: &str,
) -> PyResult<Py<PyAny>> {
    let mut payload = convert::to_json(payload)?;
    if let Some(object) = payload.as_object_mut() {
        object.insert("event".into(), convert::to_json(event)?);
    }
    events::normalize_scoped(&payload, bot_user_id, workspace_id, channel_name)
        .as_ref()
        .map(|message| convert::to_message(py, message))
        .unwrap_or_else(|| Ok(py.None()))
}
#[pyfunction]
fn slack_text_blocks(py: Python<'_>, text: &str) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &Value::Array(rendering::text_blocks(text).map_err(policy_error)?),
    )
}
#[pyfunction]
fn slack_rich_payload(py: Python<'_>, content: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &rendering::rich_payload(&convert::from_rich_reply(content)?).map_err(policy_error)?,
    )
}
#[pyfunction]
#[pyo3(signature=(reply,*,channel_id,thread_ts=None,message_ts=None,metadata=None,username=None,icon_url=None,fallback_blocks=true))]
#[allow(clippy::too_many_arguments)]
fn slack_message_request(
    py: Python<'_>,
    reply: &Bound<'_, PyAny>,
    channel_id: &str,
    thread_ts: Option<&str>,
    message_ts: Option<&str>,
    metadata: Option<&Bound<'_, PyAny>>,
    username: Option<String>,
    icon_url: Option<String>,
    fallback_blocks: bool,
) -> PyResult<Py<PyAny>> {
    let metadata = metadata.map(convert::to_json).transpose()?;
    if username.is_none() && icon_url.is_some() {
        return Err(PyValueError::new_err(
            "Slack message icon_url requires a username",
        ));
    }
    let author = username
        .map(|username| agentchat_slack::message_request::SlackAuthor { username, icon_url });
    let request = agentchat_slack::message_request::MessageRequest::new(
        &convert::from_rich_reply(reply)?,
        channel_id,
        thread_ts,
        message_ts,
        metadata.as_ref(),
        author.as_ref(),
        fallback_blocks,
    )
    .map_err(policy_error)?;
    convert::from_json(py, &serde_json::json!(request))
}
#[pyfunction]
#[pyo3(signature=(message,*,origin="web"))]
fn slack_mirror_payload(
    py: Python<'_>,
    message: &Bound<'_, PyAny>,
    origin: &str,
) -> PyResult<Py<PyAny>> {
    convert::from_json(
        py,
        &rendering::mirror_payload(&convert::from_message(message)?, origin)
            .map_err(policy_error)?,
    )
}
#[pyfunction]
#[pyo3(signature=(record,*,bot_user_id=None))]
fn slack_read_mirror(
    py: Python<'_>,
    record: &Bound<'_, PyAny>,
    bot_user_id: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let value = convert::to_json(record)?;
    match bot_user_id.and_then(|id| rendering::read_mirror(&value, id)) {
        Some(mirror) => Ok((
            py.import("agentchat.models")?
                .getattr("Sender")?
                .call1((mirror.sender.id, mirror.sender.display_name))?,
            mirror.text,
        )
            .into_pyobject(py)?
            .into_any()
            .unbind()),
        None => Ok(py.None()),
    }
}
/// Whether Slack accepts a response body in this payload's acknowledgement (view
/// submissions, options requests and slash commands). A host-owned source awaits
/// the receiver for these and returns its answer as the response.
#[pyfunction]
fn slack_responds(payload: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(agentchat_slack::interaction::responds(&convert::to_json(
        payload,
    )?))
}
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(slack_responds, module)?)?;
    module.add(
        "SLACK_ACK_DEADLINE",
        agentchat_slack::interaction::ACK_DEADLINE.as_secs_f64(),
    )?;
    module
        .getattr("slack_responds")?
        .setattr("__module__", "agentchat.channels.slack.events")?;
    module.add_function(wrap_pyfunction!(slack_attachments, module)?)?;
    module.add_function(wrap_pyfunction!(slack_filename, module)?)?;
    module.add_function(wrap_pyfunction!(slack_to_message, module)?)?;
    module.add_function(wrap_pyfunction!(slack_text_blocks, module)?)?;
    module.add_function(wrap_pyfunction!(slack_rich_payload, module)?)?;
    module.add_function(wrap_pyfunction!(slack_message_request, module)?)?;
    module.add_function(wrap_pyfunction!(slack_mirror_payload, module)?)?;
    module.add_function(wrap_pyfunction!(slack_read_mirror, module)?)?;
    module
        .getattr("slack_attachments")?
        .setattr("__module__", "agentchat.channels.slack.events")?;
    module
        .getattr("slack_filename")?
        .setattr("__module__", "agentchat.channels.slack.events")?;
    module
        .getattr("slack_to_message")?
        .setattr("__module__", "agentchat.channels.slack.events")?;
    module
        .getattr("slack_text_blocks")?
        .setattr("__module__", "agentchat.channels.slack.rendering")?;
    module
        .getattr("slack_rich_payload")?
        .setattr("__module__", "agentchat.channels.slack.rendering")?;
    module
        .getattr("slack_message_request")?
        .setattr("__module__", "agentchat.channels.slack.rendering")?;
    module
        .getattr("slack_mirror_payload")?
        .setattr("__module__", "agentchat.channels.slack.mirroring")?;
    module
        .getattr("slack_read_mirror")?
        .setattr("__module__", "agentchat.channels.slack.mirroring")?;
    Ok(())
}
