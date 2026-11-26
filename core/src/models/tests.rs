use std::cell::Cell;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::contracts::{BenchmarkStage, DeviceCapabilityProfile, Platform};
use crate::scheduler::ResourceSample;

use super::*;

const FIRST_ID: &str = "e73ae205-064b-4f33-bc62-72b4a19a497e";
const DEVICE_ID: &str = "c6e29520-4184-4dcf-9001-a284af242ec0";
const RUN_ID: &str = "259479ac-2770-4ef2-bcae-6e31bb76dd82";
const SAFE: Activity = Activity {
    approved: true,
    recording_active: false,
};
const RUNTIME: RuntimeIdentity<'_> = RuntimeIdentity {
    id: "whisper.cpp",
    version: "1.8.2",
};
const QUALITY_LIMITS: ErrorRates = ErrorRates {
    zh_cer: 0.25,
    en_wer: 0.25,
    ja_cer: 0.25,
    mixed_token_error_rate: 0.25,
};

fn device() -> DeviceCapabilityProfile {
    serde_json::from_value(json!({
        "id": FIRST_ID, "device_id": DEVICE_ID, "observed_at_unix_ms": 1,
        "platform": "windows", "os_version": "10.0.26100", "abi": "x86_64",
        "cpu_features": ["sse2"], "logical_cpu_count": 8,
        "process_memory_bytes": 100_000_000, "available_memory_bytes": 4_000_000_000u64,
        "storage_available_bytes": 2_000_000_000u64, "low_memory": false
    }))
    .unwrap()
}

fn model() -> Model {
    let mut model = Catalog::bundled().unwrap().models()[0].clone();
    model.artifacts[0].size_bytes = 3;
    model.artifacts[0].sha256 = format!("{:x}", Sha256::digest(b"abc")).try_into().unwrap();
    model
}

struct TempRoot(PathBuf);
impl TempRoot {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("voiceledger-models-{}-{id}", std::process::id()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn request<'a>(
        &'a self,
        device: &'a DeviceCapabilityProfile,
        activity: Activity,
    ) -> InstallRequest<'a> {
        InstallRequest {
            root: &self.0,
            device,
            activity,
            resources: None,
        }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Probe {
    loads: Cell<u32>,
    accepts: bool,
}

impl RuntimeProbe for Probe {
    fn identity(&self) -> RuntimeIdentity<'_> {
        RUNTIME
    }

    fn load_and_release(&self, _: &Model, _: &Path, _: &dyn Fn() -> bool) -> bool {
        self.loads.set(self.loads.get() + 1);
        self.accepts
    }
}

fn fetch_body(start: u64, bytes: &[u8]) -> Download {
    Download {
        start,
        body: Box::new(Cursor::new(bytes.to_vec())),
    }
}

#[test]
fn published_candidates_use_their_own_download_and_memory_metadata() {
    let catalog = Catalog::bundled().unwrap();
    let [whisper, sensevoice, translation, gguf, q6, nemotron] = catalog.models() else {
        panic!("Expected five transcription candidates and one translation candidate");
    };
    assert_eq!(whisper.download_bytes(), 32_152_673);
    assert_eq!(
        whisper.peak_ram_estimate.as_ref().unwrap().bytes,
        273_000_000
    );
    assert_eq!(sensevoice.download_bytes(), 239_549_735);
    assert_eq!(
        sensevoice.peak_ram_estimate.as_ref().unwrap().bytes,
        1_100_000_000
    );
    assert_eq!(sensevoice.license.id, "LicenseRef-FunASR-Model");
    assert_eq!(gguf.download_bytes(), 252_684_608);
    assert_eq!(gguf.runtime.id, "transcribe.cpp");
    assert_eq!(gguf.targets.len(), 1);
    assert_eq!(gguf.targets[0].platform, Platform::Android);
    assert_eq!(q6.download_bytes(), 196_438_336);
    assert_eq!(nemotron.download_bytes(), 495_831_520);
    assert_eq!(nemotron.runtime.id, gguf.runtime.id);
    assert_eq!(nemotron.runtime.version, gguf.runtime.version);
    assert_eq!(nemotron.targets.len(), 1);
    assert_eq!(nemotron.targets[0].platform, Platform::Android);
    let pair = translation.translation.unwrap();
    assert_eq!(pair.source.code(), "en");
    assert_eq!(pair.target.code(), "zh");
    assert_eq!(translation.download_bytes(), 82_482_780);

    let mut altered = json!({"schema_version": 1, "models": [whisper]});
    altered["models"][0]["artifacts"][0]["file"] = json!("../weights");
    assert_eq!(
        Catalog::decode(&serde_json::to_vec(&altered).unwrap()).unwrap_err(),
        CatalogError::InvalidCatalog
    );
}

