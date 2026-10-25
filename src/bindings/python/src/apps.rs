//! The application's handlers for provider payloads that are not chat messages.
//!
//! Every channel registers handlers the same way (`on_event`, `on_action`,
//! `on_view`, `on_command`, `on_raw`); the provider decides each payload's
//! `Route`. Handlers receive the raw payload. Where the provider's acknowledgement
//! can carry an answer, the handler's return value is that answer if it arrives
//! within `ACK_DEADLINE`; a slower handler keeps running as drained work.
use crate::host::await_py;
use agentchat_core::Route;
use pyo3::{
    exceptions::PyTypeError,
    prelude::*,
    types::{PyDict, PyList},
    PyTraverseError, PyVisit,
};
use std::time::Duration;

type Object = Py<PyAny>;

/// How long a channel waits for a handler whose answer it sends back to the
/// provider, leaving room within the provider's three-second limits.
pub const ACK_DEADLINE: Duration = Duration::from_millis(2_500);

#[pyclass(module = "agentchat._native")]
pub struct AppRoutes {
    /// `(kind, key, handler)` tuples in registration order.
    routes: Option<Py<PyList>>,
}

#[pymethods]
impl AppRoutes {
    fn _register(
        &self,
        py: Python<'_>,
        kind: &str,
        key: Object,
        handler: Object,
    ) -> PyResult<Object> {
        if !handler.bind(py).is_callable() {
            return Err(PyTypeError::new_err("A handler must be callable"));
        }
        self.list(py)?.append((kind, key, handler.clone_ref(py)))?;
        Ok(handler)
    }
    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.routes)
    }
    fn __clear__(&mut self) {
        self.routes = None;
    }
}

impl AppRoutes {
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        self.routes
            .as_ref()
            .map(|routes| routes.bind(py).clone())
            .ok_or_else(|| PyTypeError::new_err("Handlers have been cleared"))
    }

    /// The first handler registered for `route`, else the raw handler.
    fn find(&self, py: Python<'_>, route: &Route) -> PyResult<Option<Object>> {
        let (kind, keys): (&str, Vec<&str>) = match route {
            Route::Event { event_type } => ("event", vec![event_type.as_str()]),
            Route::Action { action_ids } => {
                ("action", action_ids.iter().map(String::as_str).collect())
            }
            Route::View { callback_id } => ("view", vec![callback_id.as_str()]),
            Route::Command { command } => ("command", vec![command.as_str()]),
            Route::Other => ("other", vec![]),
        };
        let mut raw = None;
        for entry in self.list(py)?.iter() {
            let (entry_kind, key, handler): (String, Bound<'_, PyAny>, Bound<'_, PyAny>) =
                entry.extract()?;
            if entry_kind == "raw" {
                raw = Some(handler.unbind());
                continue;
            }
            if entry_kind != kind {
                continue;
            }
            for value in &keys {
                let matched = match key.extract::<String>() {
                    Ok(exact) => exact == *value,
                    Err(_) => !key.call_method1("search", (*value,))?.is_none(),
                };
                if matched {
                    return Ok(Some(handler.unbind()));
                }
            }
        }
        Ok(raw)
    }
}

/// The channel's handlers, created on first use and kept in its `_apps` attribute.
fn routes_of(channel: &Bound<'_, PyAny>) -> PyResult<Py<AppRoutes>> {
    if let Ok(routes) = channel.getattr("_apps") {
        if let Ok(routes) = routes.extract::<Py<AppRoutes>>() {
            return Ok(routes);
        }
    }
    let py = channel.py();
    let routes = Py::new(
        py,
        AppRoutes {
            routes: Some(PyList::empty(py).unbind()),
        },
    )?;
    channel.setattr("_apps", routes.clone_ref(py))?;
    Ok(routes)
}

