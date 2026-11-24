mod engine;
mod jobs;
mod pipeline;
mod transcript;
mod transfer;

#[cfg(test)]
mod tests;

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use voiceledger_audio::{CommittedReader, FinishedSession};
use voiceledger_core::contracts::{Count, DeviceCapabilityProfile, LanguagePolicy, StableId};
use voiceledger_core::device_profile;
use voiceledger_core::models::{
    Activity, Block, Catalog, InstallError, InstallRequest, Model, RuntimeIdentity, check_candidate,
};
use voiceledger_core::scheduler::{Governor, Heat, JobKind, Observation, Reason, ResourceSample};

use crate::engine::{Adapter, adapter_path};
pub use crate::pipeline::Partial;
use crate::pipeline::{Processing, Segmenter};
use crate::transcript::{Transcripts, Writer};

#[derive(Debug)]
pub enum Error {
    Invalid,
    Cancelled,
    Runtime,
    Storage,
    NoAudio,
    NotInstalled,
    Blocked(Block),
    Transfer,
    Integrity,
    Busy,
    Deferred(Reason),
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self {
        Self::Storage
    }
}

impl From<voiceledger_audio::Error> for Error {
    fn from(_: voiceledger_audio::Error) -> Self {
        Self::Storage
    }
}

impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Invalid
    }
}

impl From<libloading::Error> for Error {
    fn from(_: libloading::Error) -> Self {
        Self::Runtime
    }
}

