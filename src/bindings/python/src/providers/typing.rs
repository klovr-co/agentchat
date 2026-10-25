//! Keep a provider's short-lived typing indicator visible while a handler works.
//!
//! Discord, Teams, WhatsApp, and iMessage clear their indicator after a few seconds, while
//! `working()` refreshes only once a minute. A keeper sends the indicator once,
//! then repeats it in the background until it is stopped, replaced, closed, or
//! expires, so a forgotten indicator cannot run forever.
use super::transport::{acall, call, j, module, none, sleep};
use crate::host;
use pyo3::{exceptions::PyValueError, prelude::*, types::PyDict, PyTraverseError, PyVisit};
use serde_json::json;

type Output = PyResult<Py<PyAny>>;

#[pyclass(module = "agentchat._native")]
pub struct TypingKeeper {
    interval: f64,
    expiry: f64,
    tasks: Option<Py<PyDict>>,
}

impl TypingKeeper {
    fn tasks(&self, py: Python<'_>) -> PyResult<Py<PyDict>> {
        self.tasks
            .as_ref()
            .map(|tasks| tasks.clone_ref(py))
            .ok_or_else(|| PyValueError::new_err("TypingKeeper has been cleared"))
    }
}

fn cancel(tasks: &Bound<'_, PyDict>, key: &Bound<'_, PyAny>) -> PyResult<()> {
    let task = tasks.call_method1("pop", (key, tasks.py().None()))?;
    if !task.is_none() {
        task.call_method0("cancel")?;
    }
    Ok(())
}

#[pymethods]
impl TypingKeeper {
    #[new]
    fn new(py: Python<'_>, interval: f64, expiry: f64) -> PyResult<Self> {
        if !(interval > 0.0 && expiry > interval) {
            return Err(PyValueError::new_err(
                "Typing interval must be positive and shorter than its expiry",
            ));
        }
        Ok(Self {
            interval,
            expiry,
            tasks: Some(PyDict::new(py).unbind()),
        })
    }

    /// Send the indicator now; errors propagate. Then refresh it in the background.
    fn start(slf: Py<Self>, py: Python<'_>, key: Py<PyAny>, send: Py<PyAny>) -> Output {
        slf.borrow(py).stop(py, key.bind(py))?;
        host::coroutine(py, async move {
            let first = Python::attach(|py| send.bind(py).call0().map(Bound::unbind))?;
            host::await_py(first).await?;
            Python::attach(|py| Self::keep(slf.bind(py), key.bind(py), send.bind(py)))?;
            j(json!(true))
        })
    }

    /// Refresh an indicator the caller already sent, replacing any refresh for `key`.
    fn keep(
        slf: &Bound<'_, Self>,
        key: &Bound<'_, PyAny>,
        send: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let py = slf.py();
        let tasks = slf.borrow().tasks(py)?;
        cancel(tasks.bind(py), key)?;
        let refresh = slf.call_method1("_refresh", (key, send))?;
        let task = py
            .import("asyncio")?
            .call_method1("create_task", (refresh,))?;
        tasks.bind(py).set_item(key, task)
    }

    /// Stop refreshing the indicator for `key`; the provider clears it on its own.
    fn stop(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<()> {
        cancel(self.tasks(py)?.bind(py), key)
    }

    /// Cancel every refresh and wait for them to end.
    fn close(slf: Py<Self>, py: Python<'_>) -> Output {
        host::coroutine(py, async move {
            let tasks = Python::attach(|py| -> PyResult<Vec<Py<PyAny>>> {
                let tasks = slf.borrow(py).tasks(py)?;
                let tasks = tasks.bind(py);
                let pending: Vec<_> = tasks.values().iter().map(Bound::unbind).collect();
                tasks.clear();
                for task in &pending {
                    task.bind(py).call_method0("cancel")?;
                }
                Ok(pending)
            })?;
            if !tasks.is_empty() {
                acall(
                    &module("asyncio")?,
                    "gather",
                    tasks,
                    Some(j(json!({"return_exceptions": true}))?),
                )
                .await?;
            }
            Ok(none())
        })
    }

    fn _refresh(slf: Py<Self>, py: Python<'_>, key: Py<PyAny>, send: Py<PyAny>) -> Output {
        let (interval, expiry) = {
            let this = slf.borrow(py);
            (this.interval, this.expiry)
        };
        host::coroutine(py, async move {
            let loop_obj = call(&module("asyncio")?, "get_running_loop", vec![], None)?;
            let now = || -> PyResult<f64> {
                Python::attach(|py| loop_obj.bind(py).call_method0("time")?.extract())
            };
            let deadline = now()? + expiry;
            while now()? + interval < deadline {
                sleep(interval).await?;
                let sent = Python::attach(|py| send.bind(py).call0().map(Bound::unbind));
                let sent = match sent {
                    Ok(pending) => host::await_py(pending).await,
                    Err(error) => Err(error),
                };
                if let Err(error) = sent {
                    // A missed refresh only hides the indicator; cancellation still ends the task.
                    if Python::attach(|py| {
                        !error.is_instance_of::<pyo3::exceptions::PyException>(py)
                    }) {
                        return Err(error);
                    }
                }
            }
            let current = call(&module("asyncio")?, "current_task", vec![], None)?;
            Python::attach(|py| -> PyResult<()> {
                let tasks = slf.borrow(py).tasks(py)?;
                let tasks = tasks.bind(py);
                let owner = tasks.call_method1("get", (key.bind(py),))?;
                if owner.is(current.bind(py)) {
                    tasks.del_item(key.bind(py))?;
                }
                Ok(())
            })?;
            Ok(none())
        })
    }

    fn __traverse__(&self, visit: PyVisit<'_>) -> Result<(), PyTraverseError> {
        visit.call(&self.tasks)
    }

    fn __clear__(&mut self) {
        self.tasks = None;
    }
}

/// A keeper using the adapter module's `TYPING_INTERVAL` and `TYPING_EXPIRY`, if set.
pub(crate) fn keeper(py: Python<'_>, adapter: &str, interval: f64, expiry: f64) -> Output {
    let adapter = py.import(adapter)?;
    let constant = |name: &str, default: f64| -> f64 {
        adapter
            .getattr(name)
            .and_then(|v| v.extract())
            .unwrap_or(default)
    };
    Ok(Py::new(
        py,
        TypingKeeper::new(
            py,
            constant("TYPING_INTERVAL", interval),
            constant("TYPING_EXPIRY", expiry),
        )?,
    )?
    .into_any())
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<TypingKeeper>()
}
