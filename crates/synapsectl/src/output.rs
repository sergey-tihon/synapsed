use serde::Serialize;
use std::io::{self, Write};
use synapsed_api::{
    Characteristic, CharacteristicSpec, CharacteristicStatus, CharacteristicValue, Device,
    LogEntry, LogsResponse, Snapshot, StatusResponse,
};

pub struct Output {
    pub json: bool,
}
impl Output {
    pub fn data(&self, value: &impl Serialize) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, value)?;
        stdout.write_all(b"\n")?;
        stdout.flush()
    }
    fn text(&self, text: &str) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()
    }
    pub fn status(&self, status: &StatusResponse) -> io::Result<()> {
        if self.json {
            return self.data(status);
        }
        let mut text = format!(
            "Node: {} ({})\nInstance: {}\nAPI: {}\nRevision: {}\nUptime: {} seconds\nDrivers:\n",
            status.node.name,
            status.node.node_id,
            status.node.instance_id,
            status.api_version,
            status.revision,
            status.uptime_seconds
        );
        if status.drivers.is_empty() {
            text.push_str("  No installed drivers.\n");
        }
        for driver in &status.drivers {
            text.push_str(&format!(
                "  {} {}: {:?}",
                driver.id, driver.version, driver.state
            ));
            if let Some(error) = &driver.error {
                text.push_str(&format!(" — {:?}: {}", error.code, error.message));
            }
            text.push('\n');
        }
        self.text(&text)
    }
    pub fn snapshot(&self, snapshot: &Snapshot) -> io::Result<()> {
        if self.json {
            return self.data(snapshot);
        }
        let mut text = format!(
            "Node: {} | Instance: {} | Revision: {}\n",
            snapshot.node.name, snapshot.node.instance_id, snapshot.revision
        );
        if snapshot.devices.is_empty() {
            text.push_str("Devices: none discovered.\n");
        }
        for device in &snapshot.devices {
            render_device(&mut text, device);
        }
        self.text(&text)
    }
    pub fn characteristic(&self, device: &str, characteristic: &Characteristic) -> io::Result<()> {
        if self.json {
            return self.data(characteristic);
        }
        let mut text = format!("Device: {device}\n");
        render_characteristic(&mut text, characteristic);
        self.text(&text)
    }
    pub fn logs(&self, logs: &LogsResponse) -> io::Result<()> {
        if self.json {
            return self.data(logs);
        }
        if logs.entries.is_empty() {
            return self.text("Logs: no recent entries.\n");
        }
        for entry in &logs.entries {
            self.log(entry)?;
        }
        Ok(())
    }
    pub fn log(&self, log: &LogEntry) -> io::Result<()> {
        if self.json {
            return self.data(log);
        }
        self.text(&format!(
            "{} {:?} [{}] #{} {}\n",
            log.timestamp, log.level, log.target, log.id, log.message
        ))
    }
}
fn render_device(text: &mut String, device: &Device) {
    text.push_str(&format!(
        "Device {}: {} | kind={} | driver={} | {:?}\n",
        device.id, device.label, device.kind, device.driver, device.status
    ));
    if let Some(error) = &device.error {
        text.push_str(&format!("  Error: {:?}: {}\n", error.code, error.message));
    }
    for characteristic in &device.characteristics {
        render_characteristic(text, characteristic);
    }
}
fn render_characteristic(text: &mut String, characteristic: &Characteristic) {
    let value = match &characteristic.value {
        Some(CharacteristicValue::Boolean(value)) => value.to_string(),
        Some(CharacteristicValue::Number(value)) => value.to_string(),
        Some(CharacteristicValue::String(value)) => format!("{value:?}"),
        None if matches!(characteristic.spec, CharacteristicSpec::Action) => {
            "Invoke (no stored value)".to_owned()
        }
        None => "Not yet observed".to_owned(),
    };
    let stale =
        if characteristic.status != CharacteristicStatus::Ok && characteristic.value.is_some() {
            " (last observation; stale)"
        } else {
            ""
        };
    text.push_str(&format!(
        "  {} ({}) = {}{} | {:?}\n",
        characteristic.id, characteristic.label, value, stale, characteristic.status
    ));
    text.push_str(&format!(
        "    Access: read={} write={} notify={}\n",
        characteristic.access.read, characteristic.access.write, characteristic.access.notify
    ));
    match &characteristic.spec {
        CharacteristicSpec::Number {
            minimum,
            maximum,
            step,
            integer,
            unit,
        } => {
            text.push_str(&format!("    Number: minimum={minimum:?} maximum={maximum:?} step={step:?} integer={integer} unit={}\n", unit.as_deref().unwrap_or("none")));
        }
        CharacteristicSpec::Enum { options } => {
            text.push_str("    Enum options:");
            for option in options {
                text.push_str(&format!(" {:?} ({})", option.value, option.label));
            }
            text.push('\n');
        }
        CharacteristicSpec::String { max_length } => {
            text.push_str(&format!("    String: maximum {max_length} characters\n"))
        }
        CharacteristicSpec::Boolean => text.push_str("    Boolean: true or false\n"),
        CharacteristicSpec::Action => text.push_str("    Action: invoke without arguments\n"),
    }
    if let Some(error) = &characteristic.error {
        text.push_str(&format!("    Error: {:?}: {}\n", error.code, error.message));
    }
}
