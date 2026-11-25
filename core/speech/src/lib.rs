mod engine;
mod jobs;
mod library;
mod live;
mod pipeline;
mod sync_data;
mod sync_request;
mod transcript;
mod transfer;
mod translation_engine;
mod translations;

#[cfg(test)]
mod tests;

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use voiceledger_audio::{CommittedReader, FinishedSession, HistoryTrack};
use voiceledger_core::contracts::{
    Count, DeviceCapabilityProfile, Language, LanguagePolicy, ModelRef, StableId,
};
use voiceledger_core::device_profile;
use voiceledger_core::models::{
    Activity, Block, Catalog, InstallError, InstallRequest, Model, RuntimeIdentity,
    check_candidate, current_memory_budget,
};
use voiceledger_core::scheduler::{Governor, Heat, JobKind, Observation, Reason, ResourceSample};

use crate::engine::{Adapter, adapter_path};
pub use crate::pipeline::Partial;
use crate::pipeline::{Processing, Segmenter};
pub use crate::sync_request::request as sync_request;
use crate::transcript::{Transcripts, Writer};
use crate::translation_engine::TranslationAdapter;

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
    RevisionConflict,
    TranslationTextLimit,
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
    LiveModel,
    LiveTranscript {
        session_id: StableId,
    },
    TranscribeLive {
        session_id: StableId,
        language: LanguagePolicy,
        translate: bool,
        approved: bool,
    },
    Inspect {
        session_id: Option<StableId>,
    },
    History {
        query: Option<String>,
    },
    Export {
        session_id: StableId,
        format: library::ExportFormat,
    },
    TrackTimeline {
        session_id: StableId,
        track_id: StableId,
    },
    Playback {
        session_id: StableId,
        track_id: StableId,
        position_ns: i64,
    },
    TrackArchive {
        session_id: StableId,
        track_id: StableId,
    },
    TrackArchiveChunk {
        session_id: StableId,
        track_id: StableId,
        chunk_id: StableId,
    },
    SaveUtterance(library::UtteranceEdit),
    ResolveConflict {
        session_id: StableId,
        utterance_id: StableId,
        expected_heads: Vec<StableId>,
        text: String,
    },
    RenameSession(library::TitleEdit),
    SaveSpeaker(library::SpeakerEdit),
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
    Translate {
        model_id: String,
        session_id: StableId,
        approved: bool,
    },
    SaveTranslation(translations::ManualEdit),
}

