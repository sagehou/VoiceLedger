use std::cell::{Cell, OnceCell};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use voiceledger_audio::CommittedReader;
use voiceledger_core::SessionState;
use voiceledger_core::contracts::{
    DeviceCapabilityProfile, Language, LanguagePolicy, ModelRef, StableId,
};
use voiceledger_core::models::{
    Catalog, InstalledModel, Model, RuntimeIdentity, current_memory_budget, verify_installed,
};
use voiceledger_core::scheduler::{Governor, JobKind, Observation, Reason, ResourceSample, Tier};

use crate::engine::{Adapter, adapter_path};
use crate::jobs::{Outcome, SourceJob};
use crate::pipeline::{Engine, Processing, Segmenter};
use crate::transcript::{self, Transcripts, Writer};
use crate::translation_engine::{TranslationAdapter, Translator};
use crate::{
    Error, LiveStage, Result, Update, check_cancel, model_dir, model_memory_ok, model_ref,
};

const WHISPER: &str = "whisper-tiny-multilingual-q5-1";
const SENSEVOICE: &str = "sensevoice-small-int8";
const SENSEVOICE_GGUF: &str = "sensevoice-small-gguf-q8-0";
const SENSEVOICE_GGUF_Q6: &str = "sensevoice-small-gguf-q6-k";
pub(super) const NEMOTRON: &str = "nemotron-3-5-asr-streaming-0-6b-q4-k-m";
pub(super) const TRANSLATION_MODEL: &str = "opus-mt-en-zh-int8";
const POLL: Duration = Duration::from_millis(200);
const QUIET: Duration = Duration::from_secs(3);

pub(super) fn asr_model<'a>(catalog: &'a Catalog, model_id: Option<&str>) -> Result<&'a Model> {
    let id = model_id.unwrap_or(WHISPER);
    if !matches!(
        id,
        WHISPER | SENSEVOICE | SENSEVOICE_GGUF | SENSEVOICE_GGUF_Q6 | NEMOTRON
    ) {
        return Err(Error::Invalid);
    }
    catalog
        .models()
        .iter()
        .find(|model| model.id == id)
        .ok_or(Error::Invalid)
}

pub(super) fn model_status(
    profiles: &Path,
    adapters: &Path,
    model_id: Option<&str>,
) -> Result<Vec<u8>> {
    let catalog = Catalog::bundled().map_err(|_| Error::Invalid)?;
    let model = asr_model(&catalog, model_id)?;
    let status = if !adapter_path(&model.runtime.id, adapters).is_some_and(|path| path.is_file()) {
        "runtime_unavailable"
    } else if !model_dir(&profiles.join("models"), model).is_dir() {
        "not_installed"
    } else {
        "installed"
    };
    Ok(serde_json::to_vec(
        &serde_json::json!({ "status": status }),
    )?)
}

#[derive(Serialize)]
struct Transcript {
    lines: Vec<transcript::Line>,
    translation_pending: bool,
    job: Option<crate::jobs::Status>,
}

pub(super) fn transcript(root: &Path, session: &StableId, worker_active: bool) -> Result<Vec<u8>> {
    let mut lines = Vec::new();
    transcript::visit_selected(root, session, transcript::Selection::Recent, |line| {
        lines.push(line);
        Ok(())
    })?;
    lines.reverse();
    let (translation_pending, job) = match crate::library::open_readonly(root)? {
        Some((db, version)) if version >= 2 => {
            let queue = crate::jobs::Queue(&db);
            (
                version >= 3 && queue.translation_pending(session)?,
                queue.status(session, worker_active)?,
            )
        }
        _ => (false, None),
    };
    Ok(serde_json::to_vec(&Transcript {
        lines,
        translation_pending,
        job,
    })?)
}

pub(super) struct Task<'a> {
    pub reader: &'a CommittedReader,
    pub store: &'a Transcripts,
    pub session: &'a StableId,
    pub model: &'a Model,
    pub translation: Option<&'a Model>,
    pub language: LanguagePolicy,
    pub adapters: &'a Path,
    pub model_root: &'a Path,
    pub device: &'a DeviceCapabilityProfile,
    pub allow_active: bool,
}

struct Track {
    id: StableId,
    vad: String,
    asr: String,
    next_ns: i64,
    last_chunk: Option<Instant>,
    segments: Segmenter,
}

