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
    events: broadcast::Sender<(u64, LogEntry)>,
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
    pub fn subscribe(&self) -> broadcast::Receiver<(u64, LogEntry)> {
        self.inner.events.subscribe()
    }
    pub fn recent_with_sequence(&self) -> Vec<(u64, LogEntry)> {
        let state = self.inner.state.lock().expect("log mutex poisoned");
        let first = state
            .next
            .saturating_sub(u64::try_from(state.entries.len()).unwrap_or(u64::MAX));
        state
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                (
                    first + u64::try_from(index).unwrap_or(u64::MAX),
                    entry.clone(),
                )
            })
            .collect()
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
        let entry = {
            let mut state = self.inner.state.lock().expect("log mutex poisoned");
            let Some(next) = state.next.checked_add(1) else {
                return;
            };
            let sequence = state.next;
            let entry = LogEntry {
                id: sequence.to_string(),
                timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                level,
                target: target
                    .chars()
                    .filter(|character| {
                        character.is_ascii_alphanumeric()
                            || matches!(character, '_' | '-' | '.' | ':')
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
            let _ = self.inner.events.send((sequence, entry.clone()));
            entry
        };
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
struct BoundedMessage {
    value: String,
    remaining: usize,
}
impl std::fmt::Write for BoundedMessage {
    fn write_str(&mut self, input: &str) -> Result<(), std::fmt::Error> {
        let mut end = 0;
        let mut count = 0;
        for (index, character) in input.char_indices() {
            if count == self.remaining {
                break;
            }
            end = index + character.len_utf8();
            count += 1;
        }
        self.value.push_str(&input[..end]);
        self.remaining -= count;
        Ok(())
    }
}
impl Visit for PublicFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "public_message" {
            let mut message = BoundedMessage {
                value: String::with_capacity(256),
                remaining: 256,
            };
            let _ = std::fmt::write(&mut message, format_args!("{value:?}"));
            self.message = Some(message.value);
        }
    }
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
