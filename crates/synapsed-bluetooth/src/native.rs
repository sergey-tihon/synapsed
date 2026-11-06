use crate::{BackendEvent, BackendFuture, BluetoothBackend, DriverError, ErrorCode, Uuid};
use btleplug::{
    api::{
        Central, CentralEvent, CharPropFlags, Characteristic, Manager as _, Peripheral as _,
        ScanFilter, WriteType,
    },
    platform::{Adapter, Manager, Peripheral},
};
use futures_util::StreamExt;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tokio::sync::mpsc;

pub(super) struct NativeBackend {
    adapter: Rc<RefCell<Option<Adapter>>>,
    peripherals: Rc<RefCell<HashMap<String, Peripheral>>>,
    characteristics: Rc<RefCell<HashMap<String, Vec<Characteristic>>>>,
    events: Rc<RefCell<Option<mpsc::Sender<BackendEvent>>>>,
}

impl NativeBackend {
    pub(super) fn new() -> Result<Self, DriverError> {
        Ok(Self {
            adapter: Rc::new(RefCell::new(None)),
            peripherals: Rc::new(RefCell::new(HashMap::new())),
            characteristics: Rc::new(RefCell::new(HashMap::new())),
            events: Rc::new(RefCell::new(None)),
        })
    }
}

impl BluetoothBackend for NativeBackend {
    fn start_scan(&mut self, events: mpsc::Sender<BackendEvent>) -> BackendFuture<()> {
        let adapter_slot = self.adapter.clone();
        let peripherals = self.peripherals.clone();
        let event_slot = self.events.clone();
        Box::pin(async move {
            let manager = Manager::new()
                .await
                .map_err(|_| bluetooth_error("Bluetooth scan failed."))?;
            let mut adapters = manager
                .adapters()
                .await
                .map_err(|_| bluetooth_error("Bluetooth scan failed."))?;
            let adapter = adapters.drain(..).next().ok_or_else(|| {
                DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "No Bluetooth adapter is available.",
                )
            })?;
            let mut stream = adapter
                .events()
                .await
                .map_err(|_| bluetooth_error("Bluetooth scan failed."))?;
            adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(|_| bluetooth_error("Bluetooth scan failed."))?;
            *adapter_slot.borrow_mut() = Some(adapter.clone());
            *event_slot.borrow_mut() = Some(events.clone());
            tokio::task::spawn_local(async move {
                while let Some(event) = stream.next().await {
                    match event {
                        CentralEvent::DeviceDisconnected(id) => {
                            let _ = events.try_send(BackendEvent::Disconnected {
                                key: id.to_string(),
                            });
                        }
                        CentralEvent::DeviceDiscovered(id)
                        | CentralEvent::DeviceUpdated(id)
                        | CentralEvent::ServicesAdvertisement { id, .. } => {
                            let key = id.to_string();
                            let Ok(peripheral) = adapter.peripheral(&id).await else {
                                continue;
                            };
                            let properties = peripheral.properties().await.ok().flatten();
                            let name = properties
                                .as_ref()
                                .and_then(|value| value.local_name.clone());
                            let services =
                                properties.map(|value| value.services).unwrap_or_default();
                            if events
                                .try_send(BackendEvent::Advertisement {
                                    key: key.clone(),
                                    name,
                                    services,
                                })
                                .is_ok()
                            {
                                peripherals.borrow_mut().insert(key, peripheral);
                            }
                        }
                        _ => {}
                    }
                }
            });
            Ok(())
        })
    }

    fn forget(&mut self, key: &str) {
        self.peripherals.borrow_mut().remove(key);
        self.characteristics.borrow_mut().remove(key);
    }
    fn connect(&mut self, key: &str) -> BackendFuture<()> {
        let key = key.to_owned();
        let peripheral = self.peripherals.borrow().get(&key).cloned();
        let characteristics = self.characteristics.clone();
        let events = self.events.borrow().clone();
        let Some(peripheral) = peripheral else {
            return Box::pin(async {
                Err(DriverError::new(
                    ErrorCode::DeviceUnavailable,
                    "Bluetooth peripheral is not available.",
                ))
            });
        };
        Box::pin(async move {
            peripheral
                .connect()
                .await
                .map_err(|_| bluetooth_error("Bluetooth connection failed."))?;
            peripheral
                .discover_services()
                .await
                .map_err(|_| bluetooth_error("Bluetooth connection failed."))?;
            characteristics.borrow_mut().insert(
                key.clone(),
                peripheral.characteristics().into_iter().collect(),
            );
            let mut notifications = peripheral
                .notifications()
                .await
                .map_err(|_| bluetooth_error("Bluetooth connection failed."))?;
            if let Some(events) = events {
                let notification_key = key;
                tokio::task::spawn_local(async move {
                    while let Some(notification) = notifications.next().await {
                        let _ = events.try_send(BackendEvent::Notification {
                            key: notification_key.clone(),
                            characteristic: notification.uuid,
                            bytes: notification.value,
                        });
                    }
                });
            }
            Ok(())
        })
    }

    fn disconnect(&mut self, key: &str) -> BackendFuture<()> {
        let peripheral = self.peripherals.borrow().get(key).cloned();
        Box::pin(async move {
            let Some(peripheral) = peripheral else {
                return Ok(());
            };
            peripheral
                .disconnect()
                .await
                .map_err(|_| bluetooth_error("Bluetooth connection failed."))
        })
    }

    fn read(&mut self, key: &str, characteristic: Uuid) -> BackendFuture<Vec<u8>> {
        let peripheral = self.peripherals.borrow().get(key).cloned();
        let characteristic = lookup(&self.characteristics, key, characteristic);
        Box::pin(async move {
            let peripheral = peripheral.ok_or_else(peripheral_unavailable)?;
            let characteristic = characteristic?;
            peripheral
                .read(&characteristic)
                .await
                .map_err(|_| bluetooth_error("Bluetooth read failed."))
        })
    }

    fn write(&mut self, key: &str, uuid: Uuid, bytes: Vec<u8>) -> BackendFuture<()> {
        let peripheral = self.peripherals.borrow().get(key).cloned();
        let characteristic = lookup(&self.characteristics, key, uuid);
        Box::pin(async move {
            let peripheral = peripheral.ok_or_else(peripheral_unavailable)?;
            let characteristic = characteristic?;
            let write_type = if characteristic.properties.contains(CharPropFlags::WRITE) {
                WriteType::WithResponse
            } else if characteristic
                .properties
                .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
            {
                WriteType::WithoutResponse
            } else {
                return Err(DriverError::new(
                    ErrorCode::UnsupportedOperation,
                    "Bluetooth characteristic does not support writes.",
                ));
            };
            peripheral
                .write(&characteristic, &bytes, write_type)
                .await
                .map_err(|_| bluetooth_error("Bluetooth write failed."))
        })
    }

    fn subscribe(&mut self, key: &str, uuid: Uuid) -> BackendFuture<()> {
        let peripheral = self.peripherals.borrow().get(key).cloned();
        let characteristic = lookup(&self.characteristics, key, uuid);
        Box::pin(async move {
            let peripheral = peripheral.ok_or_else(peripheral_unavailable)?;
            let characteristic = characteristic?;
            peripheral
                .subscribe(&characteristic)
                .await
                .map_err(|_| bluetooth_error("Bluetooth notification setup failed."))
        })
    }
}

fn lookup(
    cache: &Rc<RefCell<HashMap<String, Vec<Characteristic>>>>,
    key: &str,
    uuid: Uuid,
) -> Result<Characteristic, DriverError> {
    let matches: Vec<_> = cache
        .borrow()
        .get(key)
        .into_iter()
        .flatten()
        .filter(|characteristic| characteristic.uuid == uuid)
        .cloned()
        .collect();
    match matches.len() {
        0 => Err(DriverError::new(
            ErrorCode::UnsupportedOperation,
            "Bluetooth characteristic is not available on this peripheral.",
        )),
        1 => Ok(matches[0].clone()),
        _ => Err(bluetooth_error(
            "Bluetooth characteristic UUID is ambiguous on this peripheral.",
        )),
    }
}

fn peripheral_unavailable() -> DriverError {
    DriverError::new(
        ErrorCode::DeviceUnavailable,
        "Bluetooth peripheral is not available.",
    )
}

fn bluetooth_error(message: &'static str) -> DriverError {
    DriverError::new(ErrorCode::BluetoothError, message)
}
