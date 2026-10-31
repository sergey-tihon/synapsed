//! Linux sysfs and BlueZ read-only collectors.

pub mod bluez;
pub mod drm;
pub mod thunderbolt;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology;
    use std::{fs, os::unix::fs::symlink};
    use tempfile::tempdir;

    fn usb_device(root: &std::path::Path, name: &str, values: &[(&'static str, String)]) {
        let path = root.join("bus/usb/devices").join(name);
        let target = root.join("devices").join(name);
        fs::create_dir_all(&target).unwrap();
        for (key, value) in values {
            fs::write(target.join(*key), value.as_bytes()).unwrap();
        }
        symlink(target, path).unwrap();
    }

    fn usb_values(
        vendor: &str,
        product: &str,
        maker: &str,
        label: &str,
        class: &str,
    ) -> Vec<(&'static str, String)> {
        vec![
            ("idVendor", vendor.to_owned()),
            ("idProduct", product.to_owned()),
            ("manufacturer", maker.to_owned()),
            ("product", label.to_owned()),
            ("speed", "480".to_owned()),
            ("bDeviceClass", class.to_owned()),
            ("removable", "0".to_owned()),
        ]
    }

    #[test]
    fn caldigit_usb4_tunnel_collectors_preserve_both_port_subtrees() {
        let temp = tempdir().unwrap();
        let root = temp.path();
        let usb_bus = root.join("bus/usb/devices");
        let tb_bus = root.join("bus/thunderbolt/devices");
        fs::create_dir_all(&usb_bus).unwrap();
        fs::create_dir_all(&tb_bus).unwrap();

        for bus in ["5", "6"] {
            let host = root.join(format!("devices/pci0000:68/0000:68:00.3/usb{bus}"));
            fs::create_dir_all(&host).unwrap();
            symlink(host, usb_bus.join(format!("usb{bus}"))).unwrap();
        }
        let fixtures = [
            ("5-1", "2188", "0450", "CalDigit, Inc.", "USB2 Hub", "09"),
            ("5-1.5", "0bda", "5411", "Realtek", "USB2.1 Hub", "09"),
            ("5-1.5.6", "0bda", "1100", "Realtek", "HID Device", "03"),
            ("6-1", "8087", "0b40", "Intel", "USB hub", "09"),
            ("6-1.4", "2188", "0451", "CalDigit, Inc.", "USB3 Hub", "09"),
            ("6-1.4.2", "0fd9", "0066", "Elgato", "Facecam", "0e"),
        ];
        for (name, vendor, product, maker, label, class) in fixtures {
            let attrs = usb_values(vendor, product, maker, label, class);
            usb_device(root, name, &attrs);
        }

        let domain = root.join("devices/pci0000:68/0000:68:00.5/thunderbolt/domain0");
        let root_switch = root.join("devices/pci0000:68/0000:68:00.5/thunderbolt/0-0");
        let dock = root.join("devices/pci0000:68/0000:68:00.5/thunderbolt/0-2");
        fs::create_dir_all(&domain).unwrap();
        fs::create_dir_all(root_switch.join("usb4_port2")).unwrap();
        fs::create_dir_all(&dock).unwrap();
        fs::write(root_switch.join("usb4_port2/link"), "usb4\n").unwrap();
        fs::write(dock.join("vendor_name"), "CalDigit, Inc.").unwrap();
        fs::write(dock.join("device_name"), "TS4").unwrap();
        symlink(domain, tb_bus.join("domain0")).unwrap();
        symlink(root_switch, tb_bus.join("0-0")).unwrap();
        symlink(dock, tb_bus.join("0-2")).unwrap();

        let usb = usb::collect(root);
        let thunderbolt = thunderbolt::collect(root);
        let mut records = usb.records;
        records.extend(thunderbolt.records);
        let response = topology::build_with_sources(
            &records,
            &Default::default(),
            &[],
            b"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
            vec![usb.status, thunderbolt.status],
        );
        let dock = response
            .nodes
            .iter()
            .find(|node| node.label == "CalDigit, Inc. TS4")
            .unwrap();
        for label in ["CalDigit, Inc. USB2 Hub", "Intel USB hub"] {
            let node = response
                .nodes
                .iter()
                .find(|node| node.label == label)
                .unwrap();
            assert_eq!(node.parent_id.as_deref(), Some(dock.id.as_str()));
            assert_eq!(node.rule, Some(synapsed_api::InventoryRule::Usb4Tunnel));
        }
        assert!(
            response
                .nodes
                .iter()
                .any(|node| node.label == "Elgato Facecam"
                    && node.attachment == synapsed_api::InventoryAttachment::Dock)
        );
        assert!(response.validate().is_ok());
    }
}
pub mod usb;