impl From<InstallError> for Error {
    fn from(error: InstallError) -> Self {
        match error {
            InstallError::Blocked(reason) => Self::Blocked(reason),
            InstallError::Cancelled => Self::Cancelled,
            InstallError::InvalidArtifact => Self::Integrity,
            InstallError::RuntimeLoad => Self::Runtime,
            InstallError::Transfer => Self::Transfer,
            InstallError::Busy => Self::Busy,
            _ => Self::Storage,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    recordings: PathBuf,
    profiles: PathBuf,
    adapters: PathBuf,
    action: Action,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Inspect,
    Install {
        model_id: String,
        approved: bool,
    },
    Transcribe {
        model_id: String,
        session_id: StableId,
        language: LanguagePolicy,
        approved: bool,
    },
}

impl Request {
    pub fn decode(json: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(json)?)
    }

    pub fn read_only(&self) -> bool {
        matches!(self.action, Action::Inspect)
    }

    pub fn run(
        self,
        cancelled: &(dyn Fn() -> bool + Sync),
        resource_sample: &(dyn Fn() -> Option<ResourceSample> + Sync),
        mut publish_partial: impl FnMut(Option<Partial>),
    ) -> Result<Vec<u8>> {
        if [&self.recordings, &self.profiles, &self.adapters]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(Error::Invalid);
        }
        let catalog = Catalog::bundled().map_err(|_| Error::Invalid)?;
        let device = device_profile::load(&self.profiles)?;
        let reader = audio_reader(&self.recordings)?;
        let model_root = self.profiles.join("models");
        if self.read_only() {
            let session = reader
                .as_ref()
                .map(CommittedReader::latest)
                .transpose()?
                .flatten();
            let recordings = &self.recordings;
            let lines = session
                .as_ref()
                .map(|item| transcript::selected(recordings, &item.id))
                .transpose()?
                .unwrap_or_default();
            let job = session
                .as_ref()
                .map(|item| {
                    transcript::job_status(recordings, &item.id, worker_active(&self.profiles))
                })
                .transpose()?
                .flatten();
            let models: Vec<_> = catalog
                .models()
                .iter()
                .map(|model| {
                    let target = model_dir(&model_root, model);
                    let installed = target.is_dir();
                    let runtime = RuntimeIdentity {
                        id: &model.runtime.id,
                        version: &model.runtime.version,
                    };
                    let packaged = adapter_path(&model.runtime.id, &self.adapters)
                        .is_some_and(|path| path.is_file());
                    let runtimes = [runtime];
                    let reason = device
                        .as_ref()
                        .ok_or(Block::DeviceProfile)
                        .and_then(|device| {
                            check_candidate(
                                model,
                                device,
                                if packaged { &runtimes } else { &[] },
                                installed || target.with_extension("partial").is_dir(),
                            )
                        })
                        .err();
                    Choice {
                        id: &model.id,
                        installed,
                        blocked_by: reason,
                    }
                })
                .collect();
            return Ok(serde_json::to_vec(&Inspection {
                session,
                models,
                lines,
                job,
            })?);
        }
        check_cancel(cancelled)?;
        let (id, approved, transcription) = match self.action {
            Action::Inspect => return Err(Error::Invalid),
            Action::Install { model_id, approved } => (model_id, approved, None),
            Action::Transcribe {
                model_id,
                session_id,
                language,
                approved,
            } => (model_id, approved, Some((session_id, language))),
        };
        if !approved {
            return Err(Error::Blocked(Block::ApprovalRequired));
        }
        let active = reader
            .as_ref()
            .map(CommittedReader::recording)
            .transpose()?
            .unwrap_or(false);
        if active {
            return Err(Error::Blocked(Block::RecordingActive));
        }
        let model = catalog
            .models()
            .iter()
            .find(|model| model.id == id)
            .ok_or(Error::Invalid)?;
        let device = device.ok_or(Error::Blocked(Block::DeviceProfile))?;
        let exists = model_dir(&model_root, model).is_dir();
        if transcription.is_some() && !exists {
            return Err(Error::NotInstalled);
        }
        let runtime = RuntimeIdentity {
            id: &model.runtime.id,
            version: &model.runtime.version,
        };
        check_candidate(model, &device, &[runtime], true).map_err(Error::Blocked)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.profiles.join(WORKER_LOCK))?;
        lock.try_lock().map_err(|_| Error::Busy)?;
        let threads = device
            .logical_cpu_count
            .map_or(1, |value| value.get().min(2) as i32);
        let request = InstallRequest {
            root: &model_root,
            device: &device,
            activity: Activity {
                approved,
                recording_active: active,
            },
        };
        let initial = initial_resources(&device);
        let observe = || resource_sample().unwrap_or_default().with_fallback(initial);
        let interrupted = || cancelled() || observe().emergency().is_some();
        let stop_loading = || {
            interrupted()
                || reader
                    .as_ref()
                    .is_some_and(|reader| reader.recording().unwrap_or(true))
        };
        let Some((session, language)) = transcription else {
            let adapter = Adapter::load(model, &self.adapters, threads)?;
            let agent = transfer::agent();
            voiceledger_core::models::install(
                model,
                request,
                &adapter,
                |artifact, offset| transfer::fetch(&agent, artifact, offset),
                stop_loading,
            )
            .map_err(|error| deferred(error.into(), cancelled(), observe(), reader.as_ref()))?;
            return Ok(Vec::new());
        };
        let reader = reader.as_ref().ok_or(Error::NoAudio)?;
        let tracks = reader.inputs(&session)?;
        if tracks.is_empty() {
            return Err(Error::NoAudio);
        }
        let transcripts = Transcripts::open(&self.recordings)?;
        let queue = transcripts.queue();
        queue.recover()?;
        let spec = jobs::SourceJob::new(&tracks, model, language);
        let vad = queue.enqueue(transcripts.id()?, &session, JobKind::Vad, &spec)?;
        let asr = queue.enqueue(transcripts.id()?, &session, JobKind::Asr, &spec)?;
        let ids = [vad.as_str(), asr.as_str()];
        if !queue.begin(&ids)? {
            return processed(0);
        }

