//! Public client configuration only. Never reads the daemon's private configuration.
use crate::{ClientError, validate_endpoint};
use serde::Deserialize;
use std::{
    env,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use url::Url;

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:24816";
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientConfig {
    endpoint: String,
}

/// CLI precedence: explicit endpoint, environment, selected configuration, default.
/// Native clients pass `None, None` to use environment/default client configuration.
/// An explicitly selected configuration is still validated when an endpoint overrides it.
pub fn load_endpoint(explicit: Option<&str>, selected: Option<&Path>) -> Result<Url, ClientError> {
    let environment = if explicit.is_none() {
        env::var_os("SYNAPSED_ENDPOINT")
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| ClientError::Config("SYNAPSED_ENDPOINT must be UTF-8."))
            })
            .transpose()?
    } else {
        None
    };
    let path = match selected {
        Some(path) => Some(path.to_path_buf()),
        None => default_config_path()?,
    };
    let configured = match path {
        Some(path) => read_config(&path, selected.is_some())?,
        None => None,
    };
    let endpoint = explicit
        .or(environment.as_deref())
        .or(configured.as_deref())
        .unwrap_or(DEFAULT_ENDPOINT);
    parse_endpoint(endpoint)
}

pub fn default_config_path() -> Result<Option<PathBuf>, ClientError> {
    if let Some(directory) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let directory = PathBuf::from(directory);
        if !directory.is_absolute() {
            return Err(ClientError::Config("XDG_CONFIG_HOME must be absolute."));
        }
        return Ok(Some(directory.join("synapsed/client.toml")));
    }
    Ok(env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config/synapsed/client.toml")))
}

fn read_config(path: &Path, explicit: bool) -> Result<Option<String>, ClientError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit => return Ok(None),
        Err(_) => {
            return Err(ClientError::Config(
                "Cannot read selected client configuration.",
            ));
        }
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ClientError::Config("Cannot read client configuration."))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(ClientError::Config("Client configuration exceeds 16 KiB."));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ClientError::Config("Client configuration must be UTF-8."))?;
    let config: ClientConfig = toml::from_str(text).map_err(|_| {
        ClientError::Config("Invalid client configuration; only endpoint is accepted.")
    })?;
    parse_endpoint(&config.endpoint)?;
    Ok(Some(config.endpoint))
}

pub fn parse_endpoint(value: &str) -> Result<Url, ClientError> {
    // Url canonicalization can erase an empty userinfo marker; reject it before parsing.
    if value.split_once("://").is_some_and(|(_, rest)| {
        rest.split('/')
            .next()
            .is_some_and(|authority| authority.contains('@'))
    }) {
        return Err(ClientError::Config(
            "Endpoint must not contain credentials.",
        ));
    }
    let endpoint = Url::parse(value)
        .map_err(|_| ClientError::Config("Endpoint must be an HTTP loopback origin."))?;
    validate_endpoint(&endpoint)?;
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_policy_rejects_non_origins_and_remote_hosts() {
        for endpoint in [
            "https://localhost",
            "http://example.com",
            "http://0.0.0.0",
            "http://[::]",
            "http://localhost/x",
            "http://localhost?",
            "http://localhost#",
            "http://a@localhost",
            "http://@localhost",
        ] {
            assert!(parse_endpoint(endpoint).is_err(), "{endpoint}");
        }
        for endpoint in [
            "http://localhost:24816",
            "http://127.0.0.2",
            "http://[::1]:24816/",
        ] {
            assert!(parse_endpoint(endpoint).is_ok(), "{endpoint}");
        }
    }

    #[test]
    fn explicit_files_fail_instead_of_falling_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("client.toml");
        assert!(read_config(&path, true).is_err());
        assert_eq!(read_config(&path, false).unwrap(), None);
        for text in [
            "endpoint = 'http://localhost'\ntoken = 'secret'",
            "endpoint = 'http://example.com'",
            "[server]\nlisten = '127.0.0.1:1'",
            "",
            "endpoint = 1",
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(read_config(&path, true).is_err());
        }
        std::fs::write(&path, "endpoint = 'http://[::1]:24816'").unwrap();
        assert_eq!(
            read_config(&path, true).unwrap().as_deref(),
            Some("http://[::1]:24816")
        );
    }
}
