//! The application's handlers for Slack payloads that are not chat messages.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agentchat_core::Result;
use regex::Regex;
use serde_json::Value;

use super::interaction::Route;

type Reply = Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send>>;

/// A handler receives the raw payload. Its return value becomes the
/// acknowledgement body where Slack accepts one; otherwise it is ignored.
pub type AppHandler = Arc<dyn Fn(Value) -> Reply + Send + Sync>;

pub(crate) fn handler<F, Fut>(handler: F) -> AppHandler
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Value>>> + Send + 'static,
{
    Arc::new(move |payload| Box::pin(handler(payload)))
}

/// Matches an action ID or view callback ID: exactly, or where a pattern finds a
/// match anywhere in it (anchor the pattern to match the whole ID), as in Bolt.
#[derive(Clone, Debug)]
pub enum Matcher {
    Exact(String),
    Pattern(Regex),
}

impl Matcher {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(id) => id == value,
            Self::Pattern(pattern) => pattern.is_match(value),
        }
    }
}

impl From<&str> for Matcher {
    fn from(value: &str) -> Self {
        Self::Exact(value.into())
    }
}

impl From<String> for Matcher {
    fn from(value: String) -> Self {
        Self::Exact(value)
    }
}

impl From<Regex> for Matcher {
    fn from(value: Regex) -> Self {
        Self::Pattern(value)
    }
}

#[derive(Default)]
pub(crate) struct Routes {
    events: Vec<(String, AppHandler)>,
    actions: Vec<(Matcher, AppHandler)>,
    views: Vec<(Matcher, AppHandler)>,
    commands: Vec<(String, AppHandler)>,
    raw: Option<AppHandler>,
}

impl Routes {
    pub(crate) fn event(&mut self, event_type: String, handler: AppHandler) {
        self.events.push((event_type, handler));
    }
    pub(crate) fn action(&mut self, matcher: Matcher, handler: AppHandler) {
        self.actions.push((matcher, handler));
    }
    pub(crate) fn view(&mut self, matcher: Matcher, handler: AppHandler) {
        self.views.push((matcher, handler));
    }
    pub(crate) fn command(&mut self, command: String, handler: AppHandler) {
        self.commands.push((command, handler));
    }
    pub(crate) fn raw(&mut self, handler: AppHandler) {
        self.raw = Some(handler);
    }
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// The first handler registered for the route, else the raw handler.
    pub(crate) fn find(&self, route: &Route) -> Option<AppHandler> {
        let specific = match route {
            Route::Event { event_type } => self
                .events
                .iter()
                .find(|(kind, _)| kind == event_type)
                .map(|(_, handler)| handler),
            Route::Action { action_ids } => self
                .actions
                .iter()
                .find(|(matcher, _)| action_ids.iter().any(|id| matcher.matches(id)))
                .map(|(_, handler)| handler),
            Route::View { callback_id } => self
                .views
                .iter()
                .find(|(matcher, _)| matcher.matches(callback_id))
                .map(|(_, handler)| handler),
            Route::Command { command } => self
                .commands
                .iter()
                .find(|(name, _)| name == command)
                .map(|(_, handler)| handler),
            Route::Other => None,
        };
        specific.or(self.raw.as_ref()).cloned()
    }
}
