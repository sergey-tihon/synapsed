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
