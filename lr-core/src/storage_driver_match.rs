//! Selects built-in storage-controller driver packages from present PCI hardware IDs.
//!
//! Storage miniport drivers are boot-critical. Never stage every packaged driver recursively:
//! only a package whose INF explicitly covers a controller reported by SetupAPI may cross the
//! offline-DISM boundary.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltInStorageDriverPackage {
    /// Intel VMD 20.2.4.1019, retained for the 11th-generation 9A0B controller.
    IntelVmd11th,
    /// Intel VMD 20.2.12.1036 for later 467F/A77F/7D0B/AD0B controllers.
    IntelVmdCurrent,
}

impl BuiltInStorageDriverPackage {
    pub const fn directory_name(self) -> &'static str {
        match self {
            Self::IntelVmd11th => "intel-vmd-11th",
            Self::IntelVmdCurrent => "intel-vmd-current",
        }
    }
}

const INTEL_VMD_11TH: &str = "PCI\\VEN_8086&DEV_9A0B";
const INTEL_VMD_MANAGED: &str = "PCI\\VEN_8086&DEV_09AB";
const INTEL_VMD_CURRENT: [&str; 4] = [
    "PCI\\VEN_8086&DEV_467F",
    "PCI\\VEN_8086&DEV_A77F",
    "PCI\\VEN_8086&DEV_7D0B",
    "PCI\\VEN_8086&DEV_AD0B",
];

fn contains_device_id(hardware_id: &str, device_id: &str) -> bool {
    let normalized = hardware_id.trim().to_ascii_uppercase();
    normalized == device_id
        || normalized
            .strip_prefix(device_id)
            .is_some_and(|suffix| suffix.starts_with('&'))
}

/// Returns only packages that match a storage controller present in the running environment.
///
/// `09AB` is a managed/dummy VMD function and does not identify the processor generation by
/// itself. When it is the only visible VMD function, the broad 20.2.4 package is the conservative
/// fallback. AMD, Apple, VirtIO and unrelated Intel IDs intentionally select nothing.
pub fn select_builtin_storage_driver_packages<'a>(
    hardware_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<BuiltInStorageDriverPackage> {
    let ids: Vec<&str> = hardware_ids.into_iter().collect();
    let has_11th = ids.iter().any(|id| contains_device_id(id, INTEL_VMD_11TH));
    let has_current = ids.iter().any(|id| {
        INTEL_VMD_CURRENT
            .iter()
            .any(|device_id| contains_device_id(id, device_id))
    });
    let has_managed = ids
        .iter()
        .any(|id| contains_device_id(id, INTEL_VMD_MANAGED));

    let mut selected = Vec::with_capacity(2);
    if has_11th || (has_managed && !has_current) {
        selected.push(BuiltInStorageDriverPackage::IntelVmd11th);
    }
    if has_current {
        selected.push(BuiltInStorageDriverPackage::IntelVmdCurrent);
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_11th_generation_vmd_by_exact_vendor_and_device() {
        assert_eq!(
            select_builtin_storage_driver_packages([
                "PCI\\VEN_8086&DEV_9A0B&SUBSYS_00000000",
                "PCI\\VEN_8086&DEV_09AB",
            ]),
            vec![BuiltInStorageDriverPackage::IntelVmd11th]
        );
    }

    #[test]
    fn selects_current_vmd_without_also_staging_legacy_for_managed_function() {
        assert_eq!(
            select_builtin_storage_driver_packages([
                "pci\\ven_8086&dev_467f&cc_0104",
                "PCI\\VEN_8086&DEV_09AB",
            ]),
            vec![BuiltInStorageDriverPackage::IntelVmdCurrent]
        );
    }

    #[test]
    fn managed_function_alone_uses_conservative_vmd_package() {
        assert_eq!(
            select_builtin_storage_driver_packages(["PCI\\VEN_8086&DEV_09AB"]),
            vec![BuiltInStorageDriverPackage::IntelVmd11th]
        );
    }

    #[test]
    fn amd_virtio_apple_and_similar_prefixes_select_nothing() {
        for hardware_id in [
            "PCI\\VEN_1022&DEV_43BD",
            "PCI\\VEN_1AF4&DEV_1001",
            "PCI\\VEN_106B&DEV_2001",
            "PCI\\VEN_8086&DEV_9A0C",
            "PCI\\VEN_8086&DEV_467F0",
        ] {
            assert!(select_builtin_storage_driver_packages([hardware_id]).is_empty());
        }
    }
}