impl Request {
    pub fn decode(json: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(json)?)
    }

    pub fn read_only(&self) -> bool {
        matches!(
            self.action,
            Action::LiveModel
                | Action::LiveTranscript { .. }
                | Action::Inspect { .. }
                | Action::History { .. }
                | Action::Export { .. }
                | Action::TrackTimeline { .. }
                | Action::Playback { .. }
                | Action::TrackArchive { .. }
                | Action::TrackArchiveChunk { .. }
        )
    }

    pub fn run(
        self,
        cancelled: &(dyn Fn() -> bool + Sync),
        resource_sample: &(dyn Fn() -> Option<ResourceSample> + Sync),
        mut publish: impl FnMut(Option<Update>),
    ) -> Result<Vec<u8>> {
        if [&self.recordings, &self.profiles, &self.adapters]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(Error::Invalid);
        }
        if matches!(self.action, Action::LiveModel) {
            return live::model_status(&self.profiles, &self.adapters);
        }
        let reader = audio_reader(&self.recordings)?;
        match &self.action {
            Action::LiveTranscript { session_id } => {
                return live::transcript(
                    &self.recordings,
                    session_id,
                    worker_active(&self.profiles),
                );
            }
            Action::TrackTimeline {
                session_id,
                track_id,
            } => {
                let rows = reader
                    .as_ref()
                    .ok_or(Error::NoAudio)?
                    .timeline(session_id, track_id)
                    .map_err(playback_error)?;
                return Ok(serde_json::to_vec(&rows)?);
            }
            Action::Playback {
                session_id,
                track_id,
                position_ns,
            } => {
                return reader
                    .as_ref()
                    .ok_or(Error::NoAudio)?
                    .playback(session_id, track_id, *position_ns)
                    .map_err(playback_error)?
                    .into_bytes()
                    .ok_or(Error::NoAudio);
            }
            Action::TrackArchive {
                session_id,
                track_id,
            } => {
                let index = reader
                    .as_ref()
                    .ok_or(Error::NoAudio)?
                    .archive(session_id, track_id)
                    .map_err(playback_error)?;
                return Ok(serde_json::to_vec(&index)?);
            }
            Action::TrackArchiveChunk {
                session_id,
                track_id,
                chunk_id,
            } => {
                return reader
                    .as_ref()
                    .ok_or(Error::NoAudio)?
                    .archive_chunk(session_id, track_id, chunk_id)
                    .map_err(playback_error);
            }
            _ => {}
        }
        let catalog = Catalog::bundled().map_err(|_| Error::Invalid)?;
        let device = device_profile::load(&self.profiles)?;
        let model_root = self.profiles.join("models");
        if let Action::Export {
            ref session_id,
            format,
        } = self.action
        {
            let session = sync_data::finished(&self.recordings, reader.as_ref(), session_id)?
                .ok_or(Error::NoAudio)?;
            return library::export(&self.recordings, &session, format);
        }
        if let Action::History { ref query } = self.action {
            return Ok(serde_json::to_vec(&library::history(
                &self.recordings,
                reader.as_ref(),
                query.as_deref(),
            )?)?);
        }
        if let Action::Inspect { ref session_id } = self.action {
            let recordings = &self.recordings;
            let local = reader
                .as_ref()
                .map(|reader| match session_id {
                    Some(id) => reader.finished(id),
                    None => reader.latest(),
                })
                .transpose()?
                .flatten();
            let session = match session_id
                .as_ref()
                .or_else(|| local.as_ref().map(|item| &item.id))
            {
                Some(id) => sync_data::finished(recordings, reader.as_ref(), id)?,
                None => None,
            };
            let text_only = local.is_none() && session.is_some();
            let title = session
                .as_ref()
                .map(|session| library::title(recordings, &session.id))
                .transpose()?
                .flatten();
            let tracks = match (reader.as_ref(), session_id) {
                (Some(reader), Some(id)) => reader.tracks(id)?,
                _ => Vec::new(),
            };
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
            let has_translation_source = session
                .as_ref()
                .map(|item| transcript::has_translation_source(recordings, &item.id))
                .transpose()?
                .unwrap_or(false);
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
                has_translation_source,
                tracks,
                title,
                text_only,
            })?);
        }
        check_cancel(cancelled)?;
        if matches!(
            self.action,
            Action::TranscribeLive {
                approved: false,
                ..
            } | Action::Install {
                approved: false,
                ..
            } | Action::Transcribe {
                approved: false,
                ..
            } | Action::Translate {
                approved: false,
                ..
            }
        ) {
            return Err(Error::Blocked(Block::ApprovalRequired));
        }
        let active = reader
            .as_ref()
            .map(CommittedReader::recording)
            .transpose()?
            .unwrap_or(false);
        if active && !matches!(self.action, Action::TranscribeLive { .. }) {
            return Err(Error::Blocked(Block::RecordingActive));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.profiles.join(WORKER_LOCK))?;
        lock.try_lock().map_err(|_| Error::Busy)?;
        let action = match self.action {
            Action::TranscribeLive {
                session_id,
                language,
                translate,
                ..
            } => {
                let reader = reader.as_ref().ok_or(Error::NoAudio)?;
                let device = device
                    .as_ref()
                    .ok_or(Error::Blocked(Block::DeviceProfile))?;
                let model = catalog
                    .models()
                    .iter()
                    .find(|model| model.id == live::ASR_MODEL)
                    .ok_or(Error::Invalid)?;
                let translation = if translate {
                    Some(
                        catalog
                            .models()
                            .iter()
                            .find(|model| model.id == live::TRANSLATION_MODEL)
                            .ok_or(Error::Invalid)?,
                    )
                } else {
                    None
                };
                let initial = initial_resources(device);
                return processed(
                    live::Task {
                        reader,
                        store: &Transcripts::open(&self.recordings)?,
                        session: &session_id,
                        model,
                        translation,
                        language,
                        adapters: &self.adapters,
                        model_root: &model_root,
                        device,
                        allow_active: true,
                    }
                    .run(
                        cancelled,
                        &|| resource_sample().unwrap_or_default().with_fallback(initial),
                        &mut publish,
                    )?,
                );
            }
            Action::SaveUtterance(edit) => {
                Transcripts::open(&self.recordings)?.save_utterance(edit)?;
                return processed(1);
            }
            Action::ResolveConflict {
                session_id,
                utterance_id,
                expected_heads,
                text,
            } => {
                Transcripts::open(&self.recordings)?.resolve_conflict(
                    &session_id,
                    &utterance_id,
                    &expected_heads,
                    &text,
                )?;
                return processed(1);
            }
            Action::RenameSession(edit) => {
                sync_data::finished(&self.recordings, reader.as_ref(), &edit.session_id)?
                    .ok_or(Error::NoAudio)?;
                Transcripts::open(&self.recordings)?.rename(edit)?;
                return processed(1);
            }
            Action::SaveSpeaker(edit) => {
                Transcripts::open(&self.recordings)?.save_speaker(edit)?;
                return processed(1);
            }
            Action::SaveTranslation(edit) => {
                Transcripts::open(&self.recordings)?.save_manual_translation(edit)?;
                return processed(1);
            }
            action => action,
        };
        let id = match &action {
            Action::LiveModel
            | Action::LiveTranscript { .. }
            | Action::TranscribeLive { .. }
            | Action::Inspect { .. }
            | Action::History { .. }
            | Action::Export { .. }
            | Action::TrackTimeline { .. }
            | Action::Playback { .. }
            | Action::TrackArchive { .. }
            | Action::TrackArchiveChunk { .. } => return Err(Error::Invalid),
            Action::Install { model_id, .. }
            | Action::Transcribe { model_id, .. }
            | Action::Translate { model_id, .. } => model_id,
            Action::SaveTranslation(_)
            | Action::SaveUtterance(_)
            | Action::ResolveConflict { .. }
            | Action::RenameSession(_)
            | Action::SaveSpeaker(_) => {
                return Err(Error::Invalid);
            }
        };
        let model = catalog
            .models()
            .iter()
            .find(|model| model.id == *id)
            .ok_or(Error::Invalid)?;
        let device = device.ok_or(Error::Blocked(Block::DeviceProfile))?;
        if !matches!(action, Action::Install { .. }) && !model_dir(&model_root, model).is_dir() {
            return Err(Error::NotInstalled);
        }
        let runtime = RuntimeIdentity {
            id: &model.runtime.id,
            version: &model.runtime.version,
        };
        check_candidate(model, &device, &[runtime], true).map_err(Error::Blocked)?;
        let threads = device
            .logical_cpu_count
            .map_or(1, |value| value.get().min(2) as i32);
        let request = InstallRequest {
            root: &model_root,
            device: &device,
            activity: Activity {
                approved: true,
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
        let (session, language) = match action {
            Action::Transcribe {
                session_id,
                language,
                ..
            } if model.translation.is_none() => (session_id, language),
            Action::Translate { session_id, .. } if model.translation.is_some() => {
                let reader = reader.as_ref().ok_or(Error::NoAudio)?;
                let result = translate_saved(TranslateSaved {
                    store: &Transcripts::open(&self.recordings)?,
                    session: &session_id,
                    model,
                    adapters: &self.adapters,
                    request,
                    reader,
                    cancelled,
                    observe: &observe,
                });
                return processed(result?);
            }
            Action::Install { .. } => {
                let agent = transfer::agent();
                let fetch = |artifact: &voiceledger_core::models::Artifact, offset| {
                    transfer::fetch(&agent, artifact, offset)
                };
                let result = if model.translation.is_some() {
                    let adapter = TranslationAdapter::load(model, &self.adapters)?;
                    voiceledger_core::models::install(model, request, &adapter, fetch, || {
                        stop_loading() || !model_memory_ok(model, &device, observe())
                    })
                } else {
                    let adapter = Adapter::load(model, &self.adapters, threads)?;
                    voiceledger_core::models::install(model, request, &adapter, fetch, stop_loading)
                };
                result.map_err(|error| {
                    let sample = observe();
                    let error = deferred(error.into(), cancelled(), sample, reader.as_ref());
                    if matches!(error, Error::Cancelled)
                        && !cancelled()
                        && model.translation.is_some()
                        && !model_memory_ok(model, &device, sample)
                    {
                        return Error::Deferred(Reason::Memory);
                    }
                    error
                })?;
                return Ok(Vec::new());
            }
            _ => return Err(Error::Invalid),
        };
        let reader = reader.as_ref().ok_or(Error::NoAudio)?;
        let transcripts = Transcripts::open(&self.recordings)?;
        let queue = transcripts.queue();
        for track in reader.session_tracks(&session)? {
            if queue.live_exists(&session, &jobs::SourceJob::live(&track, model, language))? {
                return processed(
                    live::Task {
                        reader,
                        store: &transcripts,
                        session: &session,
                        model,
                        translation: None,
                        language,
                        adapters: &self.adapters,
                        model_root: &model_root,
                        device: &device,
                        allow_active: false,
                    }
                    .run(cancelled, &observe, &mut publish)?,
                );
            }
        }
        let tracks = reader.inputs(&session)?;
        if tracks.is_empty() {
            return Err(Error::NoAudio);
        }
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
            let mut publish_partial = |value: Option<Partial>| publish(value.map(Update::Partial));
            let mut writer = Writer {
                transcripts: &transcripts,
                session: &session,
                model,
                language,
                job: Some(&asr),
                translate: None,
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
        queue.finish(&ids, jobs::Outcome::for_result(&result))?;
        processed(result?)
    }
}

struct TranslateSaved<'a> {
    store: &'a Transcripts,
    session: &'a StableId,
    model: &'a Model,
    adapters: &'a Path,
    request: InstallRequest<'a>,
    reader: &'a CommittedReader,
    cancelled: &'a (dyn Fn() -> bool + Sync),
    observe: &'a dyn Fn() -> ResourceSample,
}

