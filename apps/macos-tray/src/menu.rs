//! Main-thread-only native menus, built exclusively from public metadata.
use crate::worker::{Connection, MAX_PENDING, Request, View, Worker};
use std::{collections::HashMap, sync::Arc};
use synapsed_api::{
    Characteristic, CharacteristicCommand, CharacteristicSpec, CharacteristicStatus,
    CharacteristicValue, DeviceStatus, DriverState, Snapshot,
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{CheckMenuItem, Menu, MenuId, MenuItem, Submenu},
};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    device: String,
    characteristic: String,
}

#[derive(Clone)]
pub struct CommandBinding {
    catalog: Arc<Snapshot>,
    device_index: usize,
    characteristic_index: usize,
    pub command: CharacteristicCommand,
}
impl CommandBinding {
    pub fn device_id(&self) -> &str {
        &self.catalog.devices[self.device_index].id
    }
    pub fn characteristic_id(&self) -> &str {
        &self.catalog.devices[self.device_index].characteristics[self.characteristic_index].id
    }
    fn key(&self) -> Key {
        Key {
            device: self.device_id().into(),
            characteristic: self.characteristic_id().into(),
        }
    }
    pub fn valid_for(&self, snapshot: &Snapshot) -> bool {
        if self.catalog.node.instance_id != snapshot.node.instance_id {
            return false;
        }
        let original = &self.catalog.devices[self.device_index];
        let characteristic = &original.characteristics[self.characteristic_index];
        snapshot
            .devices
            .iter()
            .find(|device| device.id == original.id)
            .is_some_and(|device| {
                device.driver == original.driver
                    && device.status == DeviceStatus::Online
                    && device
                        .characteristics
                        .iter()
                        .find(|current| current.id == characteristic.id)
                        .is_some_and(|current| {
                            current.spec == characteristic.spec
                                && current.access == characteristic.access
                                && current.status == CharacteristicStatus::Ok
                                && current.validate_command(&self.command).is_ok()
                        })
            })
    }
}

enum Action {
    Command(CommandBinding),
    Edit {
        device: String,
        characteristic: String,
    },
    Open,
    RestartDriver(String),
    Quit,
}
enum CommandItem {
    Plain(MenuItem),
    Check {
        item: CheckMenuItem,
        value: CharacteristicValue,
    },
}
impl CommandItem {
    fn update(&self, enabled: bool, characteristic: &Characteristic) {
        match self {
            Self::Plain(item) => item.set_enabled(enabled),
            Self::Check { item, value } => {
                // muda flips before delivering MenuEvent. Undo that flip from the
                // actual observation, never from the requested operation/result.
                item.set_checked(characteristic.value.as_ref() == Some(value));
                item.set_enabled(enabled);
            }
        }
    }
}
struct Row {
    key: Key,
    device_index: usize,
    characteristic_index: usize,
    header: Submenu,
    observation: MenuItem,
    items: Vec<CommandItem>,
}
struct DeviceMenu {
    index: usize,
    menu: Submenu,
    detail: MenuItem,
}
struct NativeMenu {
    root: Menu,
    status: MenuItem,
    detail: MenuItem,
    result: MenuItem,
    catalog: Option<Arc<Snapshot>>,
    devices: Vec<DeviceMenu>,
    rows: Vec<Row>,
    driver_items: HashMap<String, MenuItem>,
    actions: HashMap<MenuId, Action>,
}

