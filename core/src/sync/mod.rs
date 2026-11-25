mod dav;
mod outbox;
mod probe;
mod s3;

use crate::contracts::{StableId, SyncEntity, SyncEvent, SyncOperation};
use crate::vault::{ObjectId, SealedObject, SyncScopes, Vault};
use zeroize::Zeroizing;

pub use dav::WebDav;
pub use outbox::Outbox;
pub use probe::check_provider;
pub use s3::S3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncError {
    Configuration,
    Scope,
    AuthenticationRequired,
    Quota,
    Redirect,
    ProviderUnsupported,
    NotFound,
    Transport,
    Retry,
    Conflict,
    Integrity,
    LocalStorage,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ObjectKey(String);

impl ObjectKey {
    pub fn new(value: String) -> Result<Self, SyncError> {
        if value.len() > 512
            || !value.split('/').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return Err(SyncError::Configuration);
        }
        Ok(Self(value))
    }

    pub fn value(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PutOutcome {
    Created,
    Exists,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub keys: Vec<ObjectKey>,
    pub continuation: Option<String>,
}

pub trait SyncBackend {
    fn put_immutable(&self, key: &ObjectKey, body: &SealedObject) -> Result<PutOutcome, SyncError>;
    fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, SyncError>;
    fn list(&self, prefix: &ObjectKey, continuation: Option<&str>) -> Result<Page, SyncError>;
    fn delete(&self, key: &ObjectKey) -> Result<(), SyncError>;
}

pub struct PendingEvent {
    key: ObjectKey,
    envelope: SealedObject,
}

impl PendingEvent {
    pub fn seal(vault: &Vault, scopes: SyncScopes, event: &SyncEvent) -> Result<Self, SyncError> {
        check_scope(scopes, event.entity_kind)?;
        validate(event)?;
        let encoded = Zeroizing::new(serde_json::to_vec(event).map_err(|_| SyncError::Integrity)?);
        let envelope = vault.seal(&encoded).map_err(|_| SyncError::Integrity)?;
        let key = key(vault, &event.device_id, event.sequence.get(), envelope.id())?;
        Ok(Self { key, envelope })
    }

    fn reopen(vault: &Vault, bytes: Vec<u8>) -> Result<(Self, SyncEvent), SyncError> {
        let (envelope, plaintext) = vault.reopen(bytes).map_err(|_| SyncError::Integrity)?;
        let event: SyncEvent =
            serde_json::from_slice(&plaintext).map_err(|_| SyncError::Integrity)?;
        validate(&event)?;
        let key = key(vault, &event.device_id, event.sequence.get(), envelope.id())?;
        Ok((Self { key, envelope }, event))
    }

    pub fn key(&self) -> &ObjectKey {
        &self.key
    }

    pub fn publish(&self, backend: &impl SyncBackend) -> Result<(), SyncError> {
        match backend.put_immutable(&self.key, &self.envelope)? {
            PutOutcome::Created => Ok(()),
            PutOutcome::Exists if backend.get(&self.key)? == self.envelope.envelope() => Ok(()),
            PutOutcome::Exists => Err(SyncError::Conflict),
        }
    }
}

fn storage_status(status: u16) -> Result<(), SyncError> {
    match status {
        300..=399 => Err(SyncError::Redirect),
        401 | 403 => Err(SyncError::AuthenticationRequired),
        404 => Err(SyncError::NotFound),
        507 => Err(SyncError::Quota),
        408 | 425 | 429 | 500..=599 => Err(SyncError::Retry),
        _ => Ok(()),
    }
}

fn check_scope(scopes: SyncScopes, kind: SyncEntity) -> Result<(), SyncError> {
    let included = match kind {
        SyncEntity::AudioChunk => scopes.raw_audio,
        SyncEntity::SpeakerProfile => scopes.speaker_profiles,
        _ => scopes.text,
    };
    if scopes.enabled && included {
        Ok(())
    } else {
        Err(SyncError::Scope)
    }
}

pub fn read_event(
    vault: &Vault,
    backend: &impl SyncBackend,
    key: &ObjectKey,
) -> Result<SyncEvent, SyncError> {
    let name = key.value().rsplit('/').next().ok_or(SyncError::Integrity)?;
    let (sequence, object) = name.split_once('-').ok_or(SyncError::Integrity)?;
    if object.len() != 32 || sequence.len() != 20 {
        return Err(SyncError::Integrity);
    }
    let mut id = [0_u8; 16];
    for (index, pair) in object.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| SyncError::Integrity)?;
        id[index] = u8::from_str_radix(pair, 16).map_err(|_| SyncError::Integrity)?;
    }
    let result = backend.get(key)?;
    let opened = vault.open(&id, &result).map_err(|_| SyncError::Integrity)?;
    let event: SyncEvent = serde_json::from_slice(&opened).map_err(|_| SyncError::Integrity)?;
    validate(&event)?;
    if self::key(vault, &event.device_id, event.sequence.get(), &id)? != *key {
        return Err(SyncError::Integrity);
    }
    Ok(event)
}

fn key(
    vault: &Vault,
    device: &StableId,
    sequence: u64,
    id: &ObjectId,
) -> Result<ObjectKey, SyncError> {
    let prefix = device_prefix(vault, device)?;
    ObjectKey::new(format!("{}/{sequence:020}-{}", prefix.value(), hex(id)))
}

pub fn device_prefix(vault: &Vault, device: &StableId) -> Result<ObjectKey, SyncError> {
    ObjectKey::new(format!(
        "voiceledger/{}/g-{}/devices/{}",
        hex(vault.id()),
        vault.generation(),
        String::from(device.clone()),
    ))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing into a String succeeds");
    }
    output
}

fn validate(event: &SyncEvent) -> Result<(), SyncError> {
    if (event.operation == SyncOperation::Restore) != event.restored_from_tombstone_id.is_some()
        || event.object_refs.len() > 1024
    {
        return Err(SyncError::Integrity);
    }
    Ok(())
}
