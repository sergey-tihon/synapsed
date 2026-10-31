//! The worker carries only owned protocol data. No native menu handle leaves main.
use crate::{UiEvent, menu::CommandBinding};
use std::sync::{Arc, Mutex, MutexGuard};
use synapsed_api::Snapshot;
use synapsed_client::{Client, ClientError, ClientEvent, config, reconnect_delay};
use tao::event_loop::EventLoopProxy;
use tokio::sync::{mpsc, watch};
use url::Url;

pub const MAX_PENDING: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Online,
    Offline,
    Incompatible,
}
impl Connection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Online => "Online",
            Self::Offline => "Offline",
            Self::Incompatible => "Incompatible",
        }
    }
}

pub struct Completion {
    pub token: u64,
    pub binding: CommandBinding,
    pub error: Option<String>,
}
#[derive(Default)]
struct CoalescingWake {
    pending: bool,
}
impl CoalescingWake {
    fn request(&mut self) -> bool {
        let should_wake = !self.pending;
        self.pending = true;
        should_wake
    }
    fn take(&mut self) {
        self.pending = false;
    }
}
#[cfg(test)]
mod wake_tests {
    use super::CoalescingWake;
    #[test]
    fn coalesces_until_taken() {
        let mut wake = CoalescingWake::default();
        assert!(wake.request());
        assert!(!wake.request());
        wake.take();
        assert!(wake.request());
    }
}
pub struct View {
    pub snapshot: Option<Arc<Snapshot>>,
    pub connection: Connection,
    pub error: Option<String>,
    pub completions: Vec<Completion>,
    wake: CoalescingWake,
}
impl Default for View {
    fn default() -> Self {
        Self {
            snapshot: None,
            connection: Connection::Connecting,
            error: None,
            completions: Vec::with_capacity(MAX_PENDING),
            wake: CoalescingWake::default(),
        }
    }
}

/// One wake event and one latest Arc, even while Cocoa is tracking a menu.
#[derive(Clone)]
pub struct Mailbox {
    inner: Arc<Mutex<View>>,
    proxy: EventLoopProxy<UiEvent>,
}
impl Mailbox {
    pub fn new(proxy: EventLoopProxy<UiEvent>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(View::default())),
            proxy,
        }
    }
    fn lock(&self) -> MutexGuard<'_, View> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn update(&self, change: impl FnOnce(&mut View)) {
        let should_wake = {
            let mut view = self.lock();
            change(&mut view);
            view.wake.request()
        };
        if should_wake {
            let _ = self.proxy.send_event(UiEvent::Wake);
        }
    }
    pub fn take(&self) -> View {
        let mut view = self.lock();
        view.wake.take();
        View {
            snapshot: view.snapshot.clone(),
            connection: view.connection,
            error: view.error.clone(),
            completions: std::mem::take(&mut view.completions),
            wake: CoalescingWake::default(),
        }
    }
    fn state(&self, client: &Client, connection: Connection, error: Option<String>) {
        self.update(|view| {
            view.snapshot = client.cached_snapshot();
            view.connection = connection;
            view.error = error;
        });
    }
    fn complete(
        &self,
        client: &Client,
        token: u64,
        binding: CommandBinding,
        result: Result<Arc<Snapshot>, ClientError>,
    ) {
        self.update(|view| {
            // Read the shared cache, not the initiating HTTP response. A WS observation
            // can have advanced it again between response reconciliation and this wake.
            view.snapshot = client.cached_snapshot();
            if let Err(error) = &result {
                if error.is_terminal() {
                    view.connection = Connection::Incompatible;
                    view.error = Some(error.to_string());
                } else if client.is_stale() {
                    view.connection = Connection::Offline;
                }
            }
            // The main thread admits at most MAX_PENDING commands. It drains these
            // receipts before admitting more, so this vector is bounded by that cap.
            view.completions.push(Completion {
                token,
                binding,
                error: result.err().map(|error| error.to_string()),
            });
        });
    }
}