        let result = (|| {
            let cursor = queue.cursor(&asr)?;
            let mut observation = Observation {
                pending_jobs: queue.backlog()?,
                ..Observation::default()
            };
            let mut governor = Governor::default();
            let started = Instant::now();
            let mut policy = |rtf: Option<f64>| {
                observation.resources = observe();
                observation.recording_active = reader.recording()?;
                observation.rtf = rtf.or(observation.rtf);
                let decision = governor.observe(observation, started.elapsed().as_millis() as u64);
                if decision.tier.admits(JobKind::Asr) {
                    Ok(decision.tier)
                } else {
                    Err(Error::Deferred(decision.reason.unwrap_or(Reason::Memory)))
                }
            };
            policy(None)?;
            let adapter = Adapter::load(model, &self.adapters, threads)?;
            let installed = voiceledger_core::models::install(
                model,
                request,
                &adapter,
                |_, _| {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Model not installed.",
                    ))
                },
                stop_loading,
            )?;
            check_cancel(&interrupted)?;
            policy(None)?;
            let mut engine = adapter.engine(model, installed.directory(), language)?;
            let mut writer = Writer {
                transcripts: &transcripts,
                session: &session,
                model,
                language,
                job: Some(&asr),
                publish_partial: &mut publish_partial,
                saved: 0,
            };
            let mut processor = Processing {
                engine: &mut engine,
                output: &mut writer,
                language,
                cancelled: &interrupted,
                partials: true,
                policy: &mut policy,
            };
            for track in &tracks {
                let mut from_ns = match &cursor {
                    Some(saved) if track.track_id < saved.track => continue,
                    Some(saved) if track.track_id == saved.track => saved.next_ns,
                    _ => track.start_ns,
                };
                let mut segments = Segmenter::new();
                loop {
                    (processor.policy)(None)?;
                    check_cancel(&interrupted)?;
                    let Some(chunk) = reader.input_chunk(&session, track, from_ns)? else {
                        break;
                    };
                    let end = chunk.end_ns;
                    segments.ingest(chunk, from_ns, &mut processor)?;
                    from_ns = end;
                }
                segments.flush(&mut processor)?;
                queue.checkpoint(&asr, &track.track_id, track.end_ns)?;
            }
            Ok(writer.saved)
        })()
        .map_err(|error| deferred(error, cancelled(), observe(), Some(reader)));
        use jobs::Outcome;
        let outcome = match &result {
            Ok(_) => Outcome::Completed,
            Err(Error::Cancelled) => Outcome::Deferred("user_cancelled"),
            Err(Error::Deferred(reason)) => Outcome::Deferred(reason.code()),
            Err(Error::Blocked(_)) => Outcome::Deferred("resource_pressure"),
            Err(Error::Runtime | Error::Integrity) => Outcome::Failed("runtime_unavailable"),
            Err(_) => Outcome::Failed("job_failed"),
        };
        queue.finish(&ids, outcome)?;
        processed(result?)
    }
}

pub fn encode_partial(value: Partial) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&value)?)
}

#[derive(Serialize)]
struct Choice<'a> {
    id: &'a str,
    installed: bool,
    blocked_by: Option<Block>,
}

#[derive(Serialize)]
struct Inspection<'a> {
    session: Option<FinishedSession>,
    models: Vec<Choice<'a>>,
    lines: Vec<transcript::Line>,
    job: Option<jobs::Status>,
}

const WORKER_LOCK: &str = "speech-worker.lock";

fn worker_active(profiles: &Path) -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(profiles.join(WORKER_LOCK))
        .is_ok_and(|lock| lock.try_lock_shared().is_err())
}

fn initial_resources(device: &DeviceCapabilityProfile) -> ResourceSample {
    let heat = match device.thermal_state.as_deref() {
        Some("severe" | "critical" | "emergency" | "shutdown") => Some(Heat::Severe),
        Some("moderate") => Some(Heat::Moderate),
        Some("none" | "light") => Some(Heat::Normal),
        _ => None,
    };
    ResourceSample {
        low_memory: device.low_memory,
        available_memory_bytes: device.available_memory_bytes.map(Count::get),
        low_memory_threshold_bytes: device.low_memory_threshold_bytes.map(Count::get),
        storage_available_bytes: device.storage_available_bytes.map(Count::get),
        heat,
        power_saver: device.power_saver,
        battery_percent: device.battery_percent,
    }
}

fn deferred(
    error: Error,
    cancelled: bool,
    resources: ResourceSample,
    reader: Option<&CommittedReader>,
) -> Error {
    if !matches!(error, Error::Cancelled) || cancelled {
        return error;
    }
    if let Some(reason) = resources.emergency() {
        return Error::Deferred(reason);
    }
    if reader.is_some_and(|reader| reader.recording().unwrap_or(false)) {
        return Error::Deferred(Reason::Recording);
    }
    error
}

fn processed(saved: u64) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::json!({ "saved": saved }))?)
}

fn audio_reader(root: &Path) -> Result<Option<CommittedReader>> {
    if root.join("manifest.sqlite3").try_exists()? {
        return Ok(Some(CommittedReader::open(root)?));
    }
    Ok(None)
}

fn model_dir(root: &Path, model: &Model) -> PathBuf {
    root.join(format!("{}-{}", model.id, model.version))
}

pub(crate) fn check_cancel(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
