use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::contracts::DeviceCapabilityProfile;
use crate::scheduler::ResourceSample;

use super::policy::{check_model_memory, check_pressure, estimate, preflight};
use super::{Activity, Artifact, Block, Model, RuntimeIdentity};

pub struct Download {
    pub start: u64,
    pub body: Box<dyn Read>,
}

pub trait RuntimeProbe {
    fn identity(&self) -> RuntimeIdentity<'_>;

    fn load_and_release(
        &self,
        model: &Model,
        directory: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> bool;
}

pub struct InstallRequest<'a> {
    pub root: &'a Path,
    pub device: &'a DeviceCapabilityProfile,
    pub activity: Activity,
    pub resources: Option<&'a dyn Fn() -> ResourceSample>,
    pub progress: Option<&'a mut dyn FnMut(u64)>,
}

#[derive(Debug)]
pub enum InstallError {
    Blocked(Block),
    Cancelled,
    Busy,
    Storage(io::Error),
    Transfer,
    InvalidArtifact,
    RuntimeLoad,
}

impl From<io::Error> for InstallError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub struct InstalledModel<'a> {
    model: &'a Model,
    directory: PathBuf,
}

impl<'a> InstalledModel<'a> {
    pub fn model(&self) -> &'a Model {
        self.model
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

pub fn install<'a>(
    model: &'a Model,
    request: InstallRequest<'_>,
    runtime: &impl RuntimeProbe,
    mut fetch: impl FnMut(&Artifact, &str, u64) -> io::Result<Download>,
    cancelled: impl Fn() -> bool,
) -> Result<InstalledModel<'a>, InstallError> {
    request.activity.check().map_err(InstallError::Blocked)?;
    check_pressure(request.device).map_err(InstallError::Blocked)?;
    let identities = [runtime.identity()];
    preflight(model, request.device, &identities, 0, estimate(model))
        .map_err(InstallError::Blocked)?;
    check_cancel(&cancelled)?;

    fs::create_dir_all(request.root)?;
    let stem = format!("{}-{}", model.id, model.version);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(request.root.join(format!("{stem}.lock")))?;
    lock.try_lock().map_err(|_| InstallError::Busy)?;
    check_cancel(&cancelled)?;
    let destination = request.root.join(&stem);
    let staging = request.root.join(format!("{stem}.partial"));
    let published = destination.try_exists()?;
    let source = if published { &destination } else { &staging };

    if !published {
        let remaining = model.artifacts.iter().try_fold(0, |total, artifact| {
            let written = partial_bytes(&source.join(&artifact.file), artifact)?;
            Ok::<_, io::Error>(total + artifact.size_bytes - written)
        })?;
        preflight(
            model,
            request.device,
            &identities,
            remaining,
            estimate(model),
        )
        .map_err(InstallError::Blocked)?;
        fs::create_dir_all(source)?;
    }

    let mut progress = request.progress;
    let mut completed = 0;
    for artifact in &model.artifacts {
        check_cancel(&cancelled)?;
        let path = source.join(&artifact.file);
        if published {
            if !verify(&path, artifact, &cancelled)? {
                return Err(InstallError::InvalidArtifact);
            }
        } else {
            download_artifact(&path, artifact, &mut fetch, &cancelled, &mut |bytes| {
                if let Some(publish) = progress.as_mut() {
                    publish(completed + bytes);
                }
            })?;
        }
        completed += artifact.size_bytes;
    }
    check_cancel(&cancelled)?;
    if let Some(observe) = request.resources {
        check_model_memory(model, request.device, observe()).map_err(InstallError::Blocked)?;
    }
    if !runtime.load_and_release(model, source, &cancelled) {
        check_cancel(&cancelled)?;
        return Err(InstallError::RuntimeLoad);
    }
    check_cancel(&cancelled)?;
    if !published {
        fs::rename(staging, &destination)?;
    }
    Ok(InstalledModel {
        model,
        directory: destination,
    })
}

/// Verify a published package for inference without provisioning or probing a model.
pub fn verify_installed<'a>(
    model: &'a Model,
    root: &Path,
    device: &DeviceCapabilityProfile,
    runtime: RuntimeIdentity<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<InstalledModel<'a>, InstallError> {
    preflight(model, device, &[runtime], 0, estimate(model)).map_err(InstallError::Blocked)?;
    let directory = root.join(format!("{}-{}", model.id, model.version));
    for artifact in &model.artifacts {
        if !verify(&directory.join(&artifact.file), artifact, &cancelled)? {
            return Err(InstallError::InvalidArtifact);
        }
    }
    check_cancel(&cancelled)?;
    Ok(InstalledModel { model, directory })
}

fn partial_bytes(path: &Path, artifact: &Artifact) -> io::Result<u64> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= artifact.size_bytes => Ok(meta.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn download_artifact(
    path: &Path,
    artifact: &Artifact,
    fetch: &mut impl FnMut(&Artifact, &str, u64) -> io::Result<Download>,
    cancelled: &impl Fn() -> bool,
    progress: &mut impl FnMut(u64),
) -> Result<(), InstallError> {
    progress(partial_bytes(path, artifact)?);
    let mut failure = InstallError::Transfer;
    for url in artifact.download_sources() {
        check_cancel(cancelled)?;
        let outcome = transfer(
            path,
            artifact,
            |offset| fetch(artifact, &url, offset),
            cancelled,
            progress,
        );
        check_cancel(cancelled)?;
        match outcome {
            Ok(()) => {
                if verify(path, artifact, cancelled)? {
                    return Ok(());
                }
                fs::remove_file(path)?;
                progress(0);
                failure = InstallError::InvalidArtifact;
            }
            Err(InstallError::Transfer) => {}
            Err(InstallError::InvalidArtifact) => {
                progress(0);
                failure = InstallError::InvalidArtifact;
            }
            Err(error) => return Err(error),
        }
    }
    Err(failure)
}

fn transfer(
    path: &Path,
    artifact: &Artifact,
    fetch: impl FnOnce(u64) -> io::Result<Download>,
    cancelled: &impl Fn() -> bool,
    progress: &mut impl FnMut(u64),
) -> Result<(), InstallError> {
    let offset = partial_bytes(path, artifact)?;
    if offset == artifact.size_bytes {
        return Ok(());
    }
    let response = fetch(offset);
    check_cancel(cancelled)?;
    let mut response = response.map_err(|_| InstallError::Transfer)?;
    if response.start != offset && response.start != 0 {
        return Err(InstallError::Transfer);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(response.start != 0)
        .truncate(response.start == 0)
        .open(path)?;
    let mut total = response.start;
    let mut reported = total;
    if total != offset {
        progress(total);
    }
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = response
            .body
            .read(&mut buffer)
            .map_err(|_| InstallError::Transfer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > artifact.size_bytes {
            drop(file);
            fs::remove_file(path)?;
            return Err(InstallError::InvalidArtifact);
        }
        file.write_all(&buffer[..count])?;
        if total - reported >= 512 * 1024 || total == artifact.size_bytes {
            progress(total);
            reported = total;
        }
    }
    file.sync_all()?;
    if total == artifact.size_bytes {
        Ok(())
    } else {
        Err(InstallError::Transfer)
    }
}

fn verify(
    path: &Path,
    artifact: &Artifact,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, InstallError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != artifact.size_bytes {
        return Ok(false);
    }
    let mut reader = file.take(artifact.size_bytes + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(reader.limit() == 1
        && format!("{:x}", hash.finalize()) == String::from(artifact.sha256.clone()))
}

fn check_cancel(cancelled: &dyn Fn() -> bool) -> Result<(), InstallError> {
    if cancelled() {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}