#[test]
fn candidate_retains_hard_gates_and_the_recording_only_default() {
    let catalog = Catalog::bundled().unwrap();
    let mut current = device();
    let models = catalog.models();
    let supported = model_options(models, Some(&current), &[RUNTIME]);
    assert_eq!(supported.mode, Mode::RecordingOnly);
    assert_eq!(supported.candidates[0].compatibility, "compatible");
    assert_eq!(
        model_options(models, Some(&current), &[]).candidates[0].blocked_by,
        Some(Block::RuntimeUnavailable)
    );
    current.cpu_features.clear();
    assert_eq!(
        model_options(models, Some(&current), &[RUNTIME]).candidates[0].blocked_by,
        Some(Block::CpuFeatures)
    );
    current = device();
    current.available_memory_bytes = Some(1_000_000i64.try_into().unwrap());
    assert_eq!(
        model_options(models, Some(&current), &[RUNTIME]).candidates[0].blocked_by,
        Some(Block::MemoryBudget)
    );
    current.available_memory_bytes = None;
    assert_eq!(
        model_options(models, Some(&current), &[RUNTIME]).candidates[0].blocked_by,
        Some(Block::MemoryUnknown)
    );
    current.storage_available_bytes = Some(1_000_000i64.try_into().unwrap());
    assert_eq!(
        model_options(models, Some(&current), &[RUNTIME]).candidates[0].blocked_by,
        Some(Block::StorageSpace)
    );
}

#[test]
fn android_sensevoice_keeps_two_estimated_peaks_after_reserves() {
    let catalog = Catalog::bundled().unwrap();
    let model = &catalog.models()[1];
    let runtime = RuntimeIdentity {
        id: &model.runtime.id,
        version: &model.runtime.version,
    };
    let mut profile = device();
    profile.platform = Platform::Android;
    profile.os_version = "35".into();
    profile.abi = "arm64-v8a".into();
    profile.cpu_features = vec!["neon".into()];
    profile.low_ram_device = Some(false);
    profile.low_memory_threshold_bytes = Some(300_000_000i64.try_into().unwrap());
    profile.available_memory_bytes = Some(2_600_000_000i64.try_into().unwrap());
    assert_eq!(check_candidate(model, &profile, &[runtime], false), Ok(()));

    let sample = ResourceSample {
        low_memory: Some(false),
        available_memory_bytes: Some(2_599_999_999),
        low_memory_threshold_bytes: Some(300_000_000),
        ..ResourceSample::default()
    };
    assert_eq!(
        check_model_memory(model, &profile, sample),
        Err(Block::MemoryBudget)
    );
    profile.low_ram_device = Some(true);
    assert_eq!(
        check_candidate(model, &profile, &[runtime], false),
        Err(Block::MemoryBudget)
    );
    profile.low_ram_device = Some(false);
    profile.low_memory = Some(true);
    assert_eq!(
        check_candidate(model, &profile, &[runtime], false),
        Err(Block::MemoryPressure)
    );
    let mut windows = device();
    windows.available_memory_bytes = Some(2_600_000_000i64.try_into().unwrap());
    assert_eq!(
        check_candidate(model, &windows, &[runtime], false),
        Err(Block::MemoryBudget)
    );

    let gguf = &catalog.models()[3];
    let runtimes = [RuntimeIdentity {
        id: &gguf.runtime.id,
        version: &gguf.runtime.version,
    }];
    profile.low_memory = Some(false);
    profile.available_memory_bytes = Some(1_700_000_000i64.try_into().unwrap());
    assert_eq!(check_candidate(gguf, &profile, &runtimes, false), Ok(()));
    profile.available_memory_bytes = Some(1_699_999_999i64.try_into().unwrap());
    assert_eq!(
        check_candidate(gguf, &profile, &runtimes, false),
        Err(Block::MemoryBudget)
    );
}