pub struct Ui {
    pub tray: TrayIcon,
    menu: NativeMenu,
    generation: u64,
    next_token: u64,
    pending: HashMap<Key, u64>,
    pending_driver_restarts: HashMap<String, u64>,
    errors: HashMap<Key, String>,
    last_result: Option<String>,
    snapshot: Option<Arc<Snapshot>>,
    connection: Connection,
    service_error: Option<String>,
}
impl Ui {
    pub fn new() -> Result<Self, String> {
        let menu = NativeMenu::build(None, 0)?;
        let tray = TrayIconBuilder::new()
            .with_id("synapsed")
            .with_menu(Box::new(menu.root.clone()))
            .with_icon_templated(template_icon()?)
            .with_tooltip("Synapsed — Connecting")
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(false)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            tray,
            menu,
            generation: 0,
            next_token: 0,
            pending: HashMap::new(),
            pending_driver_restarts: HashMap::new(),
            errors: HashMap::new(),
            last_result: None,
            snapshot: None,
            connection: Connection::Connecting,
            service_error: None,
        })
    }
    pub fn apply(&mut self, view: View) -> Result<(), String> {
        self.connection = view.connection;
        self.service_error = view.error;
        for completion in view.completions {
            let key = completion.binding.key();
            if self.pending.get(&key) == Some(&completion.token) {
                self.pending.remove(&key);
                if let Some(error) = completion.error {
                    eprintln!("Synapsed tray: {error}");
                    self.last_result = Some(format!("Command failed: {error}"));
                    self.errors.insert(key, error);
                } else {
                    self.errors.remove(&key);
                    self.last_result = Some("Command confirmed by service observation.".into());
                }
            }
        }
        for completion in view.driver_restarts {
            if self.pending_driver_restarts.get(&completion.driver) == Some(&completion.token) {
                self.pending_driver_restarts.remove(&completion.driver);
                if let Some(error) = completion.error {
                    eprintln!("Synapsed tray: driver restart failed: {error}");
                    self.last_result = Some(format!("Driver restart failed: {error}"));
                } else {
                    self.last_result =
                        Some(format!("Driver restart completed: {}.", completion.driver));
                }
            }
        }
        let changed = !same_structure(self.menu.catalog.as_deref(), view.snapshot.as_deref());
        self.snapshot = view.snapshot;
        if changed {
            self.generation += 1;
            let next = NativeMenu::build(self.snapshot.clone(), self.generation)?;
            self.tray.set_menu(Some(Box::new(next.root.clone())));
            self.menu = next;
            self.errors.retain(|key, _| {
                self.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.devices.iter().any(|device| {
                        device.id == key.device
                            && device
                                .characteristics
                                .iter()
                                .any(|characteristic| characteristic.id == key.characteristic)
                    })
                })
            });
        }
        self.refresh();
        Ok(())
    }
    fn refresh(&self) {
        let driver_errors = self.snapshot.as_ref().map_or(0, |snapshot| {
            snapshot
                .drivers
                .iter()
                .filter(|driver| {
                    driver.error.is_some()
                        || matches!(
                            driver.state,
                            DriverState::Disconnected
                                | DriverState::Ambiguous
                                | DriverState::PermissionDenied
                                | DriverState::Error
                        )
                })
                .count()
        });
        let title = if driver_errors == 0 {
            format!("Synapsed — {}", self.connection.label())
        } else {
            format!(
                "Synapsed — {} · {driver_errors} driver errors",
                self.connection.label()
            )
        };
        self.tray.set_title((driver_errors > 0).then_some("!"));
        let tooltip = self
            .service_error
            .as_ref()
            .map_or_else(|| title.clone(), |error| format!("{title}: {error}"));
        if let Err(error) = self.tray.set_tooltip(Some(&tooltip)) {
            eprintln!("Synapsed tray: cannot update tooltip: {error}");
        }
        self.menu.status.set_text(text(&title));
        let detail = self.snapshot.as_ref().map_or_else(
            || "No snapshot received.".into(),
            |snapshot| {
                format!(
                    "{} · API {} · revision {}{}",
                    snapshot.node.name,
                    snapshot.api_version,
                    snapshot.revision,
                    if self.connection != Connection::Online {
                        " · retained values are stale"
                    } else {
                        ""
                    }
                )
            },
        );
        if let Some(error) = &self.service_error {
            self.menu
                .detail
                .set_text(text(&format!("{detail} · {error}")));
        } else {
            self.menu.detail.set_text(text(&detail));
        }
        self.menu.result.set_text(text(
            self.last_result
                .as_deref()
                .unwrap_or("Commands require a current service observation."),
        ));
        for (driver, item) in &self.menu.driver_items {
            item.set_enabled(
                self.connection == Connection::Online
                    && !self.pending_driver_restarts.contains_key(driver),
            );
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for group in &self.menu.devices {
            let device = &snapshot.devices[group.index];
            group.menu.set_text(text(&format!(
                "{} ({}){}",
                device.label,
                device.kind,
                if device.status == DeviceStatus::Unavailable {
                    " — unavailable"
                } else {
                    ""
                }
            )));
            group
                .detail
                .set_text(text(&device.error.as_ref().map_or_else(
                    || format!("Driver: {} · ID: {}", device.driver, device.id),
                    |error| format!("Driver: {} · {}", device.driver, error),
                )));
        }
        for row in &self.menu.rows {
            let device = &snapshot.devices[row.device_index];
            let characteristic = &device.characteristics[row.characteristic_index];
            let pending = self.pending.contains_key(&row.key);
            let stale = self.connection != Connection::Online
                || device.status != DeviceStatus::Online
                || characteristic.status != CharacteristicStatus::Ok;
            let reading = observation(characteristic, stale);
            row.header.set_text(text(&format!(
                "{}: {reading}{}",
                characteristic.label,
                if pending { " · Pending" } else { "" }
            )));
            let detail = self.errors.get(&row.key).map_or_else(
                || reading.clone(),
                |error| format!("{reading} · Command failed: {error}"),
            );
            row.observation.set_text(text(&detail));
            // Shape changes rebuild the command map. These operations were built
            // from validated metadata; dispatch revalidates current applicability.
            let enabled = !stale && !pending && self.pending_driver_restarts.is_empty();
            for item in &row.items {
                item.update(enabled, characteristic);
            }
        }
    }
    /// Called only after applying the latest mailbox, including after tracking ends.
    /// Unique generation IDs ensure an event from a retired structure cannot rebind.
    pub fn activate(&mut self, id: &MenuId, worker: &Worker) -> bool {
        match self.menu.actions.get(id) {
            Some(Action::Quit) => return true,
            Some(Action::Open) => self.open(worker.endpoint.as_ref(), None),
            Some(Action::RestartDriver(driver)) => {
                if self.connection != Connection::Online
                    || self.pending_driver_restarts.contains_key(driver)
                    || self.pending.len() + self.pending_driver_restarts.len() >= MAX_PENDING
                    || self.snapshot.as_ref().is_none_or(|snapshot| {
                        !snapshot.drivers.iter().any(|item| item.id == *driver)
                    })
                {
                    return false;
                }
                self.next_token += 1;
                let token = self.next_token;
                match worker.requests.try_send(Request::RestartDriver {
                    token,
                    driver: driver.clone(),
                }) {
                    Ok(()) => {
                        self.pending_driver_restarts.insert(driver.clone(), token);
                        self.last_result = Some(format!("Restart requested: {driver}."));
                    }
                    Err(_) => {
                        self.last_result = Some(
                            "Driver restart not sent: client worker is busy or stopped.".into(),
                        );
                    }
                }
            }
            Some(Action::Edit {
                device,
                characteristic,
            }) => self.open(worker.endpoint.as_ref(), Some((device, characteristic))),
            Some(Action::Command(binding)) => {
                if self.connection != Connection::Online
                    || self
                        .snapshot
                        .as_ref()
                        .is_none_or(|snapshot| !binding.valid_for(snapshot))
                {
                    return false;
                }
                let key = binding.key();
                if self.pending.contains_key(&key)
                    || self.pending.len() + self.pending_driver_restarts.len() >= MAX_PENDING
                {
                    return false;
                }
                self.next_token += 1;
                let request = Request::Command {
                    token: self.next_token,
                    binding: binding.clone(),
                };
                match worker.requests.try_send(request) {
                    Ok(()) => {
                        self.errors.remove(&key);
                        self.pending.insert(key, self.next_token);
                    }
                    Err(_) => {
                        self.last_result =
                            Some("Command not sent: client worker is busy or stopped.".into());
                        eprintln!(
                            "Synapsed tray: command not sent; client worker is busy or stopped."
                        );
                    }
                }
            }
            None => {} // Retired generation; never interpret it as a new binding.
        }
        self.refresh();
        false
    }
    fn open(&self, endpoint: Option<&Url>, target: Option<(&str, &str)>) {
        let Some(endpoint) = endpoint else {
            eprintln!("Synapsed tray: no valid local endpoint configured.");
            return;
        };
        let mut url = endpoint.clone();
        if let Some((device, characteristic)) = target {
            url.set_fragment(Some(&format!(
                "/devices/{}?characteristic={}",
                encode_component(device),
                encode_component(characteristic)
            )));
        }
        if let Err(error) = open::that_detached(url.as_str()) {
            eprintln!("Synapsed tray: cannot open Web UI: {error}");
        }
    }
    pub fn detach(self) {
        self.tray.set_menu(None);
    }
}

