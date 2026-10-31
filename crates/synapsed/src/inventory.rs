use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use synapsed_api::{InventoryResponse, InventorySource, InventorySourceKind, InventorySourceState};
use synapsed_core::{DriverError, ErrorCode, StateStore};
use synapsed_ddc::{CachedEndpointScan, LazyDdc, Placement};
use synapsed_inventory::{
    Bus, DeviceBinding, DriverCatalogEntry, RawRecord, SourceCollection, build_with_sources,
};
use synapsed_plugins::Bindings;
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::{Instant as TokioInstant, timeout_at},
};

const COLLECTION_DEADLINE: Duration = Duration::from_secs(3);
const CACHE_DURATION: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct Sources {
    store: StateStore,
    ddc: LazyDdc,
    bindings: watch::Receiver<Arc<Bindings>>,
    catalog: Arc<[DriverCatalogEntry]>,
    sysfs_root: PathBuf,
    cache: Arc<Mutex<Option<CachedResponse>>>,
}
struct CachedResponse {
    created: Instant,
    response: InventoryResponse,
}
struct DdcCollection {
    source: SourceCollection,
    aliases: BTreeMap<String, String>,
}
impl Sources {
    pub fn new(
        store: StateStore,
        ddc: LazyDdc,
        bindings: watch::Receiver<Arc<Bindings>>,
        catalog: Arc<[DriverCatalogEntry]>,
    ) -> Self {
        Self::with_sysfs_root(store, ddc, bindings, catalog, PathBuf::from("/sys"))
    }

    #[doc(hidden)]
    pub fn with_sysfs_root(
        store: StateStore,
        ddc: LazyDdc,
        bindings: watch::Receiver<Arc<Bindings>>,
        catalog: Arc<[DriverCatalogEntry]>,
        sysfs_root: PathBuf,
    ) -> Self {
        Self {
            store,
            ddc,
            bindings,
            catalog,
            sysfs_root,
            cache: Arc::new(Mutex::new(None)),
        }
    }
}

pub struct Inventory;
impl Inventory {
    pub async fn collect(sources: &Sources) -> InventoryResponse {
        let deadline = TokioInstant::now() + COLLECTION_DEADLINE;
        let Ok(mut cache) = timeout_at(deadline, sources.cache.lock()).await else {
            return timeout_response(sources);
        };
        if let Some(cached) = cache.as_ref()
            && cached.created.elapsed() < CACHE_DURATION
        {
            return cached.response.clone();
        }
        let response = collect_uncached(sources, deadline).await;
        *cache = Some(CachedResponse {
            created: Instant::now(),
            response: response.clone(),
        });
        response
    }
}

async fn collect_uncached(sources: &Sources, deadline: TokioInstant) -> InventoryResponse {
    let (usb_task, thunderbolt_task, bluetooth_task) =
        spawn_platform_collectors(&sources.sysfs_root);
    let ddc_task = tokio::spawn(collect_ddc(sources.ddc.clone()));
    let (usb, thunderbolt, bluetooth, ddc) = tokio::join!(
        await_source(usb_task, InventorySourceKind::Usb, deadline),
        await_source(thunderbolt_task, InventorySourceKind::Thunderbolt, deadline),
        await_source(bluetooth_task, InventorySourceKind::Bluetooth, deadline),
        await_ddc(ddc_task, deadline),
    );

    let mut records = Vec::new();
    records.extend(usb.records);
    records.extend(thunderbolt.records);
    records.extend(bluetooth.records);
    let ddc_aliases = ddc.aliases;
    records.extend(ddc.source.records);
    let source_states = vec![
        usb.status,
        thunderbolt.status,
        ddc.source.status,
        bluetooth.status,
    ];

    let snapshot = sources.store.snapshot();
    let bindings: BTreeMap<String, DeviceBinding> = sources
        .bindings
        .borrow()
        .iter()
        .map(|(device_id, binding)| {
            (
                device_id.clone(),
                DeviceBinding {
                    driver: binding.driver.clone(),
                    endpoints: binding
                        .endpoints
                        .iter()
                        .map(|token| {
                            let key = endpoint_key(token);
                            ddc_aliases.get(&key).cloned().unwrap_or(key)
                        })
                        .collect(),
                },
            )
        })
        .collect();
    let mut response = build_with_sources(
        &records,
        &bindings,
        &sources.catalog,
        snapshot.node.node_id.as_bytes(),
        source_states,
    );
    response.instance_id = snapshot.node.instance_id.clone();
    response
}