pub struct Request {
    pub token: u64,
    pub binding: CommandBinding,
}
pub struct Worker {
    pub endpoint: Option<Url>,
    pub requests: mpsc::Sender<Request>,
    cancel: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    pub fn start(mailbox: Mailbox) -> Self {
        let (requests, receiver) = mpsc::channel(MAX_PENDING);
        let (cancel, cancellation) = watch::channel(false);
        let endpoint = match config::load_endpoint(None, None) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                eprintln!("Synapsed tray: {error}");
                mailbox.update(|view| {
                    view.connection = Connection::Incompatible;
                    view.error = Some(error.to_string());
                });
                return Self {
                    endpoint: None,
                    requests,
                    cancel,
                    thread: None,
                };
            }
        };
        let worker_endpoint = endpoint.clone();
        let failure_mailbox = mailbox.clone();
        let thread = std::thread::Builder::new()
            .name("synapsed-tray-client".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => {
                        mailbox.update(|view| {
                            view.connection = Connection::Incompatible;
                            view.error = Some("Cannot initialize client runtime.".into());
                        });
                        return;
                    }
                };
                runtime.block_on(run(worker_endpoint, mailbox, receiver, cancellation));
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(_) => {
                failure_mailbox.update(|view| {
                    view.connection = Connection::Incompatible;
                    view.error = Some("Cannot start client worker.".into());
                });
                None
            }
        };
        Self {
            endpoint: Some(endpoint),
            requests,
            cancel,
            thread,
        }
    }
    /// Signal only: no join or network wait inside a Cocoa callback.
    pub fn stop(&self) {
        self.cancel.send_replace(true);
    }
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
    pub fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Called only after is_finished, never a blocking shutdown join.
            if thread.join().is_err() {
                eprintln!("Synapsed tray: client worker stopped unexpectedly.");
            }
        }
    }
}

async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    loop {
        if *cancel.borrow() {
            return;
        }
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

async fn run(
    endpoint: Url,
    mailbox: Mailbox,
    mut requests: mpsc::Receiver<Request>,
    mut cancel: watch::Receiver<bool>,
) {
    let client = match Client::new(endpoint) {
        Ok(client) => client,
        Err(error) => {
            mailbox.update(|view| {
                view.connection = Connection::Incompatible;
                view.error = Some(error.to_string());
            });
            return;
        }
    };
    let mut subscription = tokio::spawn(subscribe(client.clone(), mailbox.clone(), cancel.clone()));
    loop {
        let request = tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => break,
            request = requests.recv() => match request { Some(request) => request, None => break },
        };
        let result = match client.cached_snapshot() {
            Some(snapshot) if !client.is_stale() && request.binding.valid_for(&snapshot) => {
                tokio::select! {
                    biased;
                    _ = cancelled(&mut cancel) => break,
                    result = client.execute(request.binding.device_id(), request.binding.characteristic_id(), request.binding.command.clone()) => result,
                }
            }
            _ => Err(ClientError::Stale),
        };
        mailbox.complete(&client, request.token, request.binding, result);
    }
    // Shared close cancels requests and socket readers before the runtime is dropped.
    client.close();
    if tokio::time::timeout(std::time::Duration::from_secs(1), &mut subscription)
        .await
        .is_err()
    {
        subscription.abort();
        let _ = subscription.await;
    }
}

async fn subscribe(client: Client, mailbox: Mailbox, mut cancel: watch::Receiver<bool>) {
    let mut attempt = 0;
    loop {
        mailbox.state(&client, Connection::Connecting, None);
        let connected = tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => return,
            result = client.events() => result,
        };
        let error = match connected {
            Ok(mut stream) => {
                attempt = 0;
                // Connected is defined by events() consuming a valid initial snapshot.
                mailbox.state(&client, Connection::Online, None);
                loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancelled(&mut cancel) => return,
                        event = stream.next() => event,
                    };
                    match event {
                        Ok(Some(ClientEvent::Snapshot(_))) => {
                            mailbox.state(&client, Connection::Online, None)
                        }
                        Ok(Some(ClientEvent::Logs(_) | ClientEvent::Log(_))) => {}
                        Ok(None) => break ClientError::Network,
                        Err(error) => break error,
                    }
                }
            }
            Err(error) => error,
        };
        let terminal = error.is_terminal();
        mailbox.state(
            &client,
            if terminal {
                Connection::Incompatible
            } else {
                Connection::Offline
            },
            Some(error.to_string()),
        );
        if terminal {
            return;
        }
        tokio::select! {
            biased;
            _ = cancelled(&mut cancel) => return,
            _ = tokio::time::sleep(reconnect_delay(attempt)) => {}
        }
        attempt = attempt.saturating_add(1);
    }
}
