use serde::{Deserialize, Serialize};

const RECOVERY_MS: u64 = 10_000;
const MEMORY_FLOOR: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Vad,
    Asr,
    Translation,
    Speaker,
}

impl JobKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Vad => "vad",
            Self::Asr => "asr",
            Self::Translation => "translation",
            Self::Speaker => "speaker",
        }
    }

    pub fn priority(self) -> u8 {
        match self {
            Self::Vad => 0,
            Self::Asr => 1,
            Self::Translation | Self::Speaker => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    #[default]
    Normal,
    DeferOptional,
    ReducePartials,
    DeferAsr,
    Unload,
}

impl Tier {
    pub fn admits(self, kind: JobKind) -> bool {
        match kind {
            JobKind::Vad => self < Self::Unload,
            JobKind::Asr => self < Self::DeferAsr,
            JobKind::Translation | JobKind::Speaker => self < Self::DeferOptional,
        }
    }

    pub fn partial_seconds(self) -> usize {
        if self >= Self::ReducePartials { 6 } else { 3 }
    }

    fn recover(self) -> Self {
        match self {
            Self::Unload => Self::DeferAsr,
            Self::DeferAsr => Self::ReducePartials,
            Self::ReducePartials => Self::DeferOptional,
            Self::DeferOptional | Self::Normal => Self::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Heat {
    Normal,
    Moderate,
    Severe,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceSample {
    pub low_memory: Option<bool>,
    pub available_memory_bytes: Option<u64>,
    pub low_memory_threshold_bytes: Option<u64>,
    pub storage_available_bytes: Option<u64>,
    pub heat: Option<Heat>,
    pub power_saver: Option<bool>,
    pub battery_percent: Option<u8>,
}

impl ResourceSample {
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn with_fallback(self, previous: Self) -> Self {
        Self {
            low_memory: self.low_memory.or(previous.low_memory),
            available_memory_bytes: self
                .available_memory_bytes
                .or(previous.available_memory_bytes),
            low_memory_threshold_bytes: self
                .low_memory_threshold_bytes
                .or(previous.low_memory_threshold_bytes),
            storage_available_bytes: self
                .storage_available_bytes
                .or(previous.storage_available_bytes),
            heat: self.heat.or(previous.heat),
            power_saver: self.power_saver.or(previous.power_saver),
            battery_percent: self.battery_percent.or(previous.battery_percent),
        }
    }

    pub fn emergency(self) -> Option<Reason> {
        let reserve = self
            .low_memory_threshold_bytes
            .unwrap_or_default()
            .max(MEMORY_FLOOR);
        if self.low_memory == Some(true)
            || self
                .available_memory_bytes
                .is_some_and(|amount| amount <= reserve)
        {
            Some(Reason::Memory)
        } else if self.heat == Some(Heat::Severe) {
            Some(Reason::Thermal)
        } else if self.battery_percent.is_some_and(|percent| percent <= 5) {
            Some(Reason::Battery)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Recording,
    Memory,
    Thermal,
    Battery,
    Storage,
    Backlog,
    SlowInference,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Recording => "recording_active",
            Self::Memory => "memory_pressure",
            Self::Thermal => "thermal_pressure",
            Self::Battery => "battery_pressure",
            Self::Storage => "storage_pressure",
            Self::Backlog => "backlog",
            Self::SlowInference => "slow_inference",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Observation {
    pub resources: ResourceSample,
    pub recording_active: bool,
    pub live_transcription: bool,
    pub rtf: Option<f64>,
    pub pending_jobs: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub tier: Tier,
    pub reason: Option<Reason>,
}

#[derive(Default)]
pub struct Governor {
    current: Decision,
    recovery_since: Option<u64>,
}

impl Governor {
    pub fn observe(&mut self, sample: Observation, now_ms: u64) -> Decision {
        let request = desired(sample);
        if request.tier >= self.current.tier {
            self.current = request;
            self.recovery_since = None;
        } else {
            let since = *self.recovery_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= RECOVERY_MS {
                self.current.tier = self.current.tier.recover().max(request.tier);
                if self.current.tier == request.tier {
                    self.current.reason = request.reason;
                }
                self.recovery_since = None;
            }
        }
        self.current
    }
}

fn desired(sample: Observation) -> Decision {
    use Reason::*;
    use Tier::*;
    let resources = sample.resources;
    let slow = sample.rtf.is_some_and(|rtf| rtf.is_finite() && rtf >= 0.7);
    let (tier, reason) = if let Some(emergency) = resources.emergency() {
        (Unload, emergency)
    } else if sample.recording_active && !sample.live_transcription {
        (DeferAsr, Recording)
    } else if resources.heat == Some(Heat::Moderate) {
        (DeferAsr, Thermal)
    } else if resources
        .storage_available_bytes
        .is_some_and(|amount| amount < crate::models::DISK_RESERVE_BYTES)
    {
        (DeferAsr, Storage)
    } else if sample.pending_jobs >= 3 {
        (ReducePartials, Backlog)
    } else if slow {
        (ReducePartials, SlowInference)
    } else if resources.power_saver == Some(true)
        || resources
            .battery_percent
            .is_some_and(|percent| percent <= 15)
    {
        (DeferOptional, Battery)
    } else {
        return Decision::default();
    };
    Decision {
        tier,
        reason: Some(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progressively_defers_optional_work_and_reduces_partials() {
        let mut governor = Governor::default();
        let low_battery = Observation {
            resources: ResourceSample {
                battery_percent: Some(12),
                ..ResourceSample::default()
            },
            ..Observation::default()
        };
        let optional = governor.observe(low_battery, 0);
        assert_eq!(optional.tier, Tier::DeferOptional);
        assert!(!optional.tier.admits(JobKind::Speaker));
        assert!(!optional.tier.admits(JobKind::Translation));
        assert!(optional.tier.admits(JobKind::Asr));
        let slow = governor.observe(
            Observation {
                rtf: Some(1.3),
                ..Observation::default()
            },
            1_000,
        );
        assert_eq!(slow.tier.partial_seconds(), 6);
        assert_eq!(slow.reason, Some(Reason::SlowInference));
        let recording = governor.observe(
            Observation {
                recording_active: true,
                ..Observation::default()
            },
            2_000,
        );
        assert_eq!(recording.reason, Some(Reason::Recording));
        assert!(!recording.tier.admits(JobKind::Asr));
    }

    #[test]
    fn live_asr_admits_recording_with_headroom_and_defers_optional_work_first() {
        let mut governor = Governor::default();
        let mut sample = Observation {
            recording_active: true,
            live_transcription: true,
            ..Observation::default()
        };
        assert_eq!(governor.observe(sample, 0).tier, Tier::Normal);
        sample.resources.power_saver = Some(true);
        let power = governor.observe(sample, 1);
        assert!(power.tier.admits(JobKind::Asr));
        assert!(!power.tier.admits(JobKind::Translation));
        sample.pending_jobs = 3;
        assert_eq!(governor.observe(sample, 2).tier, Tier::ReducePartials);
        sample.resources.heat = Some(Heat::Severe);
        assert_eq!(governor.observe(sample, 3).tier, Tier::Unload);
    }

    #[test]
    fn pressure_unloads_immediately_and_recovery_requires_continuous_headroom() {
        let mut governor = Governor::default();
        let memory = Observation {
            resources: ResourceSample {
                low_memory: Some(true),
                ..ResourceSample::default()
            },
            ..Observation::default()
        };
        let hot = Observation {
            resources: ResourceSample {
                heat: Some(Heat::Severe),
                ..ResourceSample::default()
            },
            ..Observation::default()
        };
        assert_eq!(governor.observe(memory, 0).tier, Tier::Unload);
        assert_eq!(
            governor.observe(Observation::default(), 1_000).tier,
            Tier::Unload
        );
        assert_eq!(
            governor.observe(Observation::default(), 9_000).tier,
            Tier::Unload
        );
        assert_eq!(governor.observe(hot, 9_500).reason, Some(Reason::Thermal));
        assert_eq!(
            governor.observe(Observation::default(), 11_000).tier,
            Tier::Unload
        );
        assert_eq!(
            governor.observe(Observation::default(), 20_999).tier,
            Tier::Unload
        );
        assert_eq!(
            governor.observe(Observation::default(), 21_000).tier,
            Tier::DeferAsr
        );
        assert_eq!(
            governor.observe(Observation::default(), 22_000).tier,
            Tier::DeferAsr
        );
        assert_eq!(
            governor.observe(Observation::default(), 32_000).tier,
            Tier::ReducePartials
        );
    }
}