fn endpoint_key(token: &synapsed_core::EndpointToken) -> String {
    format!("{}-{}", token.id, token.generation)
}

async fn collect_ddc(ddc: LazyDdc) -> DdcCollection {
    let Some(client) = ddc.existing() else {
        return DdcCollection {
            source: empty_source(
                InventorySourceKind::Ddc,
                InventorySourceState::NotScanned,
                Some("The DDC owner has not started a scan."),
            ),
            aliases: BTreeMap::new(),
        };
    };
    match client.last_scan().await {
        Ok(None) => DdcCollection {
            source: empty_source(
                InventorySourceKind::Ddc,
                InventorySourceState::NotScanned,
                Some("The DDC owner has not completed its first scan."),
            ),
            aliases: BTreeMap::new(),
        },
        Ok(Some(scan)) => collect_cached_ddc(scan),
        Err(error) => DdcCollection {
            source: SourceCollection {
                records: Vec::new(),
                status: ddc_status(Some(&error)),
            },
            aliases: BTreeMap::new(),
        },
    }
}

fn collect_cached_ddc(scan: CachedEndpointScan) -> DdcCollection {
    let mut records = Vec::new();
    let mut aliases = BTreeMap::new();
    let mut controllers = BTreeSet::new();
    for (endpoint, placement) in scan.endpoints {
        let Some(placement) = placement else { continue };
        if controllers.insert(placement.controller_key.clone()) {
            records.push(RawRecord::Controller {
                key: placement.controller_key.clone(),
                label: "Display controller".to_owned(),
                bus: Bus::Display,
            });
        }
        let token_key = endpoint_key(&endpoint.token);
        let display_key = stable_display_key(&endpoint, &placement);
        aliases.insert(token_key, display_key.clone());
        records.push(display_record(endpoint, placement, display_key));
    }
    DdcCollection {
        source: SourceCollection {
            records,
            status: ddc_status(scan.error.as_ref()),
        },
        aliases,
    }
}

fn ddc_status(error: Option<&DriverError>) -> InventorySource {
    let (state, message) = match error {
        Some(error) => {
            let state = match error.code {
                ErrorCode::PermissionDenied => InventorySourceState::PermissionDenied,
                ErrorCode::Timeout => InventorySourceState::Timeout,
                _ => InventorySourceState::Error,
            };
            (state, Some(error.message.clone()))
        }
        None => (InventorySourceState::Ok, None),
    };
    InventorySource {
        kind: InventorySourceKind::Ddc,
        state,
        message,
    }
}

fn stable_display_key(
    endpoint: &synapsed_ddc::EndpointDescriptor,
    placement: &Placement,
) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        placement.controller_key,
        placement.connector,
        endpoint.identity.as_deref().unwrap_or_default(),
        endpoint.manufacturer.as_deref().unwrap_or_default(),
        endpoint.product_name.as_deref().unwrap_or_default(),
    )
}

fn display_record(
    endpoint: synapsed_ddc::EndpointDescriptor,
    placement: Placement,
    stable_key: String,
) -> RawRecord {
    RawRecord::Display {
        endpoint_token: stable_key,
        connector: placement.connector,
        controller_key: placement.controller_key,
        manufacturer: endpoint.manufacturer,
        product_name: endpoint.product_name,
    }
}

