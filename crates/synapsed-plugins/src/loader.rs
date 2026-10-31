use crate::{
    PluginConfig,
    manifest::{Manifest, invalid},
    registry::CapabilityRegistry,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::Path,
    sync::Arc,
};
use synapsed_api::{DriverState, DriverStatus};
use synapsed_core::{DriverError, ErrorCode};

#[derive(Clone)]
pub(crate) struct Package {
    pub manifest: Manifest,
    pub sources: Arc<BTreeMap<String, Arc<[u8]>>>,
}
pub(crate) struct Catalog {
    pub packages: BTreeMap<String, Package>,
    pub statuses: BTreeMap<String, DriverStatus>,
}
struct UniqueJson(serde_json::Value);
impl<'de> serde::Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate fields")
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueJson(number.into()))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(value.into()))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                self.visit_none()
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJson>()? {
                    values.push(value.0);
                }
                Ok(UniqueJson(values.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut entries: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = entries.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate field"));
                    }
                    values.insert(key, entries.next_value::<UniqueJson>()?.0);
                }
                Ok(UniqueJson(values.into()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}
fn read_capped(path: &Path, maximum: usize) -> Result<Vec<u8>, DriverError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| invalid())?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| invalid())?;
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > maximum {
        return Err(invalid());
    }
    Ok(bytes)
}
fn sources(
    root: &Path,
    at: &Path,
    total: &mut usize,
    output: &mut BTreeMap<String, Arc<[u8]>>,
) -> Result<(), DriverError> {
    if at
        .strip_prefix(root)
        .map_err(|_| invalid())?
        .components()
        .count()
        > 64
    {
        return Err(invalid());
    }
    for item in fs::read_dir(at).map_err(|_| invalid())? {
        let item = item.map_err(|_| invalid())?;
        let kind = item.file_type().map_err(|_| invalid())?;
        if kind.is_symlink() {
            return Err(invalid());
        }
        if kind.is_dir() {
            sources(root, &item.path(), total, output)?;
        } else if item
            .path()
            .extension()
            .is_some_and(|extension| extension == "lua")
        {
            if output.len() >= 4096 {
                return Err(invalid());
            }
            let bytes = read_capped(&item.path(), 256 * 1024)?;
            *total += bytes.len();
            if *total > 1024 * 1024
                || bytes.first() == Some(&0x1b)
                || std::str::from_utf8(&bytes).is_err()
            {
                return Err(invalid());
            }
            let path = item.path();
            let name = path
                .strip_prefix(root)
                .map_err(|_| invalid())?
                .to_str()
                .ok_or_else(invalid)?
                .to_owned();
            output.insert(name, bytes.into());
        }
    }
    Ok(())
}
pub(crate) fn load(
    config: &PluginConfig,
    registry: &CapabilityRegistry,
) -> Result<Catalog, DriverError> {
    let mut catalog = Catalog {
        packages: BTreeMap::new(),
        statuses: BTreeMap::new(),
    };
    let Some((canonical_directory, directories)) = package_directories(config)? else {
        return Ok(catalog);
    };
    let reserved: BTreeSet<String> = directories
        .iter()
        .filter_map(|item| item.file_name().into_string().ok())
        .collect();
    for (index, item) in directories.into_iter().enumerate() {
        let name = item.file_name();
        let id = match name.to_str().filter(|name| name.chars().count() <= 256) {
            Some(name) => name.to_owned(),
            None => {
                use sha2::Digest;
                // Filename diagnostics only: this is never a native endpoint or hardware identity.
                let base = format!(
                    "invalid-directory-{:x}-{index}",
                    sha2::Sha256::digest(name.as_encoded_bytes())
                );
                let mut id = base.clone();
                let mut suffix = 0;
                while reserved.contains(&id) || catalog.statuses.contains_key(&id) {
                    suffix += 1;
                    id = format!("{base}-{suffix}");
                }
                id
            }
        };
        let mut status = DriverStatus {
            id: id.clone(),
            version: "unknown".into(),
            state: DriverState::Error,
            error: None,
        };
        match load_package(
            &item,
            &canonical_directory,
            &id,
            config,
            registry,
            &mut status,
        ) {
            Ok(Some(package)) => {
                catalog.packages.insert(id.clone(), package);
            }
            Ok(None) => {}
            Err(error) => {
                status.version = if synapsed_api::validate_driver_version(&status.version).is_ok() {
                    status.version
                } else {
                    "unknown".into()
                };
                status.state = DriverState::Error;
                status.error = Some(error);
            }
        }
        catalog.statuses.insert(id, status);
    }
    disable_invalid_dependencies(&mut catalog);
    disable_dependants(&mut catalog);
    Ok(catalog)
}
fn package_directories(
    config: &PluginConfig,
) -> Result<Option<(std::path::PathBuf, Vec<fs::DirEntry>)>, DriverError> {
    let listing = match fs::read_dir(&config.directory) {
        Ok(listing) => listing,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && !config.directory_explicit =>
        {
            return Ok(None);
        }
        Err(_) => {
            return Err(DriverError::new(
                ErrorCode::PluginError,
                "Configured plugin directory is not readable.",
            ));
        }
    };
    let canonical_directory = fs::canonicalize(&config.directory).map_err(|_| invalid())?;
    let mut directories = Vec::new();
    for item in listing {
        let item = item.map_err(|_| invalid())?;
        let kind = item.file_type().map_err(|_| invalid())?;
        if kind.is_dir() || kind.is_symlink() {
            directories.push(item);
        }
    }
    if directories.len() > 16 {
        return Err(DriverError::new(
            ErrorCode::PluginError,
            "Plugin catalog exceeds sixteen packages.",
        ));
    }
    directories.sort_by_key(|item| item.file_name());
    Ok(Some((canonical_directory, directories)))
}
fn load_package(
    item: &fs::DirEntry,
    canonical_directory: &Path,
    id: &str,
    config: &PluginConfig,
    registry: &CapabilityRegistry,
    status: &mut DriverStatus,
) -> Result<Option<Package>, DriverError> {
    if item.file_type().map_err(|_| invalid())?.is_symlink() {
        return Err(invalid());
    }
    let root = fs::canonicalize(item.path()).map_err(|_| invalid())?;
    if root.parent() != Some(canonical_directory) {
        return Err(invalid());
    }
    let manifest = parse_manifest(&root, id, status)?;
    if config
        .disabled
        .iter()
        .any(|disabled| disabled.as_str() == id)
    {
        status.state = DriverState::Disabled;
        return Ok(None);
    }
    registry.validate_manifest(&manifest)?;
    let mut frozen = BTreeMap::new();
    sources(&root, &root, &mut 0, &mut frozen)?;
    if !frozen.contains_key(&manifest.entry) {
        return Err(invalid());
    }
    status.state = DriverState::Disconnected;
    Ok(Some(Package {
        manifest,
        sources: Arc::new(frozen),
    }))
}
fn parse_manifest(
    root: &Path,
    id: &str,
    status: &mut DriverStatus,
) -> Result<Manifest, DriverError> {
    let value: UniqueJson =
        serde_json::from_slice(&read_capped(&root.join("plugin.json"), 64 * 1024)?)
            .map_err(|_| invalid())?;
    let manifest: Manifest = serde_json::from_value(value.0).map_err(|_| invalid())?;
    status.version = manifest.version.clone();
    manifest.validate(id)?;
    Ok(manifest)
}
fn disable_invalid_dependencies(catalog: &mut Catalog) {
    let all = catalog.packages.clone();
    for (id, package) in &all {
        if cycle(id, &all, false, &mut BTreeSet::new())
            || cycle(id, &all, true, &mut BTreeSet::new())
        {
            catalog.packages.remove(id);
            let status = catalog.statuses.get_mut(id).expect("catalog status");
            status.state = DriverState::Error;
            status.error = Some(DriverError::new(
                ErrorCode::PluginError,
                "Plugin dependency or supersedes graph contains a cycle.",
            ));
        } else if package
            .manifest
            .supersedes
            .iter()
            .any(|target| !catalog.statuses.contains_key(target))
        {
            catalog.packages.remove(id);
            let status = catalog.statuses.get_mut(id).expect("catalog status");
            status.state = DriverState::Error;
            status.error = Some(invalid());
        }
    }
}
pub(crate) fn disable_dependants(catalog: &mut Catalog) {
    loop {
        let failed: Vec<_> = catalog
            .packages
            .iter()
            .filter(|(_, package)| {
                package
                    .manifest
                    .dependencies
                    .iter()
                    .any(|id| !catalog.packages.contains_key(id))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if failed.is_empty() {
            break;
        }
        for id in failed {
            catalog.packages.remove(&id);
            let status = catalog.statuses.get_mut(&id).expect("catalog status");
            status.state = DriverState::Disabled;
            status.error = Some(DriverError::new(
                ErrorCode::PluginError,
                "Plugin dependency is missing, disabled or failed.",
            ));
        }
    }
}
fn cycle(
    id: &str,
    packages: &BTreeMap<String, Package>,
    supersedes: bool,
    path: &mut BTreeSet<String>,
) -> bool {
    if !path.insert(id.into()) {
        return true;
    }
    if let Some(package) = packages.get(id) {
        let edges = if supersedes {
            &package.manifest.supersedes
        } else {
            &package.manifest.dependencies
        };
        for edge in edges {
            if cycle(edge, packages, supersedes, path) {
                return true;
            }
        }
    }
    path.remove(id);
    false
}
/// Resolve only frozen source names; no filesystem access is possible after startup.
pub(crate) fn module_source<'a>(
    current: &str,
    name: &str,
    packages: &'a BTreeMap<String, Package>,
) -> Result<(&'a str, &'a str, &'a [u8]), DriverError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
    {
        return Err(invalid());
    }
    let package = packages.get(current).ok_or_else(invalid)?;
    let mut parts = name.split('.');
    let first = parts.next().ok_or_else(invalid)?;
    let (owner, relative) = if package.manifest.dependencies.iter().any(|id| id == first) {
        (first, parts.collect::<Vec<_>>().join("/"))
    } else {
        (current, name.replace('.', "/"))
    };
    if relative.is_empty() {
        return Err(invalid());
    }
    let (owner, dependency) = packages.get_key_value(owner).ok_or_else(invalid)?;
    let (path, source) = dependency
        .sources
        .get_key_value(&format!("{relative}.lua"))
        .ok_or_else(invalid)?;
    Ok((owner, path, source))
}
