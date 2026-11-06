use serde::Serialize;
use std::io::{self, Write};
use synapsed_api::{
    Characteristic, CharacteristicChange, CharacteristicSpec, CharacteristicStatus,
    CharacteristicValue, Device, InventoryNode, InventoryNodeKind, InventoryRelation,
    InventoryResponse, InventoryRule, InventorySourceKind, LogEntry, LogsResponse, Snapshot,
    StatusResponse,
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
        render_characteristic(&mut text, characteristic, "");
        self.text(&text)
    }
    pub fn command_result(
        &self,
        device_id: &str,
        characteristic_id: &str,
        snapshot: &Snapshot,
    ) -> io::Result<()> {
        let characteristic = snapshot
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .and_then(|device| {
                device
                    .characteristics
                    .iter()
                    .find(|characteristic| characteristic.id == characteristic_id)
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "command result missing from snapshot",
                )
            })?;
        self.characteristic(device_id, characteristic)
    }
    pub fn inventory(&self, inventory: &InventoryResponse) -> io::Result<()> {
        if self.json {
            return self.text(&inventory_json(inventory)?);
        }
        self.text(&render_inventory(inventory))
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
    pub fn change(&self, change: &CharacteristicChange) -> io::Result<()> {
        if self.json {
            return self.data(change);
        }
        let value = change
            .value
            .as_ref()
            .map(value_text)
            .unwrap_or_else(|| "none".to_owned());
        let mut line = format!(
            "{} #{} {}/{} = {} | {:?}",
            change.timestamp,
            change.revision,
            change.device_id,
            change.characteristic_id,
            value,
            change.status
        );
        if let Some(error) = &change.error {
            line.push_str(&format!(" — {:?}: {}", error.code, error.message));
        }
        line.push('\n');
        self.text(&line)
    }
}
fn inventory_json(inventory: &InventoryResponse) -> io::Result<String> {
    let mut output = serde_json::to_string(inventory)?;
    output.push('\n');
    Ok(output)
}
fn render_inventory(inventory: &InventoryResponse) -> String {
    let mut text = String::from("Inventory:\nSources:\n");
    for source in &inventory.sources {
        text.push_str(&format!(
            "  {}: {:?}",
            inventory_source_kind(source.kind),
            source.state
        ));
        if let Some(message) = &source.message {
            text.push_str(&format!(" — {message}"));
        }
        text.push('\n');
    }
    let mut children = vec![Vec::new(); inventory.nodes.len()];
    let by_id: std::collections::HashMap<&str, usize> = inventory
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect();
    let mut roots = Vec::new();
    for (index, node) in inventory.nodes.iter().enumerate() {
        if let Some(parent) = node.parent_id.as_deref() {
            if let Some(parent) = by_id.get(parent) {
                children[*parent].push(index);
            }
        } else {
            roots.push(index);
        }
    }
    let mut stack: Vec<_> = roots
        .into_iter()
        .rev()
        .map(|index| (index, 0usize))
        .collect();
    let mut visited = vec![false; inventory.nodes.len()];
    while let Some((index, depth)) = stack.pop() {
        if visited[index] {
            continue;
        }
        visited[index] = true;
        render_inventory_node(&mut text, &inventory.nodes[index], depth);
        stack.extend(
            children[index]
                .iter()
                .rev()
                .map(|child| (*child, depth + 1)),
        );
    }
    text
}

fn inventory_source_kind(kind: InventorySourceKind) -> &'static str {
    match kind {
        InventorySourceKind::Usb => "usb",
        InventorySourceKind::Thunderbolt => "thunderbolt",
        InventorySourceKind::Ddc => "ddc",
        InventorySourceKind::Bluetooth => "bluetooth",
    }
}
fn render_inventory_node(text: &mut String, node: &InventoryNode, depth: usize) {
    text.push_str(&"  ".repeat(depth));
    text.push_str(inventory_kind(node.kind));
    text.push_str(": ");
    text.push_str(&node.label);
    if let Some(detail) = &node.detail {
        text.push_str(" — ");
        text.push_str(detail);
    }
    if let Some(driver) = &node.driver {
        text.push_str(&format!(
            " [{} driver: {}]",
            inventory_driver_class(driver.class),
            driver.id
        ));
    }
    if node.relation == InventoryRelation::Inferred
        && let Some(rule) = node.rule
    {
        text.push_str(&format!(" [inferred: {}]", inventory_rule(rule)));
    }
    if !node.connected {
        text.push_str(" [disconnected]");
    }
    text.push('\n');
}

fn inventory_kind(kind: InventoryNodeKind) -> &'static str {
    match kind {
        InventoryNodeKind::Host => "Host",
        InventoryNodeKind::Controller => "Controller",
        InventoryNodeKind::Dock => "Dock",
        InventoryNodeKind::Hub => "Hub",
        InventoryNodeKind::UsbDevice => "USB device",
        InventoryNodeKind::Display => "Display",
        InventoryNodeKind::BluetoothAdapter => "Bluetooth adapter",
        InventoryNodeKind::BluetoothDevice => "Bluetooth device",
    }
}

fn inventory_driver_class(class: synapsed_api::InventoryDriverClass) -> &'static str {
    match class {
        synapsed_api::InventoryDriverClass::Specialized => "specialized",
        synapsed_api::InventoryDriverClass::Generic => "generic",
    }
}