/// A decorator registering a handler of `kind` for `key`: a string, or for
/// actions and views also a compiled pattern whose `search` matches.
pub fn decorator(channel: &Bound<'_, PyAny>, kind: &str, key: Object) -> PyResult<Object> {
    let py = channel.py();
    let text = key.bind(py).extract::<String>().is_ok();
    let exact_only = matches!(kind, "event" | "command");
    if !text && (exact_only || !key.bind(py).hasattr("search")?) {
        return Err(PyTypeError::new_err(if exact_only {
            "Expected a string"
        } else {
            "Expected a string or a compiled pattern"
        }));
    }
    let routes = routes_of(channel)?;
    Ok(py
        .import("functools")?
        .getattr("partial")?
        .call1((routes.bind(py).getattr("_register")?, kind, key))?
        .unbind())
}

/// Register the handler for every payload no other handler matched.
pub fn raw(channel: &Bound<'_, PyAny>, handler: Object) -> PyResult<Object> {
    let py = channel.py();
    let routes = routes_of(channel)?;
    let this = routes.borrow(py);
    this._register(py, "raw", py.None(), handler)
}

/// The handler for `route`, if the application registered one.
pub fn find(channel: &Bound<'_, PyAny>, route: &Route) -> PyResult<Option<Object>> {
    let Ok(routes) = channel.getattr("_apps") else {
        return Ok(None);
    };
    let Ok(routes) = routes.extract::<Py<AppRoutes>>() else {
        return Ok(None);
    };
    let py = channel.py();
    let found = routes.borrow(py).find(py, route);
    found
}

/// The event types the application registered handlers for, by exact name.
pub fn event_types(channel: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    let Ok(routes) = channel.getattr("_apps") else {
        return Ok(Vec::new());
    };
    let Ok(routes) = routes.extract::<Py<AppRoutes>>() else {
        return Ok(Vec::new());
    };
    let py = channel.py();
    let list = routes.borrow(py).list(py)?;
    let mut types = Vec::new();
    for entry in list.iter() {
        let (kind, key, _): (String, Bound<'_, PyAny>, Bound<'_, PyAny>) = entry.extract()?;
        if kind == "event" {
            if let Ok(key) = key.extract::<String>() {
                types.push(key);
            }
        }
    }
    Ok(types)
}

/// Run `handler(payload)` as drained work on the channel's delivery tasks, without
/// waiting for it. False when the channel is closed or busy.
pub fn spawn(
    channel: &Bound<'_, PyAny>,
    delivery: &Bound<'_, PyAny>,
    handler: &Object,
    payload: &Object,
) -> PyResult<bool> {
    let py = channel.py();
    let pending = handler.bind(py).call1((payload.bind(py),))?;
    delivery
        .call_method1("spawn", (channel, pending))?
        .is_truthy()
}

/// Run `handler(payload)` and return its answer if it arrives within the
/// acknowledgement deadline; otherwise None, and the handler keeps running as
/// drained work. A handler that fails in time raises.
pub async fn answer(delivery: Object, handler: Object, payload: Object) -> PyResult<Object> {
    let task = Python::attach(|py| -> PyResult<Object> {
        let pending = handler.bind(py).call1((payload.bind(py),))?;
        Ok(py
            .import("asyncio")?
            .call_method1("ensure_future", (pending,))?
            .unbind())
    })?;
    let waiting = Python::attach(|py| -> PyResult<Object> {
        let kwargs = PyDict::new(py);
        kwargs.set_item("timeout", ACK_DEADLINE.as_secs_f64())?;
        Ok(py
            .import("asyncio")?
            .call_method("wait", ((task.bind(py),),), Some(&kwargs))?
            .unbind())
    })?;
    await_py(waiting).await?;
    Python::attach(|py| {
        let pending = task.bind(py);
        if pending.call_method0("done")?.is_truthy()? {
            return Ok(pending.call_method0("result")?.unbind());
        }
        py.import("logging")?
            .call_method1("getLogger", ("agentchat._delivery",))?
            .call_method1(
                "warning",
                ("A handler missed the acknowledgement deadline",),
            )?;
        delivery.bind(py).call_method1("track", (pending,))?;
        Ok(py.None())
    })
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AppRoutes>()?;
    module.add("ACK_DEADLINE", ACK_DEADLINE.as_secs_f64())
}