#[test]
fn installer_rechecks_memory_before_probing_downloaded_weights() {
    let root = TempRoot::new();
    let model = model();
    let device = device();
    let probe = Probe {
        loads: Cell::new(0),
        accepts: true,
    };
    let resources = || ResourceSample {
        available_memory_bytes: Some(800_000_000),
        ..ResourceSample::default()
    };
    let result = install(
        &model,
        InstallRequest {
            resources: Some(&resources),
            ..root.request(&device, SAFE)
        },
        &probe,
        |_, offset| Ok(fetch_body(offset, b"abc")),
        || false,
    );
    assert!(matches!(
        result,
        Err(InstallError::Blocked(Block::MemoryBudget))
    ));
    assert_eq!(probe.loads.get(), 0);
    let installed = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, _| panic!("verified staged download must be reused"),
        || false,
    )
    .unwrap();
    assert!(installed.directory().is_dir());
    assert_eq!(probe.loads.get(), 1);
}

#[test]
fn download_resumes_then_runs_probe_before_atomic_publication() {
    let root = TempRoot::new();
    let model = model();
    let device = device();
    let probe = Probe {
        loads: Cell::new(0),
        accepts: true,
    };
    let attempt = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, offset| {
            assert_eq!(offset, 0);
            Ok(fetch_body(0, b"a"))
        },
        || false,
    );
    assert!(matches!(attempt, Err(InstallError::Transfer)));
    assert_eq!(probe.loads.get(), 0);
    let installed = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, offset| {
            assert_eq!(offset, 1);
            Ok(fetch_body(1, b"bc"))
        },
        || false,
    )
    .unwrap();
    assert_eq!(probe.loads.get(), 1);
    assert_eq!(
        fs::read(installed.directory().join(&model.artifacts[0].file)).unwrap(),
        b"abc"
    );
    fs::write(installed.directory().join(&model.artifacts[0].file), b"BAD").unwrap();
    let again = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, _| panic!("unexpected download"),
        || false,
    );
    assert!(matches!(again, Err(InstallError::InvalidArtifact)));
    assert_eq!(probe.loads.get(), 1);
}

#[test]
fn refuses_unapproved_cancelled_corrupt_and_unloadable_downloads() {
    let root = TempRoot::new();
    let model = model();
    let device = device();
    let probe = Probe {
        loads: Cell::new(0),
        accepts: false,
    };
    let unapproved = install(
        &model,
        root.request(
            &device,
            Activity {
                approved: false,
                ..SAFE
            },
        ),
        &probe,
        |_, _| panic!("unexpected download"),
        || false,
    );
    assert!(matches!(
        unapproved,
        Err(InstallError::Blocked(Block::ApprovalRequired))
    ));
    let cancelled = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, _| panic!("unexpected download"),
        || true,
    );
    assert!(matches!(cancelled, Err(InstallError::Cancelled)));
    let corrupt = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, offset| {
            assert_eq!(offset, 0);
            Ok(fetch_body(0, b"bad"))
        },
        || false,
    );
    assert!(matches!(corrupt, Err(InstallError::InvalidArtifact)));
    assert_eq!(probe.loads.get(), 0);
    let unloadable = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, offset| {
            assert_eq!(offset, 0);
            Ok(fetch_body(0, b"abc"))
        },
        || false,
    );
    assert!(matches!(unloadable, Err(InstallError::RuntimeLoad)));
    assert_eq!(probe.loads.get(), 1);
    assert!(
        !root
            .0
            .join(format!("{}-{}", model.id, model.version))
            .exists()
    );
}