impl Task<'_> {
    pub fn run(
        &self,
        cancelled: &(dyn Fn() -> bool + Sync),
        observe: &(dyn Fn() -> ResourceSample + Sync),
        publish: &mut dyn FnMut(Option<Update>),
    ) -> Result<u64> {
        let ids = self.reader.session_tracks(self.session)?;
        if ids.is_empty() {
            return Err(Error::NoAudio);
        }
        let queue = self.store.queue();
        queue.recover()?;
        let mut tracks = Vec::with_capacity(ids.len());
        for id in ids {
            let spec = SourceJob::live(&id, self.model, self.language);
            let vad = queue.enqueue(self.store.id()?, self.session, JobKind::Vad, &spec)?;
            let asr = queue.enqueue(self.store.id()?, self.session, JobKind::Asr, &spec)?;
            let next_ns = queue.cursor(&asr)?.map_or(0, |cursor| cursor.next_ns);
            tracks.push(Track {
                id,
                vad,
                asr,
                next_ns,
                segments: Segmenter::new(),
                last_chunk: None,
            });
        }
        let ids: Vec<_> = tracks
            .iter()
            .flat_map(|track| [track.vad.as_str(), track.asr.as_str()])
            .collect();
        if !queue.begin(&ids)? {
            return Ok(0);
        }

        let result = self
            .consume(&mut tracks, cancelled, observe, publish)
            .map_err(|error| crate::deferred(error, cancelled(), observe(), None));
        let ids: Vec<_> = tracks
            .iter()
            .flat_map(|track| [track.vad.as_str(), track.asr.as_str()])
            .collect();
        queue.finish(&ids, Outcome::for_result(&result))?;
        result
    }

    fn consume(
        &self,
        tracks: &mut [Track],
        cancelled: &(dyn Fn() -> bool + Sync),
        observe: &(dyn Fn() -> ResourceSample + Sync),
        publish: &mut dyn FnMut(Option<Update>),
    ) -> Result<u64> {
        let interrupted = || cancelled() || observe().emergency().is_some();
        let started = Instant::now();
        let ready = Cell::new(0_u32);
        let recording = Cell::new((1..=4).contains(&self.state()?));
        let mut governor = Governor::default();
        let mut last_rtf = None;
        let mut policy = |rtf| {
            check_cancel(&interrupted)?;
            if let Some(rate) = rtf {
                last_rtf = Some((rate, Instant::now()));
            }
            let decision = governor.observe(
                Observation {
                    resources: observe(),
                    live_transcription: self.allow_active,
                    recording_active: recording.get(),
                    pending_jobs: ready.get(),
                    rtf: last_rtf
                        .filter(|(_, at)| at.elapsed() < QUIET)
                        .map(|(rate, _)| rate),
                },
                started.elapsed().as_millis() as u64,
            );
            if decision.tier.admits(JobKind::Asr) {
                Ok(decision.tier)
            } else {
                Err(Error::Deferred(decision.reason.unwrap_or(Reason::Memory)))
            }
        };
        if self.allow_active {
            publish(Some(Update::Live {
                stage: LiveStage::Loading,
                partials: Vec::new(),
                processed_chunks: 0,
            }));
        }
        if !model_dir(self.model_root, self.model).is_dir() {
            return Err(Error::NotInstalled);
        }
        policy(None)?;
        let startup_resources = observe();
        if !model_memory_ok(self.model, self.device, startup_resources) {
            return Err(Error::Deferred(Reason::Memory));
        }
        let installed = self.verified(self.model, &interrupted)?;
        let threads = self
            .device
            .logical_cpu_count
            .map_or(1, |value| value.get().min(2) as i32);
        let adapter = Adapter::load(self.model, self.adapters, threads)?;
        policy(None)?;
        if !model_memory_ok(self.model, self.device, observe()) {
            return Err(Error::Deferred(Reason::Memory));
        }
        // A native stream has one cache for the Android microphone track.
        let mut engine = adapter.engine(
            self.model,
            installed.directory(),
            self.language,
            tracks.len() == 1,
        )?;
        let mut heard_audio = false;
        if self.allow_active {
            publish(Some(Update::Live {
                stage: LiveStage::WaitingForAudio,
                partials: Vec::new(),
                processed_chunks: 0,
            }));
        }

        let translation = self
            .translation
            .map(|model| {
                Ok::<_, Error>(Translation {
                    task: self,
                    model,
                    provenance: model_ref(model)?,
                    adapter: OnceCell::new(),
                    has_headroom: combined_memory_ok(
                        self.model,
                        model,
                        self.device,
                        startup_resources,
                    ),
                })
            })
            .transpose()?;
        let mut translator = None;
        let mut translation_failed = false;
        let provenance = model_ref(self.model)?;
        let mut partials = BTreeMap::new();
        let mut saved = 0;
        let mut processed_chunks = 0;
        loop {
            let state = self.state()?;
            let ended = state == SessionState::Completed as i32
                || state == SessionState::Interrupted as i32;
            recording.set(!ended);
            let pending = tracks.iter().try_fold(0, |total, track| {
                self.reader
                    .live_backlog(&track.id, track.next_ns)
                    .map(|count| total + count)
            })?;
            ready.set(pending);
            // Consume the committed backlog before recomputing unfinished speech.
            let partials_due = !ended && (engine.streams() || pending <= tracks.len() as u32);
            let tier = policy(None)?;
            if !tier.admits(JobKind::Translation) {
                translator = None;
            }
            let mut advanced = 0;
            for track in &mut *tracks {
                let chunk = self
                    .reader
                    .live_chunk(self.session, &track.id, track.next_ns)?;
                if !heard_audio && chunk.as_ref().is_some_and(|chunk| chunk.samples.is_some()) {
                    heard_audio = true;
                    if self.allow_active {
                        publish(Some(Update::Live {
                            stage: LiveStage::Listening,
                            partials: Vec::new(),
                            processed_chunks,
                        }));
                    }
                }
                let mut update = |value: Option<crate::Partial>| {
                    if let Some(value) = value {
                        partials.insert(track.id.clone(), value);
                    } else {
                        partials.remove(&track.id);
                    }
                    if self.allow_active {
                        (publish)(Some(Update::Live {
                            stage: LiveStage::Listening,
                            partials: partials.values().cloned().collect(),
                            processed_chunks,
                        }));
                    } else {
                        (publish)(partials.values().next_back().cloned().map(Update::Partial));
                    }
                };
                let mut writer = Writer {
                    transcripts: self.store,
                    session: self.session,
                    model: &provenance,
                    language: self.language,
                    job: Some(&track.asr),
                    translate: translation.as_ref().map(|task| &task.provenance),
                    publish_partial: &mut update,
                    saved: 0,
                };
                let mut processing = Processing {
                    engine: &mut engine,
                    output: &mut writer,
                    language: self.language,
                    cancelled: &interrupted,
                    partials: partials_due,
                    policy: &mut policy,
                };
                if let Some(chunk) = chunk {
                    let end_ns = chunk.end_ns;
                    track
                        .segments
                        .ingest(chunk, track.next_ns, &mut processing)?;
                    track.next_ns = end_ns;
                    track.last_chunk = Some(Instant::now());
                    advanced += 1;
                } else if (ended
                    || state == SessionState::Paused as i32
                    || track.last_chunk.is_some_and(|at| at.elapsed() >= QUIET))
                    && track.last_chunk.take().is_some()
                {
                    // A loopback stream can stop delivering PCM while its endpoint is silent.
                    track.segments.flush(&mut processing)?;
                    track.segments = Segmenter::new();
                    self.store
                        .queue()
                        .checkpoint(&track.asr, &track.id, track.next_ns)?;
                }
                saved += writer.saved;
            }
            // Check for more committed audio before scheduling each optional sentence.
            if advanced > 0 {
                processed_chunks += advanced;
                if self.allow_active && heard_audio {
                    publish(Some(Update::Live {
                        stage: LiveStage::Listening,
                        partials: partials.values().cloned().collect(),
                        processed_chunks,
                    }));
                }
                continue;
            }
            if let Some(translation) = &translation
                && !translation_failed
                && policy(None)?.admits(JobKind::Translation)
            {
                match translation.run_one(&mut translator, &interrupted, observe, &mut policy) {
                    Ok(true) => continue,
                    Ok(false) => {}
                    Err(Error::Deferred(_)) => translator = None,
                    Err(Error::TranslationTextLimit | Error::RevisionConflict) => continue,
                    Err(_) => {
                        translator = None;
                        translation_failed = true;
                    }
                }
            }
            if ended {
                return Ok(saved);
            }
            std::thread::sleep(POLL);
        }
    }

    fn state(&self) -> Result<i32> {
        let state = self
            .reader
            .session_state(self.session)?
            .ok_or(Error::NoAudio)?;
        if (!(1..=4).contains(&state) || !self.allow_active) && self.reader.recording()? {
            return Err(Error::Deferred(Reason::Recording));
        }
        if !(1..=6).contains(&state) {
            return Err(Error::NoAudio);
        }
        Ok(state)
    }

    fn verified<'a>(
        &self,
        model: &'a Model,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<InstalledModel<'a>> {
        if !model_dir(self.model_root, model).is_dir() {
            return Err(Error::NotInstalled);
        }
        Ok(verify_installed(
            model,
            self.model_root,
            self.device,
            RuntimeIdentity {
                id: &model.runtime.id,
                version: &model.runtime.version,
            },
            cancelled,
        )?)
    }
}

