use serde::Serialize;

use crate::contracts::{Count, DeviceCapabilityProfile, Platform};
use crate::{Compatibility, Recommendation, RecommendationInput, recommend};

use super::{Mode, Model};

const DISK_RESERVE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Block {
    Catalog,
    DeviceProfile,
    Abi,
    OsUnknown,
    OsVersion,
    CpuFeatures,
    StorageUnknown,
    StorageSpace,
    RuntimeUnavailable,
    MemoryUnknown,
    MemoryBudget,
    MemoryPressure,
    ThermalPressure,
    BatteryPressure,
    ApprovalRequired,
    RecordingActive,
}

impl Block {
    fn compatibility(self) -> Compatibility {
        match self {
            Self::Abi
            | Self::OsVersion
            | Self::CpuFeatures
            | Self::StorageSpace
            | Self::MemoryBudget => Compatibility::Incompatible,
            _ => Compatibility::Unknown,
        }
    }
}

#[derive(Clone, Copy)]
pub struct RuntimeIdentity<'a> {
    pub id: &'a str,
    pub version: &'a str,
}

#[derive(Clone, Copy)]
pub struct Activity {
    pub approved: bool,
    pub recording_active: bool,
}

impl Activity {
    pub(super) fn check(self) -> Result<(), Block> {
        if self.recording_active {
            return Err(Block::RecordingActive);
        }
        if !self.approved {
            return Err(Block::ApprovalRequired);
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct Candidate<'a> {
    pub model: &'a Model,
    pub download_bytes: u64,
    pub compatibility: &'static str,
    pub blocked_by: Option<Block>,
}

#[derive(Debug, Serialize)]
pub struct ModelOptions<'a> {
    pub mode: Mode,
    pub candidates: Vec<Candidate<'a>>,
}

pub fn model_options<'a>(
    models: &'a [Model],
    device: Option<&DeviceCapabilityProfile>,
    runtimes: &[RuntimeIdentity<'_>],
) -> ModelOptions<'a> {
    let candidates = models
        .iter()
        .map(|model| {
            let download_bytes = model.download_bytes();
            let blocked_by = device
                .ok_or(Block::DeviceProfile)
                .and_then(|device| {
                    preflight(model, device, runtimes, download_bytes, estimate(model))
                })
                .err();
            let compatibility = match blocked_by.map(Block::compatibility) {
                None | Some(Compatibility::Compatible) => "compatible",
                Some(Compatibility::Incompatible) => "incompatible",
                Some(Compatibility::Unknown) => "unknown",
            };
            Candidate {
                model,
                download_bytes,
                compatibility,
                blocked_by,
            }
        })
        .collect();
    ModelOptions {
        mode: Mode::RecordingOnly,
        candidates,
    }
}

pub(super) fn estimate(model: &Model) -> Option<u64> {
    model.peak_ram_estimate.as_ref().map(|value| value.bytes)
}

pub(super) fn preflight(
    model: &Model,
    device: &DeviceCapabilityProfile,
    runtimes: &[RuntimeIdentity<'_>],
    download_bytes: u64,
    peak: Option<u64>,
) -> Result<(), Block> {
    if !model.valid() {
        return Err(Block::Catalog);
    }
    let target = model
        .targets
        .iter()
        .find(|target| target.platform == device.platform && target.abi == device.abi)
        .ok_or(Block::Abi)?;
    let os = match device.platform {
        Platform::Windows => device.os_version.split('.').next().unwrap_or_default(),
        Platform::Android => &device.os_version,
    };
    if os.parse::<u32>().map_err(|_| Block::OsUnknown)? < target.min_os {
        return Err(Block::OsVersion);
    }
    if target
        .cpu_features
        .iter()
        .any(|feature| !device.cpu_features.contains(feature))
    {
        return Err(Block::CpuFeatures);
    }
    let disk = device
        .storage_available_bytes
        .ok_or(Block::StorageUnknown)?
        .get();
    if disk.saturating_sub(DISK_RESERVE) < download_bytes || disk < DISK_RESERVE {
        return Err(Block::StorageSpace);
    }
    if !runtimes
        .iter()
        .any(|item| item.id == model.runtime.id && item.version == model.runtime.version)
    {
        return Err(Block::RuntimeUnavailable);
    }
    let budget = memory_budget(device).ok_or(Block::MemoryUnknown)?;
    let peak = peak.ok_or(Block::MemoryUnknown)?;
    if recommend(RecommendationInput {
        compatibility: Compatibility::Compatible,
        memory_budget_bytes: Some(budget),
        peak_memory_bytes: Some(peak),
    }) != Recommendation::CandidateForBenchmark
    {
        return Err(Block::MemoryBudget);
    }
    Ok(())
}

fn memory_budget(device: &DeviceCapabilityProfile) -> Option<u64> {
    let available = device.available_memory_bytes?.get();
    let capture = device.process_memory_bytes?.get();
    let os_reserve = device.low_memory_threshold_bytes.map_or(0, Count::get);
    let after_reserves = available.saturating_sub(capture).saturating_sub(os_reserve);
    Some(after_reserves / 10 * 3)
}

pub(super) fn check_pressure(device: &DeviceCapabilityProfile) -> Result<(), Block> {
    if device.low_memory == Some(true) {
        return Err(Block::MemoryPressure);
    }
    if matches!(
        device.thermal_state.as_deref(),
        Some("moderate" | "severe" | "critical" | "emergency" | "shutdown")
    ) {
        return Err(Block::ThermalPressure);
    }
    if device.power_saver == Some(true)
        || device.battery_percent.is_some_and(|battery| battery <= 15)
    {
        return Err(Block::BatteryPressure);
    }
    Ok(())
}