fn inventory_rule(rule: InventoryRule) -> &'static str {
    match rule {
        InventoryRule::Usb4Tunnel => "USB4 tunnel",
        InventoryRule::VendorMatch => "vendor match",
        InventoryRule::DriverLink => "driver link",
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
    for (group, indices) in device.characteristic_sections() {
        let indent = if let Some(group) = group {
            text.push_str(&format!("  [{}]\n", group.label));
            "  "
        } else {
            ""
        };
        for index in indices {
            render_characteristic(text, &device.characteristics[index], indent);
        }
    }
}
fn render_characteristic(text: &mut String, characteristic: &Characteristic, indent: &str) {
    let value = match &characteristic.value {
        Some(value) => value_text(value),
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
        "{}  {} ({}) = {}{} | {:?}\n",
        indent, characteristic.id, characteristic.label, value, stale, characteristic.status
    ));
    text.push_str(&format!(
        "{}    Access: read={} write={} notify={}\n",
        indent,
        characteristic.access.read,
        characteristic.access.write,
        characteristic.access.notify
    ));
    match &characteristic.spec {
        CharacteristicSpec::Number {
            minimum,
            maximum,
            step,
            integer,
            unit,
        } => {
            text.push_str(&format!("{}    Number: minimum={minimum:?} maximum={maximum:?} step={step:?} integer={integer} unit={}\n", indent, unit.as_deref().unwrap_or("none")));
        }
        CharacteristicSpec::Enum { options } => {
            text.push_str(&format!("{}    Enum options:", indent));
            for option in options {
                text.push_str(&format!(" {:?} ({})", option.value, option.label));
            }
            text.push('\n');
        }
        CharacteristicSpec::String { max_length } => text.push_str(&format!(
            "{}    String: maximum {max_length} characters\n",
            indent
        )),
        CharacteristicSpec::Boolean => {
            text.push_str(&format!("{}    Boolean: true or false\n", indent))
        }
        CharacteristicSpec::Action => {
            text.push_str(&format!("{}    Action: invoke without arguments\n", indent))
        }
    }
    if let Some(error) = &characteristic.error {
        text.push_str(&format!(
            "{}    Error: {:?}: {}\n",
            indent, error.code, error.message
        ));
    }
}

fn value_text(value: &CharacteristicValue) -> String {
    match value {
        CharacteristicValue::Boolean(value) => value.to_string(),
        CharacteristicValue::Number(value) => value.to_string(),
        CharacteristicValue::String(value) => format!("{value:?}"),
    }
}

#[test]
fn grouped_device_text_uses_declared_sections() {
    let device: Device = serde_json::from_value(serde_json::json!({
            "id": "monitor", "driver": "test-driver", "kind": "display", "label": "Monitor",
            "status": "online", "error": null,
            "groups": [{"id": "general", "label": "General"}, {"id": "sensors", "label": "Sensors"}],
            "characteristics": [
                {"id": "brightness", "label": "Brightness", "group": "general", "spec": {"kind": "number", "minimum": 0, "maximum": 100, "step": 5, "integer": true, "unit": null}, "access": {"read": true, "write": true, "notify": true}, "value": 50, "status": "ok", "error": null},
                {"id": "ambient_light", "label": "Ambient Light", "group": "sensors", "spec": {"kind": "number", "minimum": 0, "maximum": 1000, "step": null, "integer": true, "unit": null}, "access": {"read": true, "write": false, "notify": true}, "value": 80, "status": "ok", "error": null}
            ]
        })).unwrap();
    let mut text = String::new();
    render_device(&mut text, &device);
    assert_eq!(
        text,
        concat!(
            "Device monitor: Monitor | kind=display | driver=test-driver | Online\n",
            "  [General]\n",
            "    brightness (Brightness) = 50 | Ok\n",
            "      Access: read=true write=true notify=true\n",
            "      Number: minimum=Some(0.0) maximum=Some(100.0) step=Some(5.0) integer=true unit=none\n",
            "  [Sensors]\n",
            "    ambient_light (Ambient Light) = 80 | Ok\n",
            "      Access: read=true write=false notify=true\n",
            "      Number: minimum=Some(0.0) maximum=Some(1000.0) step=None integer=true unit=none\n"
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> InventoryResponse {
        serde_json::from_value(serde_json::json!({
            "apiVersion": 1,
            "instanceId": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            "generatedAt": "2025-01-02T03:04:05Z",
            "sources": [
                {"kind": "usb", "state": "ok", "message": null},
                {"kind": "ddc", "state": "not_scanned", "message": null}
            ],
            "nodes": [
                {"id": "n0000000000000000", "parentId": null, "kind": "host", "label": "This machine", "detail": null, "vendorId": null, "productId": null, "speedMbps": null, "connected": true, "attachment": "host", "relation": "exact", "rule": null, "driver": null},
                {"id": "n0000000000000001", "parentId": "n0000000000000000", "kind": "controller", "label": "Graphics", "detail": null, "vendorId": null, "productId": null, "speedMbps": null, "connected": true, "attachment": "host", "relation": "exact", "rule": null, "driver": null},
                {"id": "n0000000000000002", "parentId": "n0000000000000001", "kind": "dock", "label": "TS4", "detail": null, "vendorId": null, "productId": null, "speedMbps": null, "connected": true, "attachment": "dock", "relation": "inferred", "rule": "usb4_tunnel", "driver": null}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn inventory_tree_is_indented_and_labels_inference() {
        let response = response();
        response.validate().unwrap();
        assert_eq!(
            render_inventory(&response),
            "Inventory:\nSources:\n  usb: Ok\n  ddc: NotScanned\nHost: This machine\n  Controller: Graphics\n    Dock: TS4 [inferred: USB4 tunnel]\n"
        );
    }

    #[test]
    fn inventory_json_preserves_the_exact_response_object() {
        let response = response();
        let output = inventory_json(&response).unwrap();
        assert_eq!(
            output,
            format!("{}\n", serde_json::to_string(&response).unwrap())
        );
        assert_eq!(
            serde_json::from_str::<InventoryResponse>(&output).unwrap(),
            response
        );
    }
}