impl NativeMenu {
    fn build(catalog: Option<Arc<Snapshot>>, generation: u64) -> Result<Self, String> {
        let root = Menu::new();
        let status = MenuItem::new("Synapsed — Connecting", false, None);
        let detail = MenuItem::new("No snapshot received.", false, None);
        let result = MenuItem::new(
            "Commands require a current service observation.",
            false,
            None,
        );
        root.append_items(&[&status, &detail, &result])
            .map_err(|error| error.to_string())?;
        let mut menu = Self {
            root,
            status,
            detail,
            result,
            catalog: catalog.clone(),
            devices: Vec::new(),
            rows: Vec::new(),
            driver_items: HashMap::new(),
            actions: HashMap::new(),
        };
        let mut sequence = 0;
        if let Some(snapshot) = &catalog {
            let drivers = Submenu::new("Drivers", true);
            if snapshot.drivers.is_empty() {
                drivers
                    .append(&MenuItem::new("No installed driver packages.", false, None))
                    .map_err(|error| error.to_string())?;
            }
            for driver in &snapshot.drivers {
                let state = match driver.state {
                    DriverState::Disabled => "disabled",
                    DriverState::Online => "online",
                    DriverState::Disconnected => "disconnected",
                    DriverState::Ambiguous => "ambiguous",
                    DriverState::PermissionDenied => "permission denied",
                    DriverState::Error => "error",
                };
                let caption = driver.error.as_ref().map_or_else(
                    || format!("{} {} — {state}", driver.id, driver.version),
                    |error| format!("{} {} — {state}: {error}", driver.id, driver.version),
                );
                let id = next_id(generation, &mut sequence);
                let item = MenuItem::with_id(
                    id.clone(),
                    text(&format!("{caption} · Restart")),
                    true,
                    None,
                );
                drivers.append(&item).map_err(|error| error.to_string())?;
                menu.actions
                    .insert(id, Action::RestartDriver(driver.id.clone()));
                menu.driver_items.insert(driver.id.clone(), item);
            }
            menu.root
                .append(&drivers)
                .map_err(|error| error.to_string())?;
            for (device_index, device) in snapshot.devices.iter().enumerate() {
                let group = Submenu::new(text(&device.label), true);
                let detail = MenuItem::new(
                    text(&format!("Driver: {} · ID: {}", device.driver, device.id)),
                    false,
                    None,
                );
                group.append(&detail).map_err(|error| error.to_string())?;
                for (section, indices) in device.characteristic_sections() {
                    if let Some(section_group) = section {
                        group
                            .append(&MenuItem::new(text(&section_group.label), false, None))
                            .map_err(|error| error.to_string())?;
                    }
                    for characteristic_index in indices {
                        let characteristic = &device.characteristics[characteristic_index];
                        let header = Submenu::new(text(&characteristic.label), true);
                        let reading =
                            MenuItem::new(text(&observation(characteristic, false)), false, None);
                        header
                            .append_items(&[
                                &reading,
                                &MenuItem::new(text(&metadata(characteristic)), false, None),
                            ])
                            .map_err(|error| error.to_string())?;
                        let mut row = Row {
                            key: Key {
                                device: device.id.clone(),
                                characteristic: characteristic.id.clone(),
                            },
                            device_index,
                            characteristic_index,
                            header,
                            observation: reading,
                            items: Vec::new(),
                        };
                        let binding = |command| CommandBinding {
                            catalog: snapshot.clone(),
                            device_index,
                            characteristic_index,
                            command,
                        };
                        if characteristic.access.write {
                            match &characteristic.spec {
                                CharacteristicSpec::Number { step: Some(_), .. } => {
                                    menu.command(
                                        &mut row,
                                        binding(CharacteristicCommand::Step { steps: -1 }),
                                        "Decrease",
                                        None,
                                        generation,
                                        &mut sequence,
                                    )?;
                                    menu.command(
                                        &mut row,
                                        binding(CharacteristicCommand::Step { steps: 1 }),
                                        "Increase",
                                        None,
                                        generation,
                                        &mut sequence,
                                    )?;
                                }
                                CharacteristicSpec::Enum { options } => {
                                    for option in options {
                                        let value =
                                            CharacteristicValue::String(option.value.clone());
                                        menu.command(
                                            &mut row,
                                            binding(CharacteristicCommand::Set {
                                                value: value.clone(),
                                            }),
                                            &option.label,
                                            Some(value),
                                            generation,
                                            &mut sequence,
                                        )?;
                                    }
                                }
                                CharacteristicSpec::Boolean => menu.command(
                                    &mut row,
                                    binding(CharacteristicCommand::Toggle {}),
                                    "Toggle",
                                    Some(CharacteristicValue::Boolean(true)),
                                    generation,
                                    &mut sequence,
                                )?,
                                CharacteristicSpec::Action => menu.command(
                                    &mut row,
                                    binding(CharacteristicCommand::Invoke {}),
                                    "Invoke",
                                    None,
                                    generation,
                                    &mut sequence,
                                )?,
                                CharacteristicSpec::String { .. }
                                | CharacteristicSpec::Number { step: None, .. } => {
                                    let id = next_id(generation, &mut sequence);
                                    let edit =
                                        MenuItem::with_id(id.clone(), "Edit in Web UI", true, None);
                                    row.header
                                        .append(&edit)
                                        .map_err(|error| error.to_string())?;
                                    menu.actions.insert(
                                        id,
                                        Action::Edit {
                                            device: device.id.clone(),
                                            characteristic: characteristic.id.clone(),
                                        },
                                    );
                                }
                            }
                        }
                        group
                            .append(&row.header)
                            .map_err(|error| error.to_string())?;
                        menu.rows.push(row);
                    }
                }
                menu.root
                    .append(&group)
                    .map_err(|error| error.to_string())?;
                menu.devices.push(DeviceMenu {
                    index: device_index,
                    menu: group,
                    detail,
                });
            }
            if snapshot.devices.is_empty() {
                menu.root
                    .append(&MenuItem::new("No devices discovered.", false, None))
                    .map_err(|error| error.to_string())?;
            }
        }
        // These actions never bind hardware. Keep Quit usable even when the
        // structure changes during tracking and characteristic IDs are retired.
        for (id, label, action) in [
            ("synapsed:open", "Open Web UI", Action::Open),
            ("synapsed:quit", "Quit Tray", Action::Quit),
        ] {
            let id = MenuId::new(id);
            menu.root
                .append(&MenuItem::with_id(id.clone(), label, true, None))
                .map_err(|error| error.to_string())?;
            menu.actions.insert(id, action);
        }
        Ok(menu)
    }
    fn command(
        &mut self,
        row: &mut Row,
        binding: CommandBinding,
        label: &str,
        checked: Option<CharacteristicValue>,
        generation: u64,
        sequence: &mut usize,
    ) -> Result<(), String> {
        let id = next_id(generation, sequence);
        let item = match checked {
            Some(value) => {
                let item = CheckMenuItem::with_id(id.clone(), text(label), false, false, None);
                row.header
                    .append(&item)
                    .map_err(|error| error.to_string())?;
                CommandItem::Check { item, value }
            }
            None => {
                let item = MenuItem::with_id(id.clone(), text(label), false, None);
                row.header
                    .append(&item)
                    .map_err(|error| error.to_string())?;
                CommandItem::Plain(item)
            }
        };
        self.actions.insert(id, Action::Command(binding));
        row.items.push(item);
        Ok(())
    }
}