fn spawn_platform_collectors(
    root: &Path,
) -> (
    JoinHandle<SourceCollection>,
    JoinHandle<SourceCollection>,
    JoinHandle<SourceCollection>,
) {
    #[cfg(target_os = "linux")]
    {
        let usb_root = root.to_path_buf();
        let thunderbolt_root = root.to_path_buf();
        let bluetooth_root = root.to_path_buf();
        (
            tokio::task::spawn_blocking(move || synapsed_inventory::linux::usb::collect(&usb_root)),
            tokio::task::spawn_blocking(move || {
                synapsed_inventory::linux::thunderbolt::collect(&thunderbolt_root)
            }),
            tokio::task::spawn_blocking(move || {
                synapsed_inventory::linux::bluez::collect(&bluetooth_root)
            }),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let _ = root;
        (
            tokio::task::spawn_blocking(synapsed_inventory::macos::usb::collect),
            tokio::task::spawn_blocking(synapsed_inventory::macos::thunderbolt::collect),
            tokio::task::spawn_blocking(synapsed_inventory::macos::bluetooth::collect),
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        (
            unavailable_task(InventorySourceKind::Usb),
            unavailable_task(InventorySourceKind::Thunderbolt),
            unavailable_task(InventorySourceKind::Bluetooth),
        )
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unavailable_task(kind: InventorySourceKind) -> JoinHandle<SourceCollection> {
    tokio::task::spawn_blocking(move || {
        empty_source(
            kind,
            InventorySourceState::Unavailable,
            Some("This source is unsupported on this platform."),
        )
    })
}

async fn await_source(
    task: JoinHandle<SourceCollection>,
    kind: InventorySourceKind,
    deadline: TokioInstant,
) -> SourceCollection {
    match timeout_at(deadline, task).await {
        Ok(Ok(source)) => source,
        Ok(Err(_)) => empty_source(
            kind,
            InventorySourceState::Error,
            Some("The inventory source failed."),
        ),
        Err(_) => empty_source(
            kind,
            InventorySourceState::Timeout,
            Some("The inventory source exceeded the collection deadline."),
        ),
    }
}

async fn await_ddc(task: JoinHandle<DdcCollection>, deadline: TokioInstant) -> DdcCollection {
    match timeout_at(deadline, task).await {
        Ok(Ok(source)) => source,
        Ok(Err(_)) => DdcCollection {
            source: empty_source(
                InventorySourceKind::Ddc,
                InventorySourceState::Error,
                Some("The DDC inventory source failed."),
            ),
            aliases: BTreeMap::new(),
        },
        Err(_) => DdcCollection {
            source: empty_source(
                InventorySourceKind::Ddc,
                InventorySourceState::Timeout,
                Some("The DDC inventory source exceeded the collection deadline."),
            ),
            aliases: BTreeMap::new(),
        },
    }
}

fn empty_source(
    kind: InventorySourceKind,
    state: InventorySourceState,
    message: Option<&str>,
) -> SourceCollection {
    SourceCollection {
        records: Vec::new(),
        status: InventorySource {
            kind,
            state,
            message: message.map(str::to_owned),
        },
    }
}

fn timeout_response(sources: &Sources) -> InventoryResponse {
    let snapshot = sources.store.snapshot();
    let states = [
        InventorySourceKind::Usb,
        InventorySourceKind::Thunderbolt,
        InventorySourceKind::Ddc,
        InventorySourceKind::Bluetooth,
    ]
    .into_iter()
    .map(|kind| InventorySource {
        kind,
        state: InventorySourceState::Timeout,
        message: Some("The inventory collection deadline expired.".to_owned()),
    })
    .collect();
    let mut response = build_with_sources(
        &[],
        &BTreeMap::new(),
        &sources.catalog,
        snapshot.node.node_id.as_bytes(),
        states,
    );
    response.instance_id = snapshot.node.instance_id.clone();
    response
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_ddc_scan_error_is_visible_in_source_status() {
        let error = DriverError::new(
            ErrorCode::PermissionDenied,
            "An eligible adapter was denied.",
        );
        let collected = collect_cached_ddc(CachedEndpointScan {
            endpoints: Vec::new(),
            error: Some(error),
        });
        assert_eq!(collected.source.status.kind, InventorySourceKind::Ddc);
        assert_eq!(
            collected.source.status.state,
            InventorySourceState::PermissionDenied
        );
        assert_eq!(
            collected.source.status.message.as_deref(),
            Some("An eligible adapter was denied.")
        );
    }
}
