use crate::{ClientError, ClientEvent};
use std::{collections::VecDeque, sync::Arc};
use synapsed_api::{LogEntry, MAX_LOG_ENTRIES, Snapshot, parse_revision};

#[derive(Default)]
pub(crate) struct Ordering {
    pub generation: u64,
    pub socket: Option<u64>,
    next_socket: u64,
    pub socket_instance: Option<String>,
    pub instance: Option<String>,
    pub snapshot: Option<Arc<Snapshot>>,
    pub stale: bool,
    pub closed: bool,
    pub terminal: Option<ClientError>,
    logs: VecDeque<LogEntry>,
    log_watermark: Option<u64>,
}
impl Ordering {
    pub fn check(&self) -> Result<(), ClientError> {
        if self.closed {
            return Err(ClientError::Closed);
        }
        if let Some(error) = &self.terminal {
            return Err(error.clone());
        }
        Ok(())
    }
    pub fn begin_socket(&mut self) -> Result<u64, ClientError> {
        self.check()?;
        self.generation += 1;
        self.next_socket += 1;
        self.socket = Some(self.next_socket);
        self.socket_instance = None;
        self.stale = true;
        Ok(self.next_socket)
    }
    pub fn check_socket(&self, token: u64) -> Result<(), ClientError> {
        self.check()?;
        if self.socket != Some(token) {
            return Err(ClientError::Stale);
        }
        Ok(())
    }
    pub fn disconnect(&mut self, token: u64) {
        if self.socket == Some(token) {
            self.socket = None;
            self.socket_instance = None;
            self.stale = true;
            self.generation += 1;
        }
    }
    pub fn fail(&mut self, error: &ClientError) {
        self.stale = true;
        self.socket = None;
        self.socket_instance = None;
        self.generation += 1;
        if error.is_terminal() {
            self.terminal = Some(error.clone());
        }
    }
    fn adopt(&mut self, instance: &str) {
        if self.instance.as_deref() != Some(instance) {
            if self.instance.is_some() {
                self.generation += 1;
            }
            self.instance = Some(instance.to_owned());
            self.snapshot = None;
            self.logs.clear();
            self.log_watermark = None;
        }
    }
    pub fn http_identity(&mut self, generation: u64, instance: &str) -> Result<(), ClientError> {
        self.check()?;
        if generation != self.generation {
            return Err(ClientError::Stale);
        }
        if self.socket.is_some()
            && self
                .instance
                .as_deref()
                .is_some_and(|known| known != instance)
        {
            self.fail(&ClientError::Stale);
            return Err(ClientError::Stale);
        }
        self.adopt(instance);
        Ok(())
    }
    pub fn http_snapshot(
        &mut self,
        generation: u64,
        snapshot: Snapshot,
    ) -> Result<Arc<Snapshot>, ClientError> {
        self.check()?;
        if generation != self.generation {
            return self
                .snapshot
                .as_ref()
                .filter(|known| known.node.instance_id == snapshot.node.instance_id)
                .cloned()
                .ok_or(ClientError::Stale);
        }
        self.http_identity(generation, &snapshot.node.instance_id)?;
        let (snapshot, _) = self.accept_snapshot(snapshot)?;
        if self.socket.is_none() {
            self.stale = false;
        }
        Ok(snapshot)
    }
    pub fn initial(
        &mut self,
        token: u64,
        snapshot: Snapshot,
    ) -> Result<Arc<Snapshot>, ClientError> {
        self.check_socket(token)?;
        self.adopt(&snapshot.node.instance_id);
        self.socket_instance = Some(snapshot.node.instance_id.clone());
        let (snapshot, _) = self.accept_snapshot(snapshot)?;
        self.stale = false;
        Ok(snapshot)
    }
    pub fn live_snapshot(
        &mut self,
        token: u64,
        snapshot: Snapshot,
    ) -> Result<Option<ClientEvent>, ClientError> {
        self.check_socket(token)?;
        if self.socket_instance.as_deref() != Some(&snapshot.node.instance_id) {
            return Err(ClientError::Protocol(
                "Instance changed inside one WebSocket stream.",
            ));
        }
        let (snapshot, changed) = self.accept_snapshot(snapshot)?;
        Ok(changed.then_some(ClientEvent::Snapshot(snapshot)))
    }
    fn accept_snapshot(
        &mut self,
        snapshot: Snapshot,
    ) -> Result<(Arc<Snapshot>, bool), ClientError> {
        let revision = parse_revision(&snapshot.revision)
            .map_err(|_| ClientError::Protocol("Invalid snapshot revision."))?;
        if let Some(known) = &self.snapshot {
            let previous = parse_revision(&known.revision)
                .map_err(|_| ClientError::Protocol("Invalid cached revision."))?;
            if revision <= previous {
                return Ok((known.clone(), false));
            }
        }
        let snapshot = Arc::new(snapshot);
        self.snapshot = Some(snapshot.clone());
        Ok((snapshot, true))
    }
    /// Replace the view with the current ring, but deliver only never-seen sequence IDs
    /// on reconnect. The watermark survives a connection loss, not a daemon restart.
    pub fn recent_logs(
        &mut self,
        token: u64,
        entries: Vec<LogEntry>,
    ) -> Result<ClientEvent, ClientError> {
        self.check_socket(token)?;
        let watermark = self.log_watermark;
        let mut unseen = Vec::new();
        let mut newest = watermark;
        self.logs.clear();
        for entry in entries {
            let id = parse_revision(&entry.id)
                .map_err(|_| ClientError::Protocol("Invalid log sequence."))?;
            if watermark.is_none_or(|known| id > known) {
                unseen.push(entry.clone());
            }
            newest = Some(newest.map_or(id, |known| known.max(id)));
            self.logs.push_back(entry);
        }
        self.log_watermark = newest;
        Ok(ClientEvent::Logs(unseen))
    }
    pub fn live_log(
        &mut self,
        token: u64,
        entry: LogEntry,
    ) -> Result<Option<ClientEvent>, ClientError> {
        self.check_socket(token)?;
        let id = parse_revision(&entry.id)
            .map_err(|_| ClientError::Protocol("Invalid log sequence."))?;
        if self.log_watermark.is_some_and(|known| id <= known) {
            return Ok(None);
        }
        self.log_watermark = Some(id);
        if self.logs.len() == MAX_LOG_ENTRIES {
            self.logs.pop_front();
        }
        self.logs.push_back(entry.clone());
        Ok(Some(ClientEvent::Log(entry)))
    }
}
