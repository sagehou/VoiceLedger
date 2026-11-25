mod dav;
mod journal;
mod outbox;
mod probe;
mod s3;
mod transfer;

use serde::{Deserialize, Serialize};

use crate::contracts::{
    Count, Source, StableId, SyncEntity, SyncEvent, SyncOperation, UtteranceKind, UtteranceRevision,
};
use crate::vault::{ObjectId, SealedObject, SyncScopes, Vault};
use zeroize::Zeroizing;

pub use dav::WebDav;
pub use journal::{EntityState, Journal, JournalStore, Receive};
pub use outbox::Outbox;
pub use probe::{check_provider, connect};
pub use s3::S3;
pub use transfer::VaultSync;

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
    GenerationRetired,
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncedTranscript {
    pub session_id: StableId,
    pub source: Source,
    pub revision: UtteranceRevision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionTextMetadata>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTextMetadata {
    pub created_at_ms: Count,
    pub elapsed_ns: Count,
    pub interrupted: bool,
    pub title: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkedEvent {
    event: SyncEvent,
    parents: Vec<StableId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transcript: Option<Box<SyncedTranscript>>,
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
        Self::seal_record(
            vault,
            scopes,
            &LinkedEvent {
                event: event.clone(),
                parents: parents.to_vec(),
                transcript: None,
            },
        )
    }

    fn seal_record(
        vault: &Vault,
        scopes: SyncScopes,
        record: &LinkedEvent,
    ) -> Result<Self, SyncError> {
        let event = &record.event;
        check_scope(scopes, event.entity_kind)?;
        validate(record)?;
        let encoded = Zeroizing::new(serde_json::to_vec(record).map_err(|_| SyncError::Integrity)?);
        let envelope = vault.seal(&encoded).map_err(|_| SyncError::Integrity)?;
        let key = key(vault, &event.device_id, event.sequence.get(), envelope.id())?;
        Ok(Self { key, envelope })
    }

    fn reopen(vault: &Vault, bytes: Vec<u8>) -> Result<(Self, LinkedEvent), SyncError> {
        let (envelope, plaintext) = vault.reopen(bytes).map_err(|_| SyncError::Integrity)?;
        let record = match serde_json::from_slice(&plaintext).map_err(|_| SyncError::Integrity)? {
            WireEvent::Linked(record) => record,
            WireEvent::Original(event) => LinkedEvent {
                event,
                parents: Vec::new(),
                transcript: None,
            },
        };
        validate(&record)?;
        let event = &record.event;
        let key = key(vault, &event.device_id, event.sequence.get(), envelope.id())?;
        Ok((Self { key, envelope }, record))
    }

    fn download(
        vault: &Vault,
        backend: &dyn SyncBackend,
        key: &ObjectKey,
    ) -> Result<(Self, LinkedEvent), SyncError> {
        let (pending, record) = Self::reopen(vault, backend.get(key)?)?;
        if pending.key() != key {
            return Err(SyncError::Integrity);
        }
        Ok((pending, record))
    }

    pub fn key(&self) -> &ObjectKey {
        &self.key
    }

    pub fn publish(&self, backend: &dyn SyncBackend) -> Result<(), SyncError> {
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
    backend: &dyn SyncBackend,
    key: &ObjectKey,
) -> Result<SyncEvent, SyncError> {
    Ok(read_linked(vault, backend, key)?.event)
}

fn read_linked(
    vault: &Vault,
    backend: &dyn SyncBackend,
    key: &ObjectKey,
) -> Result<LinkedEvent, SyncError> {
    Ok(PendingEvent::download(vault, backend, key)?.1)
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
    let root = generation_path(vault);
    ObjectKey::new(format!("{root}/devices/{}", String::from(device.clone())))
}

fn generation_path(vault: &Vault) -> String {
    format!("voiceledger/{}/g-{}", hex(vault.id()), vault.generation())
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
    if let Some(text) = &record.transcript {
        let revision = &text.revision;
        if event.entity_kind != SyncEntity::UtteranceRevision
            || event.operation != SyncOperation::Upsert
            || event.entity_id != revision.utterance_id
            || event.revision_id != revision.id
            || !event.object_refs.is_empty()
            || revision.parent_revision_ids != record.parents
            || !revision.is_durable()
            || (revision.kind == UtteranceKind::Manual
                && (revision.parent_revision_ids.is_empty() || revision.model.is_some()))
            || (revision.kind == UtteranceKind::Final && revision.model.is_none())
            || revision
                .confidence
                .is_some_and(|confidence| !(0.0..=1.0).contains(&confidence))
            || revision.text.trim().is_empty()
            || revision.text.len() > 8192
            || revision.text.contains('\0')
            || !revision.range.is_nonempty()
            || text.session.as_ref().is_some_and(|session| {
                session.created_at_ms.get() > 253_402_300_799_999
                    || session.elapsed_ns < revision.range.end_ns
                    || session.title.as_ref().is_some_and(|title| {
                        title.chars().count() > 64 || title.chars().any(char::is_control)
                    })
            })
            || revision.source.is_empty()
            || revision.source.len() > 1024
            || revision
                .source
                .iter()
                .any(|span| !span.range.is_nonempty() || !revision.range.contains(span.range))
        {
            return Err(SyncError::Integrity);
        }
    }
    Ok(())
}
