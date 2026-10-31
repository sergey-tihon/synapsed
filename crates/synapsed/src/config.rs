//! Local configuration and private, persistent process identity.
use fs2::FileExt;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use synapsed_api::{ApiError, ErrorCode, NodeInfo};
use uuid::Uuid;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
fn invalid(message: &'static str) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest, message)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub allowed_origins: Vec<String>,
    pub name: Option<String>,
    pub state_dir: PathBuf,
    pub plugin_directory: PathBuf,
    pub plugin_directory_explicit: bool,
    pub disabled: Vec<String>,
    pub discovery_interval_ms: u64,
    pub settings: BTreeMap<String, Value>,
}
#[derive(Clone, Debug)]
pub struct ConfigPaths {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub state_home: PathBuf,
}
impl ConfigPaths {
    pub fn from_environment() -> Result<Self, ApiError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid("HOME must identify an absolute home directory."))?;
        let xdg = |key, fallback: &str| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        Ok(Self {
            config_home: xdg("XDG_CONFIG_HOME", ".config"),
            data_home: xdg("XDG_DATA_HOME", ".local/share"),
            state_home: xdg("XDG_STATE_HOME", ".local/state"),
            home,
        })
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_home.join("synapsed/config.toml")
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    server: RawServer,
    plugins: RawPlugins,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawServer {
    listen: String,
    allowed_origins: Vec<String>,
    name: Option<String>,
    state_dir: Option<PathBuf>,
}
impl Default for RawServer {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:24816".into(),
            allowed_origins: vec![],
            name: None,
            state_dir: None,
        }
    }
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPlugins {
    directory: Option<PathBuf>,
    disabled: Vec<String>,
    discovery_interval_ms: u64,
    settings: BTreeMap<String, toml::Value>,
}
impl Default for RawPlugins {
    fn default() -> Self {
        Self {
            directory: None,
            disabled: vec![],
            discovery_interval_ms: 5000,
            settings: BTreeMap::new(),
        }
    }
}
impl Config {
    pub fn load(selected: Option<&Path>) -> Result<Self, ApiError> {
        Self::load_with_paths(selected, &ConfigPaths::from_environment()?)
    }
    pub fn load_with_paths(selected: Option<&Path>, paths: &ConfigPaths) -> Result<Self, ApiError> {
        let path = match selected {
            Some(path) => resolve_path(
                path,
                &std::env::current_dir()
                    .map_err(|_| invalid("Cannot resolve the configuration directory."))?,
                &paths.home,
            )?,
            None => paths.config_file(),
        };
        let mut source = String::new();
        match File::open(&path) {
            Ok(file) => {
                file.take(MAX_CONFIG_BYTES + 1)
                    .read_to_string(&mut source)
                    .map_err(|_| invalid("Cannot read daemon configuration."))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && selected.is_none() => {}
            Err(_) => return Err(invalid("Cannot open the selected daemon configuration.")),
        }
        Self::from_text(&source, &path, paths)
    }
    pub fn from_text(
        source: &str,
        config_file: &Path,
        paths: &ConfigPaths,
    ) -> Result<Self, ApiError> {
        if source.len() as u64 > MAX_CONFIG_BYTES {
            return Err(invalid("Daemon configuration exceeds 64 KiB."));
        }
        let raw: RawConfig = toml::from_str(source).map_err(|_| {
            invalid("Malformed daemon configuration or unknown configuration field.")
        })?;
        let listen: SocketAddr = raw
            .server
            .listen
            .parse()
            .map_err(|_| invalid("Listen must be a literal loopback IP and port."))?;
        if !listen.ip().is_loopback() {
            return Err(invalid(
                "Only literal loopback listen addresses are supported.",
            ));
        }
        if raw.server.allowed_origins.len() > 32 {
            return Err(invalid("Too many allowed browser origins."));
        }
        for origin in &raw.server.allowed_origins {
            crate::access::validate_origin(origin)?;
        }
        if let Some(name) = &raw.server.name {
            synapsed_api::validate_label(name)?;
        }
        if raw.plugins.discovery_interval_ms < 1000
            || raw.plugins.discovery_interval_ms > u32::MAX as u64
        {
            return Err(invalid(
                "Discovery interval must be between 1000 and 4294967295 milliseconds.",
            ));
        }
        if raw.plugins.disabled.len() > 16 || raw.plugins.settings.len() > 16 {
            return Err(invalid("Plugin configuration exceeds the package limit."));
        }
        let mut disabled = std::collections::BTreeSet::new();
        for id in &raw.plugins.disabled {
            synapsed_api::validate_driver_id(id)?;
            if !disabled.insert(id) {
                return Err(invalid("Disabled package IDs must be unique."));
            }
        }
        let mut settings = BTreeMap::new();
        for (id, value) in &raw.plugins.settings {
            synapsed_api::validate_driver_id(id)?;
            if !value.is_table() {
                return Err(invalid("Package settings must be a table."));
            }
            let mut entries = 0;
            validate_settings(value, 0, &mut entries)?;
            settings.insert(
                id.clone(),
                serde_json::to_value(value)
                    .map_err(|_| invalid("Package settings must be plain JSON data."))?,
            );
        }
        let base = config_file
            .parent()
            .ok_or_else(|| invalid("Configuration must have a parent directory."))?;
        let plugin_directory_explicit = raw.plugins.directory.is_some();
        Ok(Self {
            listen,
            allowed_origins: raw.server.allowed_origins,
            name: raw.server.name,
            state_dir: match raw.server.state_dir {
                Some(path) => resolve_path(&path, base, &paths.home)?,
                None => paths.state_home.join("synapsed"),
            },
            plugin_directory: match raw.plugins.directory {
                Some(path) => resolve_path(&path, base, &paths.home)?,
                None => paths.data_home.join("synapsed/drivers"),
            },
            plugin_directory_explicit,
            disabled: raw.plugins.disabled,
            discovery_interval_ms: raw.plugins.discovery_interval_ms,
            settings,
        })
    }
}
fn resolve_path(path: &Path, base: &Path, home: &Path) -> Result<PathBuf, ApiError> {
    if path.as_os_str().is_empty() {
        return Err(invalid("Configured paths must not be empty."));
    }
    if path == Path::new("~") {
        return Ok(home.to_path_buf());
    }
    if let Ok(relative) = path.strip_prefix("~") {
        return Ok(home.join(relative));
    }
    if path.to_string_lossy().starts_with('~') {
        return Err(invalid("Only leading ~/ home expansion is supported."));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}
fn validate_settings(
    value: &toml::Value,
    depth: usize,
    entries: &mut usize,
) -> Result<(), ApiError> {
    *entries += 1;
    if depth > 8 || *entries > 2048 {
        return Err(invalid("Package settings exceed plain-data limits."));
    }
    match value {
        toml::Value::String(value) if value.chars().count() > 1024 => {
            Err(invalid("Package setting string exceeds 1024 characters."))
        }
        toml::Value::Float(value) if !value.is_finite() => {
            Err(invalid("Package setting numbers must be finite."))
        }
        toml::Value::Datetime(_) => Err(invalid(
            "Package settings must be plain JSON data, not TOML dates.",
        )),
        toml::Value::Array(values) => {
            for value in values {
                validate_settings(value, depth + 1, entries)?;
            }
            Ok(())
        }
        toml::Value::Table(values) => {
            for (key, value) in values {
                if key.chars().count() > 128 {
                    return Err(invalid("Package setting key is too long."));
                }
                validate_settings(value, depth + 1, entries)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The lock file stays owned for the entire daemon lifetime, including shutdown.
pub struct PrivateState {
    _lock: File,
    pub node: NodeInfo,
}
impl PrivateState {
    pub fn acquire(directory: &Path, name: Option<&str>) -> Result<Self, ApiError> {
        match fs::symlink_metadata(directory) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(invalid("Private state must be a real directory."));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(directory)
                    .map_err(|_| invalid("Cannot create private state directory."))?
            }
            Err(_) => return Err(invalid("Cannot inspect private state directory.")),
        }
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|_| invalid("Cannot protect private state directory."))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(directory.join("daemon.lock"))
            .map_err(|_| invalid("Cannot open the daemon lock."))?;
        if !lock
            .metadata()
            .map_err(|_| invalid("Cannot inspect the daemon lock."))?
            .is_file()
        {
            return Err(invalid("Daemon lock must be a regular file."));
        }
        lock.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| invalid("Cannot protect the daemon lock."))?;
        lock.try_lock_exclusive()
            .map_err(|_| invalid("The private state directory is already locked."))?;
        let node_path = directory.join("node-id");
        if !node_path
            .try_exists()
            .map_err(|_| invalid("Cannot inspect the persistent node ID."))?
        {
            let temporary = directory.join(format!(".node-id-{}", Uuid::new_v4()));
            let result = (|| {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&temporary)
                    .map_err(|_| invalid("Cannot create persistent node ID."))?;
                writeln!(file, "{}", Uuid::new_v4())
                    .map_err(|_| invalid("Cannot write persistent node ID."))?;
                file.sync_all()
                    .map_err(|_| invalid("Cannot synchronize persistent node ID."))?;
                fs::hard_link(&temporary, &node_path)
                    .map_err(|_| invalid("Cannot atomically install persistent node ID."))?;
                File::open(directory)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| invalid("Cannot synchronize private state directory."))
            })();
            let _ = fs::remove_file(&temporary);
            result?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&node_path)
            .map_err(|_| invalid("Cannot read persistent node ID."))?;
        if !file
            .metadata()
            .map_err(|_| invalid("Cannot inspect persistent node ID."))?
            .is_file()
        {
            return Err(invalid("Persistent node ID must be a regular file."));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| invalid("Cannot protect persistent node ID."))?;
        let mut stored = String::new();
        (&mut file)
            .take(65)
            .read_to_string(&mut stored)
            .map_err(|_| invalid("Cannot read persistent node ID."))?;
        let node_id = stored.strip_suffix('\n').unwrap_or(&stored);
        let id = Uuid::parse_str(node_id)
            .map_err(|_| invalid("Persistent node ID is corrupt; it will not be regenerated."))?;
        if id.get_version_num() != 4 || id.to_string() != node_id {
            return Err(invalid("Persistent node ID must be a canonical UUID-v4."));
        }
        let name = match name {
            Some(name) => name.to_owned(),
            None => hostname::get()
                .map_err(|_| invalid("Cannot determine the OS hostname."))?
                .to_string_lossy()
                .chars()
                .take(128)
                .collect(),
        };
        let node = NodeInfo {
            node_id: node_id.to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            name,
        };
        node.validate()?;
        Ok(Self { _lock: lock, node })
    }
}
