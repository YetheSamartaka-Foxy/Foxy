//! Pushes the app-global settings that tune how Foxy works into the process:
//! for the GUI on load, reset, space switch, and edit, and for every CLI
//! command after it loads the settings.

use crate::core::tasks::calculate_hashes::{HashStorageClass, set_storage_class_override};
use crate::core::utils::proxy::{ProxySettings, set_proxy_settings};
use crate::core::utils::resource_profile::{ResourcePressure, set_pressure_override};
use crate::ui::types::{MemoryPressurePreference, SettingsViewState, StorageTypePreference};

pub fn apply_runtime_settings(settings: &SettingsViewState) {
    super::cpu_threads::set_cpu_thread_limit(
        settings.cpu_thread_limit,
        settings.cpu_thread_limit_safe,
    );
    super::work_priority::set_work_priority(settings.work_priority, settings.low_disk_priority);
    set_storage_class_override(storage_class(settings.storage_type_override));
    set_pressure_override(memory_pressure(settings.memory_pressure_override));
    set_proxy_settings(proxy_settings(settings));
}

pub fn proxy_settings(settings: &SettingsViewState) -> ProxySettings {
    ProxySettings {
        mode: settings.proxy_mode,
        url: settings.proxy_url.clone(),
        username: settings.proxy_username.clone(),
        password: settings.proxy_password.clone(),
        bypass: settings.proxy_bypass.clone(),
    }
}

fn storage_class(preference: StorageTypePreference) -> Option<HashStorageClass> {
    match preference {
        StorageTypePreference::Auto => None,
        StorageTypePreference::Ssd => Some(HashStorageClass::Ssd),
        StorageTypePreference::Hdd => Some(HashStorageClass::Hdd),
        StorageTypePreference::Removable => Some(HashStorageClass::Removable),
    }
}

fn memory_pressure(preference: MemoryPressurePreference) -> Option<ResourcePressure> {
    match preference {
        MemoryPressurePreference::Auto => None,
        MemoryPressurePreference::Normal => Some(ResourcePressure::Normal),
        MemoryPressurePreference::Constrained => Some(ResourcePressure::Constrained),
        MemoryPressurePreference::Severe => Some(ResourcePressure::Severe),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_preferences_force_nothing() {
        assert_eq!(storage_class(StorageTypePreference::Auto), None);
        assert_eq!(memory_pressure(MemoryPressurePreference::Auto), None);
        assert_eq!(
            storage_class(StorageTypePreference::Hdd),
            Some(HashStorageClass::Hdd)
        );
        assert_eq!(
            memory_pressure(MemoryPressurePreference::Constrained),
            Some(ResourcePressure::Constrained)
        );
    }
}
