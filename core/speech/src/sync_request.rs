use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use voiceledger_core::contracts::StableId;
use voiceledger_core::sync::{SyncBackend, SyncError, VaultSync, connect};
use voiceledger_core::vault::Vault;

use crate::Error;
use crate::transcript::Transcripts;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    recordings: PathBuf,
    directory: PathBuf,
    action: Action,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Synchronize,
    VerifyKey,
    RemoveDevice { target: StableId },
}

#[derive(Serialize)]
struct Report {
    device: StableId,
    devices: Vec<StableId>,
    imported: usize,
    generation: u32,
}

pub fn request(
    command: &[u8],
    configuration: &[u8],
    recovery: &[u8],
    successor: &[u8],
) -> Result<Vec<u8>, SyncError> {
    let args: Request = serde_json::from_slice(command).map_err(|_| SyncError::Configuration)?;
    if !args.recordings.is_absolute() || !args.directory.is_absolute() {
        return Err(SyncError::Configuration);
    }
    let vault = recover(recovery)?;
    let backend = connect(configuration)?;
    fs::create_dir_all(&args.directory).map_err(|_| SyncError::LocalStorage)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(args.directory.join("synchronize.lock"))
        .map_err(|_| SyncError::LocalStorage)?;
    lock.try_lock().map_err(|_| SyncError::Retry)?;
    fs::create_dir_all(&args.recordings).map_err(|_| SyncError::LocalStorage)?;
    let text = Transcripts::open(&args.recordings).map_err(local_error)?;
    let device = text.sync_device().map_err(local_error)?;
    let backend = backend.as_ref();
    let report = match args.action {
        Action::Synchronize if successor.is_empty() => {
            synchronize(&args, &text, &vault, backend, device)?
        }
        Action::VerifyKey if successor.is_empty() => {
            VaultSync::verify_key(&vault, backend, &device)?;
            Report {
                device,
                devices: Vec::new(),
                imported: 0,
                generation: vault.generation(),
            }
        }
        Action::RemoveDevice { ref target } if !successor.is_empty() => {
            let next = recover(successor)?;
            if !VaultSync::finished_rotation(&vault, &next, backend, target)? {
                let mut sync = VaultSync::open(&args.directory, &vault, backend, device.clone())?;
                exchange(&text, &args.recordings, &mut sync)?;
                sync.remove(&next, target)?;
            }
            Report {
                device,
                devices: Vec::new(),
                imported: 0,
                generation: next.generation(),
            }
        }
        _ => return Err(SyncError::Configuration),
    };
    serde_json::to_vec(&report).map_err(|_| SyncError::LocalStorage)
}

fn synchronize(
    args: &Request,
    text: &Transcripts,
    vault: &Vault,
    backend: &dyn SyncBackend,
    device: StableId,
) -> Result<Report, SyncError> {
    let mut sync = VaultSync::open(&args.directory, vault, backend, device.clone())?;
    let imported = exchange(text, &args.recordings, &mut sync)?;
    Ok(Report {
        device,
        devices: sync.devices()?,
        imported,
        generation: vault.generation(),
    })
}

fn exchange(
    text: &Transcripts,
    recordings: &Path,
    sync: &mut VaultSync<'_>,
) -> Result<usize, SyncError> {
    sync.pull()?;
    let imported = text.import(sync.journal()).map_err(local_error)?;
    sync.stage_text(text.outgoing(recordings).map_err(local_error)?, || {
        text.id().map_err(local_error)
    })?;
    sync.publish()?;
    Ok(imported)
}

fn recover(bytes: &[u8]) -> Result<Vault, SyncError> {
    let code = std::str::from_utf8(bytes).map_err(|_| SyncError::Integrity)?;
    Vault::recover(code).map_err(|_| SyncError::Integrity)
}

fn local_error(error: Error) -> SyncError {
    match error {
        Error::RevisionConflict => SyncError::Conflict,
        Error::Invalid | Error::Integrity => SyncError::Integrity,
        _ => SyncError::LocalStorage,
    }
}
