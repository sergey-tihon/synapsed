//! Read-only mapping from a DDC adapter to its DRM connector and PCI controller.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// Safe placement metadata associated with an owner-local DDC adapter key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// DRM connector name, for example `DP-1`.
    pub connector: String,
    /// PCI address of the graphics controller.
    pub controller_key: String,
}

fn valid_adapter_key(adapter_key: &str) -> bool {
    adapter_key.strip_prefix("i2c-").is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Resolve an i2c-dev adapter to its DRM connector, including connector `ddc` links.
pub fn connector_for_adapter(sysfs_root: &Path, adapter_key: &str) -> Option<PathBuf> {
    if !valid_adapter_key(adapter_key) {
        return None;
    }
    let target = sysfs_root
        .join("class/i2c-dev")
        .join(adapter_key)
        .join("device")
        .canonicalize()
        .ok()?;

    if let Some(connector) = target.ancestors().find(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| connector_name(name).is_some())
    }) {
        return Some(connector.to_path_buf());
    }

    let entries = fs::read_dir(sysfs_root.join("class/drm")).ok()?;
    entries.filter_map(Result::ok).find_map(|entry| {
        let name = entry.file_name();
        let name = name.to_str()?;
        connector_name(name)?;
        let connector = entry.path().canonicalize().ok()?;
        let ddc_target = connector.join("ddc").canonicalize().ok()?;
        (ddc_target == target).then_some(connector)
    })
}

/// Resolves `i2c-N` to its DRM connector and PCI controller.
pub fn placement_for_adapter(sysfs_root: &Path, adapter_key: &str) -> Option<Placement> {
    if !valid_adapter_key(adapter_key) {
        return None;
    }
    let target = sysfs_root
        .join("class/i2c-dev")
        .join(adapter_key)
        .join("device")
        .canonicalize()
        .ok()?;
    let connector_path = connector_for_adapter(sysfs_root, adapter_key)?;
    let connector = connector_name(connector_path.file_name()?.to_str()?)?;
    let controller_key = pci_ancestor(&target)?;
    Some(Placement {
        connector,
        controller_key,
    })
}

fn connector_name(component: &str) -> Option<String> {
    let (_, connector) = component.strip_prefix("card")?.split_once('-')?;
    if connector.is_empty()
        || !connector
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return None;
    }
    Some(connector.to_owned())
}

fn pci_ancestor(path: &Path) -> Option<String> {
    path.ancestors().find_map(|ancestor| {
        let component = ancestor.file_name()?.to_str()?;
        is_pci_address(component).then(|| component.to_owned())
    })
}

fn is_pci_address(value: &str) -> bool {
    let mut parts = value.split(':');
    let (Some(domain), Some(bus), Some(device_function)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if parts.next().is_some() || domain.len() != 4 || bus.len() != 2 {
        return false;
    }
    let Some((device, function)) = device_function.split_once('.') else {
        return false;
    };
    device.len() == 2
        && function.len() == 1
        && domain
            .bytes()
            .chain(bus.bytes())
            .chain(device.bytes())
            .chain(function.bytes())
            .all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;

    #[test]
    fn resolves_connector_ancestry_and_ddc_links() {
        let temp = tempdir().unwrap();
        let sys = temp.path();
        let device =
            sys.join("devices/pci0000:00/0000:00:08.1/0000:04:00.0/drm/card1/card1-DP-1/i2c-7");
        fs::create_dir_all(&device).unwrap();
        let class = sys.join("class/i2c-dev/i2c-7");
        fs::create_dir_all(&class).unwrap();
        symlink(&device, class.join("device")).unwrap();

        let hardware_device = sys.join("devices/pci0000:00/0000:00:08.1/0000:04:00.0/i2c-0");
        fs::create_dir_all(&hardware_device).unwrap();
        let hardware_class = sys.join("class/i2c-dev/i2c-0");
        fs::create_dir_all(&hardware_class).unwrap();
        symlink(&hardware_device, hardware_class.join("device")).unwrap();
        let drm_class = sys.join("class/drm/card1-DP-1");
        fs::create_dir_all(drm_class.parent().unwrap()).unwrap();
        symlink(
            sys.join("devices/pci0000:00/0000:00:08.1/0000:04:00.0/drm/card1/card1-DP-1"),
            &drm_class,
        )
        .unwrap();
        symlink(&hardware_device, drm_class.join("ddc")).unwrap();

        let placement = placement_for_adapter(sys, "i2c-7").unwrap();
        assert_eq!(
            placement,
            Placement {
                connector: "DP-1".to_owned(),
                controller_key: "0000:04:00.0".to_owned()
            }
        );
        assert_eq!(
            connector_for_adapter(sys, "i2c-0"),
            Some(drm_class.canonicalize().unwrap())
        );
        assert_eq!(
            placement_for_adapter(sys, "i2c-0"),
            Some(Placement {
                connector: "DP-1".to_owned(),
                controller_key: "0000:04:00.0".to_owned()
            })
        );
        assert!(placement_for_adapter(sys, "i2c-../etc/passwd").is_none());
        assert!(placement_for_adapter(sys, "i2c-99").is_none());
    }
}
