use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use synapsed_api::{InventoryResponse, InventorySource, InventorySourceKind, InventorySourceState};
use synapsed_core::{DriverError, ErrorCode, StateStore};
use synapsed_ddc::{CachedEndpointScan, LazyDdc, Placement};
use synapsed_inventory::{
    BoundEndpoint, Bus, DeviceBinding, DriverCatalogEntry, RawRecord, SourceCollection,
    build_with_sources,
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
    let bindings = device_bindings(&sources.bindings.borrow(), &ddc_aliases);
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

fn device_bindings(
    bindings: &Bindings,
    ddc_aliases: &BTreeMap<String, String>,
) -> BTreeMap<String, DeviceBinding> {
    bindings
        .iter()
        .map(|(device_id, binding)| {
            let endpoints = binding
                .endpoints
                .iter()
                .filter_map(|endpoint| match endpoint.capability.as_str() {
                    "ddc" => ddc_aliases
                        .get(&endpoint_key(&endpoint.token))
                        .cloned()
                        .map(BoundEndpoint::Display),
                    "bluetooth" => {
                        endpoint
                            .identity
                            .clone()
                            .map(|identity| BoundEndpoint::Bluetooth {
                                identity,
                                name: endpoint.product_name.clone(),
                            })
                    }
                    _ => None,
                })
                .collect();
            (
                device_id.clone(),
                DeviceBinding {
                    driver: binding.driver.clone(),
                    endpoints,
                },
            )
        })
        .collect()
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
    let mut placed_by_identity = HashMap::<String, BTreeSet<String>>::new();
    let mut unplaced = Vec::new();
    for (endpoint, placement) in scan.endpoints {
        let Some(placement) = placement else {
            unplaced.push(endpoint);
            continue;
        };
        if controllers.insert(placement.controller_key.clone()) {
            records.push(RawRecord::Controller {
                key: placement.controller_key.clone(),
                label: "Display controller".to_owned(),
                bus: Bus::Display,
            });
        }
        let token_key = endpoint_key(&endpoint.token);
        let display_key = stable_display_key(&endpoint, &placement);
        if let Some(identity) = endpoint
            .identity
            .as_ref()
            .filter(|identity| !identity.is_empty())
        {
            placed_by_identity
                .entry(identity.clone())
                .or_default()
                .insert(display_key.clone());
        }
        aliases.insert(token_key, display_key.clone());
        records.push(display_record(endpoint, Some(placement), display_key));
    }

    let mut unplaced_by_identity = BTreeMap::<String, String>::new();
    for endpoint in unplaced {
        let token_key = endpoint_key(&endpoint.token);
        let identity = endpoint
            .identity
            .as_deref()
            .filter(|identity| !identity.is_empty());
        let (display_key, create_record) = if let Some(identity) = identity {
            if let Some(placed) = placed_by_identity.get(identity) {
                if placed.len() != 1 {
                    continue;
                }
                (placed.iter().next().unwrap().clone(), false)
            } else if let Some(display_key) = unplaced_by_identity.get(identity) {
                (display_key.clone(), false)
            } else {
                let display_key = stable_unplaced_display_key(&endpoint);
                unplaced_by_identity.insert(identity.to_owned(), display_key.clone());
                (display_key, true)
            }
        } else {
            (stable_unplaced_display_key(&endpoint), true)
        };
        if create_record {
            records.push(display_record(endpoint, None, display_key.clone()));
        }
        aliases.insert(token_key, display_key);
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
    endpoint: &synapsed_core::EndpointDescriptor,
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

fn stable_unplaced_display_key(endpoint: &synapsed_core::EndpointDescriptor) -> String {
    match endpoint
        .identity
        .as_deref()
        .filter(|identity| !identity.is_empty())
    {
        Some(identity) => format!("unplaced|{identity}"),
        None => format!(
            "unplaced|endpoint-{}-{}",
            endpoint.token.id, endpoint.token.generation
        ),
    }
}

fn display_record(
    endpoint: synapsed_core::EndpointDescriptor,
    placement: Option<Placement>,
    stable_key: String,
) -> RawRecord {
    let (connector, controller_key) = placement.map_or((None, None), |placement| {
        (Some(placement.connector), Some(placement.controller_key))
    });
    RawRecord::Display {
        endpoint_token: stable_key,
        connector,
        controller_key,
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

    #[test]
    fn unplaced_ddc_endpoints_are_counted_and_bind_to_their_driver() {
        let endpoint = synapsed_core::EndpointDescriptor {
            token: synapsed_core::EndpointToken {
                id: 7,
                generation: 1,
            },
            capability: "ddc".to_owned(),
            manufacturer: Some("BNQ".to_owned()),
            product: Some(0x1234),
            product_name: Some("BenQ RD280U".to_owned()),
            identity: Some("opaque-edid-identity".to_owned()),
        };
        let duplicate = synapsed_core::EndpointDescriptor {
            token: synapsed_core::EndpointToken {
                id: 8,
                generation: 1,
            },
            ..endpoint.clone()
        };
        let identityless = synapsed_core::EndpointDescriptor {
            token: synapsed_core::EndpointToken {
                id: 9,
                generation: 1,
            },
            identity: None,
            ..endpoint.clone()
        };
        let collected = collect_cached_ddc(CachedEndpointScan {
            endpoints: vec![
                (endpoint.clone(), None),
                (duplicate.clone(), None),
                (identityless, None),
            ],
            error: None,
        });
        assert_eq!(collected.source.status.state, InventorySourceState::Ok);
        assert_eq!(collected.aliases.len(), 3);
        assert_eq!(
            collected.aliases[&endpoint_key(&endpoint.token)],
            collected.aliases[&endpoint_key(&duplicate.token)]
        );

        let published = BTreeMap::from([(
            "monitor".to_owned(),
            synapsed_plugins::Binding {
                driver: "benq-rd280u".to_owned(),
                endpoints: vec![endpoint],
            },
        )]);
        let bindings = device_bindings(&published, &collected.aliases);
        let catalog = [DriverCatalogEntry {
            id: "benq-rd280u".to_owned(),
            version: "0.1.0".to_owned(),
            generic: false,
            inventory_matches: Vec::new(),
        }];
        let response = synapsed_inventory::build(
            &collected.source.records,
            &bindings,
            &catalog,
            b"inventory-test-salt",
        );
        let displays: Vec<_> = response
            .nodes
            .iter()
            .filter(|node| node.kind == synapsed_api::InventoryNodeKind::Display)
            .collect();
        assert_eq!(displays.len(), 2);
        let linked = displays.iter().find(|node| node.driver.is_some()).unwrap();
        assert!(linked.label.contains("BenQ RD280U"));
        assert_eq!(
            linked.detail.as_deref(),
            Some("Display location unavailable")
        );
        assert_eq!(
            linked.parent_id.as_deref(),
            Some(response.nodes[0].id.as_str())
        );
        let badge = linked.driver.as_ref().unwrap();
        assert_eq!(badge.id, "benq-rd280u");
        assert_eq!(badge.device_id.as_deref(), Some("monitor"));
        assert_eq!(badge.class, synapsed_api::InventoryDriverClass::Specialized);
        assert!(displays.iter().any(|node| node.driver.is_none()));
        assert!(response.validate().is_ok());
    }
}
