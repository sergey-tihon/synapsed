//! Loopback-only Synapsed daemon composition. No built-in devices.
pub mod access;
pub mod config;
pub mod inventory;
pub mod logs;
pub mod server;
#[cfg(feature = "web-ui")]
mod web;

use config::Config;
use std::sync::Arc;
use synapsed_api::ApiError;
use synapsed_core::StateStore;
use synapsed_ddc::LazyDdc;
use synapsed_plugins::{
    CapabilityRegistry, PluginConfig, PluginHost, capabilities::ddc::DdcCapabilityFactory,
};

/// Register transport metadata now, but create its native owner only on a real
/// enabled package invocation. An empty/disabled package catalog does no DDC I/O.
pub fn start_plugins(
    store: StateStore,
    config: &Config,
) -> Result<(PluginHost, LazyDdc), ApiError> {
    let ddc = LazyDdc::new();
    let mut registry = CapabilityRegistry::default();
    registry.register(Arc::new(DdcCapabilityFactory::with_shared(ddc.clone())))?;
    let host = PluginHost::start(
        store,
        PluginConfig {
            directory: config.plugin_directory.clone(),
            directory_explicit: config.plugin_directory_explicit,
            disabled: config.disabled.clone(),
            discovery_interval_ms: config.discovery_interval_ms,
            settings: config.settings.clone(),
        },
        registry,
    )?;
    Ok((host, ddc))
}
