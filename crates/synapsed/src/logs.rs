//! Memory-only, bounded logs. Raw tracing fields and formatted errors are never exposed.
use chrono::{SecondsFormat, Utc};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Mutex},
};
use synapsed_api::{LogEntry, LogLevel, MAX_LOG_ENTRIES};
use tokio::sync::broadcast;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

#[derive(Clone)]
pub struct LogRing {
    inner: Arc<Inner>,
}
struct Inner {
    state: Mutex<RingState>,
    events: broadcast::Sender<LogEntry>,
    stderr: bool,
}
struct RingState {
    next: u64,
    entries: VecDeque<LogEntry>,
}
impl LogRing {
    pub fn new(stderr: bool) -> Self {
        let (events, _) = broadcast::channel(MAX_LOG_ENTRIES);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(RingState {
                    next: 0,
                    entries: VecDeque::with_capacity(MAX_LOG_ENTRIES),
                }),
                events,
                stderr,
            }),
        }
    }
    pub fn recent(&self) -> Vec<LogEntry> {
        self.inner
            .state
            .lock()
            .expect("log mutex poisoned")
            .entries
            .iter()
            .cloned()
            .collect()
    }
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.inner.events.subscribe()
    }
    /// Callers supply a static public description; settings, requests and native errors are not messages.
    pub fn record(&self, level: LogLevel, target: &str, message: &'static str) {
        self.append(level, target, message);
    }
    pub fn listening(&self, address: std::net::SocketAddr) {
        self.append(
            LogLevel::Info,
            "synapsed",
            &format!("Local API listening at http://{address}."),
        );
    }
    fn append(&self, level: LogLevel, target: &str, message: &str) {
        let mut state = self.inner.state.lock().expect("log mutex poisoned");
        let Some(next) = state.next.checked_add(1) else {
            return;
        };
        let entry = LogEntry {
            id: state.next.to_string(),
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            level,
            target: target
                .chars()
                .filter(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':')
                })
                .take(128)
                .collect(),
            message: message
                .chars()
                .filter(|character| !character.is_control())
                .take(256)
                .collect(),
        };
        state.next = next;
        if state.entries.len() == MAX_LOG_ENTRIES {
            state.entries.pop_front();
        }
        state.entries.push_back(entry.clone());
        // Keep insertion and broadcast under one lock so concurrent producers preserve sequence order.
        let _ = self.inner.events.send(entry.clone());
        if self.inner.stderr {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{} {:?} {}: {}",
                entry.timestamp,
                entry.level,
                entry.target,
                entry.message
            );
        }
    }
    pub fn tracing_dispatch(&self) -> tracing::Dispatch {
        tracing::Dispatch::new(tracing_subscriber::registry().with(PublicLogLayer(self.clone())))
    }
    pub fn install_tracing(&self) -> Result<(), synapsed_api::ApiError> {
        tracing::dispatcher::set_global_default(self.tracing_dispatch()).map_err(|_| {
            synapsed_api::ApiError::new(
                synapsed_api::ErrorCode::InvalidRequest,
                "Cannot install daemon logging.",
            )
        })
    }
}
struct PublicLogLayer(LogRing);
#[derive(Default)]
struct PublicFields {
    message: Option<String>,
}
impl Visit for PublicFields {
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "public_message" {
            self.message = Some(value.chars().take(256).collect());
        }
    }
}
impl<S: Subscriber> Layer<S> for PublicLogLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::TRACE | tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        };
        // Only this crate's explicit public field is accepted. Dependency messages/errors
        // can contain paths, Lua source, request bodies or configuration and are discarded.
        let mut fields = PublicFields::default();
        if metadata.target() == "synapsed" || metadata.target().starts_with("synapsed::") {
            event.record(&mut fields);
        }
        let fallback = match level {
            LogLevel::Debug => "Runtime diagnostic reported.",
            LogLevel::Info => "Runtime event reported.",
            LogLevel::Warn => "Runtime warning reported.",
            LogLevel::Error => "Runtime error reported.",
        };
        self.0.append(
            level,
            metadata.target(),
            fields.message.as_deref().unwrap_or(fallback),
        );
    }
}
