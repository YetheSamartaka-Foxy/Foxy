use sysinfo::System;

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
    pub(crate) used_swap: u64,
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
    }

    pub(crate) fn from_memory(total_memory: u64, available_memory: u64, used_swap: u64) -> Self {
        const MIB: u64 = 1024 * 1024;
        const GIB: u64 = 1024 * MIB;

        let pressure = if total_memory <= 6 * GIB
            || available_memory < 1536 * MIB
            || used_swap >= 2 * GIB
        {
            ResourcePressure::Severe
        } else if total_memory <= 8704 * MIB || available_memory < 4 * GIB || used_swap >= 512 * MIB
        {
            ResourcePressure::Constrained
        } else {
            ResourcePressure::Normal
        };

        Self {
            pressure,
            total_memory,
            available_memory,
            used_swap,
        }
    }

    pub(crate) fn summary(&self) -> String {
        format!(
            "pressure={} total={} available={} swap_used={}",
            self.pressure,
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
    fn healthy_memory_is_normal() {
        let profile =
            ResourceProfile::from_memory(32 * 1024 * 1024 * 1024, 16 * 1024 * 1024 * 1024, 0);
        assert_eq!(profile.pressure, ResourcePressure::Normal);
    }
}