fn next_id(generation: u64, sequence: &mut usize) -> MenuId {
    *sequence += 1;
    MenuId::new(format!("synapsed:{generation}:{sequence}"))
}
fn same_structure(old: Option<&Snapshot>, new: Option<&Snapshot>) -> bool {
    match (old, new) {
        (None, None) => true,
        (Some(old), Some(new)) => {
            old.node.instance_id == new.node.instance_id
                && old.drivers == new.drivers
                && old.devices.len() == new.devices.len()
                && old.devices.iter().zip(&new.devices).all(|(old, new)| {
                    old.id == new.id
                        && old.driver == new.driver
                        && old.kind == new.kind
                        && old.groups == new.groups
                        && old.characteristics.len() == new.characteristics.len()
                        && old
                            .characteristics
                            .iter()
                            .zip(&new.characteristics)
                            .all(|(old, new)| {
                                old.id == new.id
                                    && old.group == new.group
                                    && old.spec == new.spec
                                    && old.access == new.access
                            })
                })
        }
        _ => false,
    }
}
fn observation(characteristic: &Characteristic, stale: bool) -> String {
    let value = match (&characteristic.spec, &characteristic.value) {
        (CharacteristicSpec::Action, _) => "Action".into(),
        (_, None) => "Not yet observed".into(),
        (CharacteristicSpec::Enum { options }, Some(CharacteristicValue::String(value))) => options
            .iter()
            .find(|option| &option.value == value)
            .map_or_else(|| value.clone(), |option| option.label.clone()),
        (_, Some(CharacteristicValue::Boolean(value))) => if *value { "On" } else { "Off" }.into(),
        (CharacteristicSpec::Number { unit, .. }, Some(CharacteristicValue::Number(value))) => unit
            .as_ref()
            .map_or_else(|| value.to_string(), |unit| format!("{value} {unit}")),
        (_, Some(CharacteristicValue::Number(value))) => value.to_string(),
        (_, Some(CharacteristicValue::String(value))) => {
            if value.is_empty() {
                "Empty string".into()
            } else {
                value.clone()
            }
        }
    };
    let status = match characteristic.status {
        CharacteristicStatus::Ok => {
            if stale {
                " · stale"
            } else {
                ""
            }
        }
        CharacteristicStatus::Unavailable => " · Unavailable",
        CharacteristicStatus::Error => " · Error",
    };
    characteristic.error.as_ref().map_or_else(
        || format!("{value}{status}"),
        |error| format!("{value}{status} · {}", error.message),
    )
}
fn metadata(characteristic: &Characteristic) -> String {
    let access = if characteristic.access.write {
        if characteristic.access.read {
            "Read / write"
        } else {
            "Write only"
        }
    } else {
        "Read only"
    };
    match &characteristic.spec {
        CharacteristicSpec::Number {
            minimum,
            maximum,
            step,
            integer,
            ..
        } => format!(
            "{access} · {} · min {} · max {} · step {}",
            if *integer { "integer" } else { "number" },
            minimum.map_or_else(|| "none".into(), |value| value.to_string()),
            maximum.map_or_else(|| "none".into(), |value| value.to_string()),
            step.map_or_else(|| "unsupported".into(), |value| value.to_string())
        ),
        CharacteristicSpec::String { max_length } => {
            format!("{access} · string · maximum {max_length} characters")
        }
        CharacteristicSpec::Boolean => format!("{access} · boolean"),
        CharacteristicSpec::Enum { .. } => format!("{access} · enum"),
        CharacteristicSpec::Action => format!("{access} · no-argument action"),
    }
}
fn text(value: &str) -> String {
    value.replace('&', "&&").replace(['\r', '\n'], " ")
}
fn encode_component(value: &str) -> String {
    // Matches JavaScript encodeURIComponent, including its five unescaped marks.
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            result.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}
fn template_icon() -> Result<Icon, String> {
    Icon::from_rgba(
        include_bytes!("../assets/tray-template.rgba").to_vec(),
        36,
        36,
    )
    .map_err(|error| error.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    use synapsed_api::{
        CharacteristicAccess, CharacteristicCommand, CharacteristicValue, Device, DriverState,
        DriverStatus, NodeInfo,
    };

    fn snapshot() -> Snapshot {
        Snapshot {
            api_version: 1,
            node: NodeInfo {
                node_id: "00000000-0000-4000-8000-000000000001".into(),
                instance_id: "00000000-0000-4000-8000-000000000002".into(),
                name: "test".into(),
            },
            revision: "1".into(),
            drivers: vec![DriverStatus {
                id: "test".into(),
                version: "1.0.0".into(),
                state: DriverState::Online,
                error: None,
            }],
            devices: vec![Device {
                id: "device".into(),
                driver: "test".into(),
                kind: "test".into(),
                label: "Device".into(),
                status: DeviceStatus::Online,
                error: None,
                groups: vec![],
                characteristics: vec![Characteristic {
                    id: "value".into(),
                    label: "Value".into(),
                    group: None,
                    spec: CharacteristicSpec::Boolean,
                    access: CharacteristicAccess {
                        read: true,
                        write: true,
                        notify: false,
                    },
                    value: Some(CharacteristicValue::Boolean(true)),
                    status: CharacteristicStatus::Ok,
                    error: None,
                }],
            }],
        }
    }

    #[test]
    fn observation_and_metadata_match_shared_value_labels() {
        let snapshot = snapshot();
        let characteristic = &snapshot.devices[0].characteristics[0];
        assert_eq!(observation(characteristic, false), "On");
        assert_eq!(observation(characteristic, true), "On · stale");
        assert_eq!(metadata(characteristic), "Read / write · boolean");
    }

    #[test]
    fn command_binding_revalidates_current_catalog() {
        let catalog = Arc::new(snapshot());
        let binding = CommandBinding {
            catalog: catalog.clone(),
            device_index: 0,
            characteristic_index: 0,
            command: CharacteristicCommand::Toggle {},
        };
        assert!(binding.valid_for(&catalog));
        let mut changed = (*catalog).clone();
        changed.devices[0].status = DeviceStatus::Unavailable;
        assert!(!binding.valid_for(&changed));
    }

    #[test]
    fn structure_and_route_fragment_are_stable() {
        let current = snapshot();
        assert!(same_structure(Some(&current), Some(&current)));
        assert!(!same_structure(None, Some(&current)));
        let mut url = Url::parse("http://127.0.0.1:24816/").unwrap();
        url.set_fragment(Some(&format!(
            "/devices/{}?characteristic={}",
            encode_component("a&b"),
            encode_component("c#d")
        )));
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:24816/#/devices/a%26b?characteristic=c%23d"
        );
    }

    #[test]
    fn encode_component_matches_encode_uri_component() {
        for (value, expected) in [
            ("plain", "plain"),
            ("a&b", "a%26b"),
            ("c#d", "c%23d"),
            ("space here", "space%20here"),
            ("!'()*-_.~", "!'()*-_.~"),
            ("é/雪", "%C3%A9%2F%E9%9B%AA"),
        ] {
            assert_eq!(encode_component(value), expected);
        }
    }
}