fn translate_saved(task: TranslateSaved<'_>) -> Result<u64> {
    let TranslateSaved {
        store,
        session,
        model,
        adapters,
        request,
        reader,
        cancelled,
        observe,
    } = task;
    let provenance = model_ref(model)?;
    let queue = store.queue();
    queue.recover()?;
    let mut governor = Governor::default();
    let started = Instant::now();
    let mut policy = || {
        check_cancel(cancelled)?;
        let decision = governor.observe(
            Observation {
                resources: observe(),
                recording_active: reader.recording()?,
                pending_jobs: queue.higher_priority_backlog(JobKind::Translation)?,
                ..Observation::default()
            },
            started.elapsed().as_millis() as u64,
        );
        if decision.tier.admits(JobKind::Translation) {
            Ok(())
        } else {
            Err(Error::Deferred(decision.reason.unwrap_or(Reason::Memory)))
        }
    };
    let adapter = TranslationAdapter::load(model, adapters)?;
    let mut engine = None;
    let mut after = String::new();
    let mut saved = 0;
    let mut problem = None;
    loop {
        let batch = store.translation_sources(session, &after)?;
        if batch.is_empty() {
            return problem.map_or(Ok(saved), Err);
        }
        for source in batch {
            after = String::from(source.utterance_id.clone());
            if !source.is_durable()
                || source.language != Language::En
                || source.text.trim().is_empty()
            {
                continue;
            }
            let spec = (&source.id, Language::Zh, &provenance);
            let job = queue.enqueue(store.id()?, session, JobKind::Translation, &spec)?;
            let ids = [job.as_str()];
            if !queue.begin(&ids)? {
                continue;
            }
            let attempt = (|| {
                policy()?;
                if engine.is_none() {
                    if !model_memory_ok(model, request.device, observe()) {
                        return Err(Error::Deferred(Reason::Memory));
                    }
                    let installed = voiceledger_core::models::install(
                        model,
                        InstallRequest { ..request },
                        &adapter,
                        |_, _| {
                            Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                "Model not installed.",
                            ))
                        },
                        || {
                            let sample = observe();
                            cancelled()
                                || sample.emergency().is_some()
                                || !model_memory_ok(model, request.device, sample)
                                || reader.recording().unwrap_or(true)
                        },
                    )?;
                    policy()?;
                    engine = Some(adapter.engine(installed.directory())?);
                }
                let text = engine
                    .as_ref()
                    .ok_or(Error::Runtime)?
                    .translate(&source.text)?;
                policy()?;
                store.save_machine_translation(session, &source, text, provenance.clone())
            })()
            .map_err(|error| {
                let sample = observe();
                let error = deferred(error, cancelled(), sample, Some(reader));
                if matches!(error, Error::Cancelled)
                    && !cancelled()
                    && !model_memory_ok(model, request.device, sample)
                {
                    return Error::Deferred(Reason::Memory);
                }
                error
            });
            queue.finish(&ids, jobs::Outcome::for_result(&attempt))?;
            match attempt {
                Ok(added) => saved += u64::from(added),
                Err(error @ (Error::TranslationTextLimit | Error::RevisionConflict)) => {
                    problem = Some(error)
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn model_memory_ok(
    model: &Model,
    device: &DeviceCapabilityProfile,
    sample: ResourceSample,
) -> bool {
    sample
        .available_memory_bytes
        .and_then(|available| {
            current_memory_budget(
                device,
                available,
                sample.low_memory_threshold_bytes.unwrap_or(0),
            )
        })
        .zip(model.peak_ram_estimate.as_ref())
        .is_some_and(|(budget, peak)| budget >= peak.bytes)
}

fn model_ref(model: &Model) -> Result<ModelRef> {
    Ok(ModelRef {
        model_id: model.id.clone(),
        model_version: model.version.clone(),
        runtime_version: model.runtime.version.clone(),
        sha256: model
            .artifacts
            .first()
            .ok_or(Error::Invalid)?
            .sha256
            .clone(),
    })
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum Update {
    Partial(Partial),
    Live {
        stage: LiveStage,
        partials: Vec<Partial>,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveStage {
    Loading,
    WaitingForAudio,
    Listening,
}

pub fn encode_update(value: Update) -> Result<Vec<u8>> {
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
    has_translation_source: bool,
    tracks: Vec<HistoryTrack>,
    title: Option<String>,
    text_only: bool,
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

fn playback_error(error: voiceledger_audio::Error) -> Error {
    match error {
        voiceledger_audio::Error::Unavailable => Error::NoAudio,
        voiceledger_audio::Error::Invalid => Error::Invalid,
        error => error.into(),
    }
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
