use std::sync::atomic::{AtomicU8, Ordering};

use sysinfo::System;

/// Pressure the user's memory setting forces; 0 means Auto.
static PRESSURE_OVERRIDE: AtomicU8 = AtomicU8::new(0);

/// Force a pressure tier for every later [`ResourceProfile::sample`], or `None`
/// to use the sampled one.
pub(crate) fn set_pressure_override(pressure: Option<ResourcePressure>) {
    let encoded = match pressure {
        None => 0,
        Some(ResourcePressure::Normal) => 1,
        Some(ResourcePressure::Constrained) => 2,
        Some(ResourcePressure::Severe) => 3,
    };
    if PRESSURE_OVERRIDE.swap(encoded, Ordering::Relaxed) != encoded {
        match pressure {
            Some(pressure) => log::info!("Memory pressure forced to {pressure}"),
            None => log::info!("Memory pressure set to auto"),
        }
    }
}

fn pressure_override() -> Option<ResourcePressure> {
    match PRESSURE_OVERRIDE.load(Ordering::Relaxed) {
        1 => Some(ResourcePressure::Normal),
        2 => Some(ResourcePressure::Constrained),
        3 => Some(ResourcePressure::Severe),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourcePressure {
    Normal,
    Constrained,
    Severe,
}

impl std::fmt::Display for ResourcePressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Normal => write!(f, "normal"),
            Self::Constrained => write!(f, "constrained"),
            Self::Severe => write!(f, "severe"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResourceProfile {
    pub(crate) pressure: ResourcePressure,
    pub(crate) total_memory: u64,
    pub(crate) available_memory: u64,
    /// Logged for diagnosis only. Windows reports commit charge beyond physical
    /// memory here rather than pagefile use, and swapped pages stay counted
    /// elsewhere long after the pressure that pushed them out has gone.
    pub(crate) used_swap: u64,
    /// `pressure` came from the user's setting, not from the memory figures.
    pub(crate) pressure_overridden: bool,
}

impl ResourceProfile {
    pub(crate) fn sample() -> Self {
        let mut system = System::new();
        system.refresh_memory();
        Self::from_memory(
            system.total_memory(),
            system.available_memory(),
            system.used_swap(),
        )
        .with_pressure_override(pressure_override())
    }

    fn with_pressure_override(mut self, pressure: Option<ResourcePressure>) -> Self {
        if let Some(pressure) = pressure {
            self.pressure = pressure;
            self.pressure_overridden = true;
        }
        self
    }

    pub(crate) fn from_memory(total_memory: u64, available_memory: u64, used_swap: u64) -> Self {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;

        let pressure = if total_memory <= 6 * GIB || available_memory < 1536 * MIB {
            ResourcePressure::Severe
        } else if total_memory <= 8704 * MIB || available_memory < 4 * GIB {
            ResourcePressure::Constrained
        } else {
            ResourcePressure::Normal
        };

        Self {
            pressure,
            total_memory,
            available_memory,
            used_swap,
            pressure_overridden: false,
        }
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "pressure={}{} total={} available={} swap_used={}",
            self.pressure,
            if self.pressure_overridden {
                " (override)"
            } else {
                ""
            },
            format_bytes(self.total_memory),
            format_bytes(self.available_memory),
            format_bytes(self.used_swap)
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.2} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eight_gib_with_three_gib_available_is_constrained() {
        let profile =
            ResourceProfile::from_memory(8 * 1024 * 1024 * 1024, 3 * 1024 * 1024 * 1024, 0);
        assert_eq!(profile.pressure, ResourcePressure::Constrained);
    }

    #[test]
    fn very_low_available_memory_is_severe() {
        let profile = ResourceProfile::from_memory(16 * 1024 * 1024 * 1024, 1024 * 1024 * 1024, 0);
        assert_eq!(profile.pressure, ResourcePressure::Severe);
    }

    #[test]
    fn swap_in_use_does_not_raise_the_pressure_while_memory_is_free() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let roomy = ResourceProfile::from_memory(94 * GIB, 40 * GIB, 5 * GIB);
        assert_eq!(roomy.pressure, ResourcePressure::Normal);
        assert!(roomy.summary().ends_with("swap_used=5.00 GiB"));
        let small = ResourceProfile::from_memory(8 * GIB, 5 * GIB, 3 * GIB);
        assert_eq!(small.pressure, ResourcePressure::Constrained);
    }

    #[test]
    fn an_override_replaces_the_sampled_pressure_and_says_so() {
        let sampled =
            ResourceProfile::from_memory(32 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, 0);
        assert_eq!(sampled.with_pressure_override(None), sampled);
        let forced = sampled.with_pressure_override(Some(ResourcePressure::Severe));
        assert_eq!(forced.pressure, ResourcePressure::Severe);
        assert!(forced.summary().starts_with("pressure=severe (override) "));
    }

    #[test]
    fn healthy_memory_is_normal() {
        let profile =
            ResourceProfile::from_memory(32 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, 0);
        assert_eq!(profile.pressure, ResourcePressure::Normal);
    }
}
