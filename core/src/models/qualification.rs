use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::contracts::{BenchmarkResult, BenchmarkStage, Count, DeviceCapabilityProfile, Platform};

use super::policy::{check_pressure, estimate, preflight};
use super::{Activity, Block, InstalledModel, Mode, RuntimeIdentity};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    SingleTrack,
    WindowsBoth,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorRates {
    pub zh_cer: f64,
    pub en_wer: f64,
    pub ja_cer: f64,
    pub mixed_token_error_rate: f64,
}

impl ErrorRates {
    fn values(self) -> [f64; 4] {
        [
            self.zh_cer,
            self.en_wer,
            self.ja_cer,
            self.mixed_token_error_rate,
        ]
    }

    fn within(self, limits: Self) -> bool {
        self.values()
            .into_iter()
            .zip(limits.values())
            .all(|(rate, limit)| {
                rate.is_finite() && rate >= 0.0 && limit.is_finite() && rate <= limit
            })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeasuredRun {
    pub result: BenchmarkResult,
    pub workload: Workload,
    pub warm_load_ms: Count,
    pub process_cpu_ms: Count,
    pub error_rates: ErrorRates,
    pub stable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BenchmarkWindow {
    pub target_seconds: u32,
    pub max_seconds: u32,
}

pub fn plan_short_benchmark(
    installed: &InstalledModel<'_>,
    device: &DeviceCapabilityProfile,
    activity: Activity,
) -> Result<BenchmarkWindow, Block> {
    activity.check()?;
    check_pressure(device)?;
    let model = installed.model();
    let runtime = RuntimeIdentity {
        id: &model.runtime.id,
        version: &model.runtime.version,
    };
    preflight(model, device, &[runtime], 0, estimate(model))?;
    Ok(BenchmarkWindow {
        target_seconds: 60,
        max_seconds: 180,
    })
}

#[derive(Clone, Copy)]
pub struct Evidence<'a> {
    pub installed: &'a InstalledModel<'a>,
    pub measured_device: &'a DeviceCapabilityProfile,
    pub run: &'a MeasuredRun,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    UserRecordingOnly,
    NoQualifiedModel,
    MeasuredRealtime,
    MeasuredPostSession,
}

#[derive(Debug, Serialize)]
pub struct RankedModel<'a> {
    pub mode: Mode,
    pub model_id: Option<&'a str>,
    pub reason: Reason,
    pub evidence: Option<&'a MeasuredRun>,
}

pub fn rank_models<'a>(
    device: &DeviceCapabilityProfile,
    workload: Workload,
    requested: Option<Mode>,
    quality_limits: ErrorRates,
    candidates: &[Evidence<'a>],
) -> RankedModel<'a> {
    let selected = if requested == Some(Mode::RecordingOnly) || check_pressure(device).is_err() {
        None
    } else {
        candidates
            .iter()
            .copied()
            .filter(|candidate| {
                requested.is_none_or(|mode| candidate.installed.model().mode == mode)
            })
            .filter(|candidate| qualified(candidate, device, workload, quality_limits))
            .min_by(|first, second| compare(first.run, second.run))
    };
    if let Some(candidate) = selected {
        let model = candidate.installed.model();
        return RankedModel {
            mode: model.mode,
            model_id: Some(&model.id),
            reason: if realtime(candidate.run) {
                Reason::MeasuredRealtime
            } else {
                Reason::MeasuredPostSession
            },
            evidence: Some(candidate.run),
        };
    }
    RankedModel {
        mode: Mode::RecordingOnly,
        model_id: None,
        reason: if requested == Some(Mode::RecordingOnly) {
            Reason::UserRecordingOnly
        } else {
            Reason::NoQualifiedModel
        },
        evidence: None,
    }
}

fn qualified(
    evidence: &Evidence<'_>,
    device: &DeviceCapabilityProfile,
    workload: Workload,
    limits: ErrorRates,
) -> bool {
    let run = evidence.run;
    let result = &run.result;
    let model = evidence.installed.model();
    let measured = evidence.measured_device;
    if (workload == Workload::WindowsBoth && device.platform != Platform::Windows)
        || run.workload != workload
        || result.device_profile_id != measured.id
        || measured.device_id != device.device_id
        || measured.platform != device.platform
        || measured.os_version != device.os_version
        || measured.abi != device.abi
        || result.observed_at_unix_ms.get() < measured.observed_at_unix_ms.get()
        || result.model.model_id != model.id
        || result.model.model_version != model.version
        || result.model.runtime_version != model.runtime.version
        || model
            .artifacts
            .first()
            .is_none_or(|file| file.sha256 != result.model.sha256)
    {
        return false;
    }
    if !run.stable
        || !run.error_rates.within(limits)
        || result.stage == BenchmarkStage::Estimate
        || result.corpus_sha256.is_none()
        || result
            .evidence_ref
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        || result.duration_ms.is_none_or(|value| value.get() == 0)
        || result
            .warm_rtf
            .is_none_or(|rtf| !rtf.is_finite() || rtf <= 0.0)
        || result.cold_start_ms.is_none()
        || result.final_p95_ms.is_none()
        || result.threads.is_none()
    {
        return false;
    }
    let Some(peak) = result
        .peak_memory_bytes
        .map(Count::get)
        .filter(|value| *value > 0)
    else {
        return false;
    };
    let runtime = RuntimeIdentity {
        id: &model.runtime.id,
        version: &model.runtime.version,
    };
    preflight(
        model,
        device,
        &[runtime],
        0,
        Some(peak.max(estimate(model).unwrap_or_default())),
    )
    .is_ok()
}

fn realtime(run: &MeasuredRun) -> bool {
    run.result.warm_rtf.is_some_and(|rtf| rtf < 1.0)
}

fn compare(first: &MeasuredRun, second: &MeasuredRun) -> Ordering {
    let a = &first.result;
    let b = &second.result;
    let cpu_share = |run: &MeasuredRun| {
        run.process_cpu_ms.get() as f64 / run.result.duration_ms.map_or(1, Count::get) as f64
    };
    let worst_rate = |run: &MeasuredRun| run.error_rates.values().into_iter().fold(0.0, f64::max);
    let stage = |result: &BenchmarkResult| match result.stage {
        BenchmarkStage::Extended => 0,
        BenchmarkStage::Sustained => 1,
        BenchmarkStage::Short => 2,
        BenchmarkStage::Estimate => 3,
    };
    a.warm_rtf
        .unwrap_or(f64::INFINITY)
        .total_cmp(&b.warm_rtf.unwrap_or(f64::INFINITY))
        .then_with(|| worst_rate(first).total_cmp(&worst_rate(second)))
        .then_with(|| a.peak_memory_bytes.cmp(&b.peak_memory_bytes))
        .then_with(|| a.final_p95_ms.cmp(&b.final_p95_ms))
        .then_with(|| a.cold_start_ms.cmp(&b.cold_start_ms))
        .then_with(|| first.warm_load_ms.cmp(&second.warm_load_ms))
        .then_with(|| cpu_share(first).total_cmp(&cpu_share(second)))
        .then_with(|| a.threads.cmp(&b.threads))
        .then_with(|| stage(a).cmp(&stage(b)))
        .then_with(|| a.model.model_id.cmp(&b.model.model_id))
}
