mod engine;
mod pipeline;
mod transcript;
mod transfer;

#[cfg(test)]
mod tests;

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use voiceledger_audio::{CommittedReader, FinishedSession};
use voiceledger_core::contracts::{LanguagePolicy, StableId};
use voiceledger_core::device_profile;
use voiceledger_core::models::{
    Activity, Block, Catalog, InstallError, InstallRequest, Model, RuntimeIdentity, check_candidate,
};

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
            .open(self.profiles.join("speech-worker.lock"))?;
        lock.try_lock().map_err(|_| Error::Busy)?;
        let threads = device
            .logical_cpu_count
            .map_or(1, |value| value.get().min(2) as i32);
        let adapter = Adapter::load(model, &self.adapters, threads)?;
        let request = InstallRequest {
            root: &model_root,
            device: &device,
            activity: Activity {
                approved,
                recording_active: active,
            },
        };
        let installed = if transcription.is_none() {
            let agent = transfer::agent();
            voiceledger_core::models::install(
                model,
                request,
                &adapter,
                |artifact, offset| transfer::fetch(&agent, artifact, offset),
                cancelled,
            )?
        } else {
            voiceledger_core::models::install(
                model,
                request,
                &adapter,
                |_, _| {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Model not installed.",
                    ))
                },
                cancelled,
            )?
        };
        let Some((session, language)) = transcription else {
            return Ok(Vec::new());
        };
        let reader = reader.ok_or(Error::NoAudio)?;
        let Some(first) = reader.next_chunk(&session, None)? else {
            return Err(Error::NoAudio);
        };
        let mut engine = adapter.engine(model, installed.directory(), language)?;
        let transcripts = Transcripts::open(&self.recordings)?;
        let mut writer = Writer {
            transcripts: &transcripts,
            session: &session,
            model,
            language,
            publish_partial: &mut publish_partial,
            saved: 0,
        };
        let mut processor = Processing {
            engine: &mut engine,
            output: &mut writer,
            language,
            cancelled,
            partials: true,
        };
        let mut segments = Segmenter::new();
        let mut next = Some(first);
        while let Some(chunk) = next {
            if reader.recording()? {
                return Err(Error::Blocked(Block::RecordingActive));
            }
            let track = chunk.track_id.clone();
            let start = chunk.start_ns;
            segments.ingest(chunk, &mut processor)?;
            next = reader.next_chunk(&session, Some((&track, start)))?;
        }
        segments.flush(&mut processor)?;
        Ok(serde_json::to_vec(
            &serde_json::json!({ "saved": writer.saved }),
        )?)
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