#[test]
fn benchmarks_require_idle_explicit_approval_and_resource_headroom() {
    let root = TempRoot::new();
    let model = model();
    let mut device = device();
    let probe = Probe {
        loads: Cell::new(0),
        accepts: true,
    };
    let installed = install(
        &model,
        root.request(&device, SAFE),
        &probe,
        |_, _| Ok(fetch_body(0, b"abc")),
        || false,
    )
    .unwrap();
    assert_eq!(
        plan_short_benchmark(&installed, &device, SAFE)
            .unwrap()
            .max_seconds,
        180
    );
    assert_eq!(
        plan_short_benchmark(
            &installed,
            &device,
            Activity {
                recording_active: true,
                ..SAFE
            }
        ),
        Err(Block::RecordingActive)
    );
    assert_eq!(
        plan_short_benchmark(
            &installed,
            &device,
            Activity {
                approved: false,
                ..SAFE
            }
        ),
        Err(Block::ApprovalRequired)
    );
    device.low_memory = Some(true);
    assert_eq!(
        plan_short_benchmark(&installed, &device, SAFE),
        Err(Block::MemoryPressure)
    );
    device.low_memory = Some(false);
    device.thermal_state = Some("severe".into());
    assert_eq!(
        plan_short_benchmark(&installed, &device, SAFE),
        Err(Block::ThermalPressure)
    );
    device.thermal_state = Some("none".into());
    device.battery_percent = Some(14);
    assert_eq!(
        plan_short_benchmark(&installed, &device, SAFE),
        Err(Block::BatteryPressure)
    );
}

fn run(model: &Model) -> MeasuredRun {
    serde_json::from_value(json!({
        "result": {
            "id": RUN_ID, "device_profile_id": FIRST_ID,
            "model": {"model_id": model.id, "model_version": model.version,
                      "runtime_version": model.runtime.version, "sha256": model.artifacts[0].sha256},
            "stage": "short", "observed_at_unix_ms": 5,
            "corpus_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "duration_ms": 60000, "cold_start_ms": 1000, "warm_rtf": 0.82,
            "peak_memory_bytes": 300000000, "final_p95_ms": 2400, "threads": 2,
            "evidence_ref": "synthetic-run"
        },
        "workload": "windows_both", "warm_load_ms": 400, "process_cpu_ms": 72000, "stable": true,
        "error_rates": {"zh_cer": 0.1, "en_wer": 0.16, "ja_cer": 0.18, "mixed_token_error_rate": 0.22}
    })).unwrap()
}

#[test]
fn recommendation_requires_target_workload_safety_and_separate_language_quality() {
    let root = TempRoot::new();
    let model = model();
    let measured = device();
    let installed = install(
        &model,
        root.request(&measured, SAFE),
        &Probe {
            loads: Cell::new(0),
            accepts: true,
        },
        |_, _| Ok(fetch_body(0, b"abc")),
        || false,
    )
    .unwrap();
    let evidence = |run: &MeasuredRun, workload, current: &DeviceCapabilityProfile, requested| {
        rank_models(
            current,
            workload,
            requested,
            QUALITY_LIMITS,
            &[Evidence {
                installed: &installed,
                measured_device: &measured,
                run,
            }],
        )
        .reason
    };
    let mut result = run(&model);
    assert_eq!(
        evidence(
            &result,
            Workload::WindowsBoth,
            &measured,
            Some(Mode::Lightweight)
        ),
        Reason::MeasuredRealtime
    );
    assert_eq!(
        evidence(&result, Workload::SingleTrack, &measured, None),
        Reason::NoQualifiedModel
    );
    result.result.warm_rtf = Some(1.0);
    assert_eq!(
        evidence(&result, Workload::WindowsBoth, &measured, None),
        Reason::MeasuredPostSession
    );
    let mut constrained = device();
    constrained.available_memory_bytes = Some(500_000_000i64.try_into().unwrap());
    assert_eq!(
        evidence(
            &result,
            Workload::WindowsBoth,
            &constrained,
            Some(Mode::Lightweight)
        ),
        Reason::NoQualifiedModel
    );
    result.error_rates.ja_cer = 0.50;
    assert_eq!(
        evidence(&result, Workload::WindowsBoth, &measured, None),
        Reason::NoQualifiedModel
    );
    result.error_rates.ja_cer = 0.18;
    result.result.stage = BenchmarkStage::Estimate;
    assert_eq!(
        evidence(&result, Workload::WindowsBoth, &measured, None),
        Reason::NoQualifiedModel
    );
}
