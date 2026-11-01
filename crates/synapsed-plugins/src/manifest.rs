use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use synapsed_core::{DriverError, ErrorCode};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub version: String,
    pub entry: String,
    pub dependencies: Vec<String>,
    pub supersedes: Vec<String>,
    #[serde(deserialize_with = "unique_map")]
    pub capabilities: BTreeMap<String, u32>,
    #[serde(deserialize_with = "unique_map")]
    pub permissions: BTreeMap<String, Value>,
    #[serde(default, deserialize_with = "present_inventory")]
    pub inventory: Option<ManifestInventory>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestInventory {
    #[serde(default)]
    pub(crate) usb: Vec<UsbInventoryMatch>,
    #[serde(default)]
    pub(crate) bluetooth: Vec<BluetoothInventoryMatch>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UsbInventoryMatch {
    pub(crate) vendor_id: String,
    pub(crate) product_id: String,
    pub(crate) role: ManifestInventoryRole,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BluetoothInventoryMatch {
    pub(crate) name_prefix: String,
    pub(crate) role: ManifestInventoryRole,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ManifestInventoryRole {
    MonitorCompanion,
    Device,
}
fn present_inventory<'de, D>(deserializer: D) -> Result<Option<ManifestInventory>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    ManifestInventory::deserialize(deserializer).map(Some)
}
fn valid_hex_id(value: &str) -> bool {
    value.len() == 4
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique capability IDs")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut entries: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = entries.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate capability ID"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Visitor(std::marker::PhantomData))
}
impl Manifest {
    pub fn validate(&self, directory: &str) -> Result<(), DriverError> {
        synapsed_api::validate_driver_id(&self.id)?;
        synapsed_api::validate_driver_version(&self.version)?;
        if self.id != directory || self.api_version != 1 || !relative_lua(&self.entry) {
            return Err(invalid());
        }
        for list in [&self.dependencies, &self.supersedes] {
            let mut seen = BTreeSet::new();
            for id in list {
                synapsed_api::validate_driver_id(id)?;
                if id == &self.id || !seen.insert(id) {
                    return Err(invalid());
                }
            }
        }
        if self.capabilities.len() > 16 || self.capabilities.keys().ne(self.permissions.keys()) {
            return Err(invalid());
        }
        for id in self.capabilities.keys() {
            synapsed_api::validate_driver_id(id)?;
        }
        if let Some(inventory) = &self.inventory
            && (inventory.usb.len() + inventory.bluetooth.len() > 16
                || inventory.usb.iter().any(|entry| {
                    !valid_hex_id(&entry.vendor_id) || !valid_hex_id(&entry.product_id)
                })
                || inventory
                    .bluetooth
                    .iter()
                    .any(|entry| entry.name_prefix.chars().count() > 64))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
pub(crate) fn relative_lua(path: &str) -> bool {
    !path.is_empty()
        && path.ends_with(".lua")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !std::path::Path::new(path).is_absolute()
}
pub(crate) fn invalid() -> DriverError {
    DriverError::new(
        ErrorCode::PluginError,
        "Plugin manifest or package data is invalid.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(inventory: Option<Value>) -> Result<Manifest, serde_json::Error> {
        let mut value = serde_json::json!({
            "apiVersion": 1,
            "id": "fixture-driver",
            "version": "0.1.0",
            "entry": "driver.lua",
            "dependencies": [],
            "supersedes": [],
            "capabilities": {},
            "permissions": {}
        });
        if let Some(inventory) = inventory {
            value
                .as_object_mut()
                .unwrap()
                .insert("inventory".to_owned(), inventory);
        }
        serde_json::from_value(value)
    }

    #[test]
    fn inventory_manifest_selectors_are_strict_and_bounded() {
        let valid = serde_json::json!({
            "usb": [{"vendorId": "0bda", "productId": "1100", "role": "monitor-companion"}],
            "bluetooth": [{"namePrefix": "Keyboard", "role": "device"}]
        });
        assert!(
            parse(Some(valid))
                .unwrap()
                .validate("fixture-driver")
                .is_ok()
        );
        assert!(parse(None).unwrap().validate("fixture-driver").is_ok());

        for invalid in [
            serde_json::json!({"usb": [{"vendorId": "0BDA", "productId": "1100", "role": "device"}]}),
            serde_json::json!({"usb": [{"vendorId": "bda", "productId": "1100", "role": "device"}]}),
            serde_json::json!({"usb": [{"vendorId": "0bda", "productId": "1100", "role": "unknown"}]}),
            serde_json::json!({"bluetooth": [{"namePrefix": "x".repeat(65), "role": "device"}]}),
            serde_json::json!({"usb": [], "unexpected": []}),
        ] {
            if let Ok(manifest) = parse(Some(invalid)) {
                assert!(manifest.validate("fixture-driver").is_err());
            }
        }

        let entries = (0..17)
            .map(|_| serde_json::json!({"vendorId": "0bda", "productId": "1100", "role": "device"}))
            .collect::<Vec<_>>();
        let too_many = serde_json::json!({"usb": entries});
        assert!(
            parse(Some(too_many))
                .unwrap()
                .validate("fixture-driver")
                .is_err()
        );
    }
}