struct Translation<'a> {
    task: &'a Task<'a>,
    model: &'a Model,
    provenance: ModelRef,
    adapter: OnceCell<TranslationAdapter>,
    has_headroom: bool,
}

impl Translation<'_> {
    fn run_one<'a>(
        &'a self,
        engine: &mut Option<Translator<'a>>,
        interrupted: &(dyn Fn() -> bool + Sync),
        observe: &dyn Fn() -> ResourceSample,
        policy: &mut dyn FnMut(Option<f64>) -> Result<Tier>,
    ) -> Result<bool> {
        let queue = self.task.store.queue();
        let Some((job, source)) = queue.next_translation(self.task.session, &self.provenance)?
        else {
            return Ok(false);
        };
        if !source.is_durable() || source.language != Language::En || source.text.trim().is_empty()
        {
            return Err(Error::Invalid);
        }
        let safe = || self.has_headroom && model_memory_ok(self.model, self.task.device, observe());
        if engine.is_none() && !safe() {
            return Ok(false);
        }
        let ids = [job.as_str()];
        if !queue.begin(&ids)? {
            return Ok(false);
        }
        let translation_policy = |policy: &mut dyn FnMut(Option<f64>) -> Result<Tier>| {
            if policy(None)?.admits(JobKind::Translation) {
                Ok(())
            } else {
                Err(Error::Deferred(Reason::Backlog))
            }
        };
        let attempt = (|| {
            translation_policy(policy)?;
            if engine.is_none() {
                let directory = self
                    .task
                    .verified(self.model, &|| interrupted() || !safe())
                    .map_err(|error| {
                        if matches!(error, Error::Cancelled) && !interrupted() && !safe() {
                            Error::Deferred(Reason::Memory)
                        } else {
                            error
                        }
                    })?;
                if self.adapter.get().is_none() {
                    let _ = self
                        .adapter
                        .set(TranslationAdapter::load(self.model, self.task.adapters)?);
                }
                translation_policy(policy)?;
                if !safe() {
                    return Err(Error::Deferred(Reason::Memory));
                }
                *engine = Some(
                    self.adapter
                        .get()
                        .ok_or(Error::Runtime)?
                        .engine(directory.directory())?,
                );
            }
            let text = engine
                .as_ref()
                .ok_or(Error::Runtime)?
                .translate(&source.text)?;
            translation_policy(policy)?;
            self.task.store.save_machine_translation(
                self.task.session,
                &source,
                text,
                self.provenance.clone(),
            )
        })();
        queue.finish(&ids, Outcome::for_result(&attempt))?;
        attempt.map(|_| true)
    }
}

fn combined_memory_ok(
    asr: &Model,
    translation: &Model,
    device: &DeviceCapabilityProfile,
    sample: ResourceSample,
) -> bool {
    let budget = sample.available_memory_bytes.and_then(|available| {
        current_memory_budget(
            device,
            available,
            sample.low_memory_threshold_bytes.unwrap_or(0),
        )
    });
    let peak = asr
        .peak_ram_estimate
        .as_ref()
        .zip(translation.peak_ram_estimate.as_ref())
        .and_then(|(asr, translation)| asr.bytes.checked_add(translation.bytes));
    budget
        .zip(peak)
        .is_some_and(|(budget, peak)| budget >= peak)
}
