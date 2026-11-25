mod dav;
mod journal;

use serde::{Deserialize, Serialize};

use crate::contracts::{StableId, SyncEntity, SyncEvent, SyncOperation};
use crate::vault::{ObjectId, SealedObject, SyncScopes, Vault};
use zeroize::Zeroizing;

pub use dav::WebDav;
pub use journal::{EntityState, Journal, Receive};

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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkedEvent {
    event: SyncEvent,
    parents: Vec<StableId>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireEvent {
    Linked(LinkedEvent),
    Original(SyncEvent),
}

pub struct PendingEvent {
    key: ObjectKey,
    envelope: SealedObject,
}

impl PendingEvent {
    pub fn seal(vault: &Vault, scopes: SyncScopes, event: &SyncEvent) -> Result<Self, SyncError> {
        Self::seal_revision(vault, scopes, event, &[])
    }

    pub fn seal_revision(
        vault: &Vault,
        scopes: SyncScopes,
        event: &SyncEvent,
        parents: &[StableId],
    ) -> Result<Self, SyncError> {
        let enabled = match event.entity_kind {
            SyncEntity::AudioChunk => scopes.raw_audio,
            SyncEntity::SpeakerProfile => scopes.speaker_profiles,
            _ => scopes.text,
        };
        if !scopes.enabled || !enabled {
            return Err(SyncError::Scope);
        }
        let record = LinkedEvent {
            event: event.clone(),
            parents: parents.to_vec(),
        };
        validate(&record)?;
        let encoded =
            Zeroizing::new(serde_json::to_vec(&record).map_err(|_| SyncError::Integrity)?);
        let envelope = vault.seal(&encoded).map_err(|_| SyncError::Integrity)?;
        let key = key(vault, &event.device_id, event.sequence.get(), envelope.id())?;
        Ok(Self { key, envelope })
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

pub fn read_event(
    vault: &Vault,
    backend: &impl SyncBackend,
    key: &ObjectKey,
) -> Result<SyncEvent, SyncError> {
    Ok(read_linked(vault, backend, key)?.event)
}

fn read_linked(
    vault: &Vault,
    backend: &impl SyncBackend,
    key: &ObjectKey,
) -> Result<LinkedEvent, SyncError> {
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
    let record = match serde_json::from_slice(&opened).map_err(|_| SyncError::Integrity)? {
        WireEvent::Linked(record) => record,
        WireEvent::Original(event) => LinkedEvent {
            event,
            parents: Vec::new(),
        },
    };
    validate(&record)?;
    if self::key(
        vault,
        &record.event.device_id,
        record.event.sequence.get(),
        &id,
    )? != *key
    {
        return Err(SyncError::Integrity);
    }
    Ok(record)
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

fn validate(record: &LinkedEvent) -> Result<(), SyncError> {
    use std::collections::BTreeSet;

    let event = &record.event;
    if (event.operation == SyncOperation::Restore) != event.restored_from_tombstone_id.is_some()
        || event.object_refs.len() > 1024
        || record.parents.len() > 64
        || (event.operation != SyncOperation::Upsert && !record.parents.is_empty())
        || record.parents.contains(&event.revision_id)
        || record.parents.iter().collect::<BTreeSet<_>>().len() != record.parents.len()
    {
        return Err(SyncError::Integrity);
    }
    Ok(())
}
