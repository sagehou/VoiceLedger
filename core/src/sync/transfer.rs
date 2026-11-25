use std::collections::BTreeSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use super::{
    Journal, JournalStore, ObjectKey, Outbox, PutOutcome, SyncBackend, SyncError, SyncedTranscript,
    generation_path,
};
use crate::contracts::{StableId, SyncEntity, SyncEvent, SyncOperation};
use crate::vault::{SyncScopes, Vault};

const TEXT_ONLY: SyncScopes = SyncScopes {
    enabled: true,
    text: true,
    raw_audio: false,
    speaker_profiles: false,
};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Retirement {
    removed: StableId,
    successor: u32,
    seed_id: [u8; 16],
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Membership {
    device: StableId,
}

pub struct VaultSync<'a> {
    vault: &'a Vault,
    backend: &'a dyn SyncBackend,
    device: StableId,
    baseline: u64,
    inbox: JournalStore,
    outbox: Outbox,
}

impl<'a> VaultSync<'a> {
    pub fn verify_key(
        vault: &'a Vault,
        backend: &'a dyn SyncBackend,
        device: &StableId,
    ) -> Result<(), SyncError> {
        if let Some(seed) = Self::active_seed(vault, backend, device)? {
            Journal::from_checkpoint(vault, &seed)?;
        }
        Ok(())
    }

    fn active_seed(
        vault: &Vault,
        backend: &dyn SyncBackend,
        device: &StableId,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SyncError> {
        if get_record::<Retirement>(vault, backend, &control(vault, "retirement")?)?.is_some() {
            return Err(SyncError::GenerationRetired);
        }
        if vault.generation() == 1 {
            return Ok(None);
        }
        let (_, removed) = get_record::<StableId>(vault, backend, &control(vault, "removed")?)?
            .ok_or(SyncError::Integrity)?;
        if &removed == device {
            return Err(SyncError::GenerationRetired);
        }
        let bytes = backend.get(&control(vault, "baseline")?)?;
        let (_, checkpoint) = vault.reopen(bytes).map_err(|_| SyncError::Integrity)?;
        Ok(Some(checkpoint))
    }

    pub fn open(
        root: &Path,
        vault: &'a Vault,
        backend: &'a dyn SyncBackend,
        device: StableId,
    ) -> Result<Self, SyncError> {
        let seed = Self::active_seed(vault, backend, &device)?;
        let baseline = if let Some(data) = seed.as_ref() {
            Journal::from_checkpoint(vault, data)?.next_sequence(&device)
        } else {
            0
        };
        let inbox =
            JournalStore::open_with_seed(root, vault, seed.as_ref().map(|bytes| bytes.as_slice()))?;
        let outbox = Outbox::open(root, vault, device.clone())?;
        Ok(Self {
            vault,
            backend,
            device,
            baseline,
            inbox,
            outbox,
        })
    }

    pub fn journal(&self) -> &Journal {
        self.inbox.journal()
    }

    pub fn devices(&self) -> Result<Vec<StableId>, SyncError> {
        let directory = control(self.vault, "members")?;
        let mut devices = BTreeSet::new();
        walk(self.backend, &directory, |key| {
            let device = get_record::<Membership>(self.vault, self.backend, key)?
                .ok_or(SyncError::Retry)?
                .1
                .device;
            if member(self.vault, &device)? != *key {
                return Err(SyncError::Integrity);
            }
            devices.insert(device);
            Ok(())
        })?;
        Ok(devices.into_iter().collect())
    }

    pub fn pull(&mut self) -> Result<(), SyncError> {
        let record = Membership {
            device: self.device.clone(),
        };
        let key = member(self.vault, &self.device)?;
        if get_record::<Membership>(self.vault, self.backend, &key)?.is_none() {
            put_record(self.vault, self.backend, &key, &record)?;
        }
        let devices = self.devices()?;
        for device in devices {
            let prefix = super::device_prefix(self.vault, &device)?;
            let mut keys = BTreeSet::new();
            walk(self.backend, &prefix, |key| {
                keys.insert(key.clone());
                Ok(())
            })?;
            for key in keys {
                self.inbox.receive(self.vault, self.backend, &key)?;
            }
        }
        Ok(())
    }

    pub fn stage_text(
        &mut self,
        revisions: impl IntoIterator<Item = SyncedTranscript>,
        mut new_id: impl FnMut() -> Result<StableId, SyncError>,
    ) -> Result<(), SyncError> {
        let (mut sequence, mut queued) = self.outbox.inventory(self.vault, self.baseline)?;
        for item in revisions {
            let revision = &item.revision;
            if self.journal().has_revision(&revision.id) || !queued.insert(revision.id.clone()) {
                continue;
            }
            let event = SyncEvent {
                id: new_id()?,
                device_id: self.device.clone(),
                sequence: i64::try_from(sequence)
                    .ok()
                    .and_then(|value| value.try_into().ok())
                    .ok_or(SyncError::LocalStorage)?,
                entity_kind: SyncEntity::UtteranceRevision,
                entity_id: revision.utterance_id.clone(),
                operation: SyncOperation::Upsert,
                restored_from_tombstone_id: None,
                revision_id: revision.id.clone(),
                object_refs: Vec::new(),
                created_at_unix_ms: now()?,
            };
            self.outbox
                .stage_transcript(self.vault, TEXT_ONLY, &event, &item)?;
            sequence += 1;
        }
        Ok(())
    }

    pub fn publish(&mut self) -> Result<(), SyncError> {
        while let Some(key) = self
            .outbox
            .publish_next(self.vault, TEXT_ONLY, self.backend)?
        {
            self.inbox.receive(self.vault, self.backend, &key)?;
        }
        Ok(())
    }

    /// Publish the successor's encrypted baseline, then retire this generation.
    pub fn remove(&self, next: &Vault, target: &StableId) -> Result<(), SyncError> {
        if &self.device == target || !self.devices()?.contains(target) {
            return Err(SyncError::Configuration);
        }
        check_successor(self.vault, next)?;
        let baseline = control(next, "baseline")?;
        let seed_id = match self.backend.get(&baseline) {
            Ok(data) => {
                let (object, checkpoint) = next.reopen(data).map_err(|_| SyncError::Conflict)?;
                Journal::from_checkpoint(next, &checkpoint)?;
                *object.id()
            }
            Err(SyncError::NotFound) => {
                let checkpoint = Zeroizing::new(self.inbox.seed(next)?);
                let seed = next.seal(&checkpoint).map_err(|_| SyncError::Integrity)?;
                if self.backend.put_immutable(&baseline, &seed)? != PutOutcome::Created {
                    return Err(SyncError::Conflict);
                }
                *seed.id()
            }
            Err(error) => return Err(error),
        };
        put_record(next, self.backend, &control(next, "removed")?, target)?;
        put_record(
            self.vault,
            self.backend,
            &control(self.vault, "retirement")?,
            &Retirement {
                removed: target.clone(),
                successor: next.generation(),
                seed_id,
            },
        )
    }

    pub fn finished_rotation(
        vault: &Vault,
        next: &Vault,
        backend: &dyn SyncBackend,
        target: &StableId,
    ) -> Result<bool, SyncError> {
        check_successor(vault, next)?;
        let Some((_, retired)) =
            get_record::<Retirement>(vault, backend, &control(vault, "retirement")?)?
        else {
            return Ok(false);
        };
        if retired.successor != next.generation() || &retired.removed != target {
            return Err(SyncError::Conflict);
        }
        let bytes = backend.get(&control(next, "baseline")?)?;
        next.open(&retired.seed_id, &bytes)
            .map_err(|_| SyncError::Integrity)?;
        let (_, removed) = get_record::<StableId>(next, backend, &control(next, "removed")?)?
            .ok_or(SyncError::Integrity)?;
        if &removed != target {
            return Err(SyncError::Integrity);
        }
        Ok(true)
    }
}

fn now() -> Result<crate::contracts::Count, SyncError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .and_then(|value| value.try_into().ok())
        .ok_or(SyncError::LocalStorage)
}

fn check_successor(current: &Vault, next: &Vault) -> Result<(), SyncError> {
    if current.id() != next.id() || current.generation().checked_add(1) != Some(next.generation()) {
        return Err(SyncError::Configuration);
    }
    Ok(())
}

fn walk(
    backend: &dyn SyncBackend,
    prefix: &ObjectKey,
    mut visit: impl FnMut(&ObjectKey) -> Result<(), SyncError>,
) -> Result<(), SyncError> {
    let directory = format!("{}/", prefix.value());
    let mut continuation = None;
    let mut tokens = BTreeSet::new();
    loop {
        let page = backend.list(prefix, continuation.as_deref())?;
        for key in page.keys {
            if key
                .value()
                .strip_prefix(&directory)
                .is_none_or(|name| name.is_empty() || name.contains('/'))
            {
                return Err(SyncError::Integrity);
            }
            visit(&key)?;
        }
        let Some(token) = page.continuation else {
            return Ok(());
        };
        if !tokens.insert(token.clone()) {
            return Err(SyncError::ProviderUnsupported);
        }
        continuation = Some(token);
    }
}

fn member(vault: &Vault, device: &StableId) -> Result<ObjectKey, SyncError> {
    control(vault, &format!("members/{}", String::from(device.clone())))
}

fn control(vault: &Vault, name: &str) -> Result<ObjectKey, SyncError> {
    ObjectKey::new(format!("{}/{name}", generation_path(vault)))
}

fn get_record<T: DeserializeOwned>(
    vault: &Vault,
    backend: &dyn SyncBackend,
    key: &ObjectKey,
) -> Result<Option<([u8; 16], T)>, SyncError> {
    let bytes = match backend.get(key) {
        Ok(value) => value,
        Err(SyncError::NotFound) => return Ok(None),
        Err(error) => return Err(error),
    };
    let (object, clear) = vault.reopen(bytes).map_err(|_| SyncError::Integrity)?;
    let (path, record): (String, T) =
        serde_json::from_slice(&clear).map_err(|_| SyncError::Integrity)?;
    if path != key.value() {
        return Err(SyncError::Integrity);
    }
    Ok(Some((*object.id(), record)))
}

fn put_record<T: Serialize>(
    vault: &Vault,
    backend: &dyn SyncBackend,
    key: &ObjectKey,
    record: &T,
) -> Result<(), SyncError> {
    // Bind the purpose and remote path into the authenticated plaintext.
    let record = Zeroizing::new(
        serde_json::to_vec(&(key.value(), record)).map_err(|_| SyncError::Integrity)?,
    );
    let object = vault.seal(&record).map_err(|_| SyncError::Integrity)?;
    match backend.put_immutable(key, &object)? {
        PutOutcome::Created => Ok(()),
        PutOutcome::Exists => {
            let (_, saved) = vault
                .reopen(backend.get(key)?)
                .map_err(|_| SyncError::Integrity)?;
            if *saved == *record {
                Ok(())
            } else {
                Err(SyncError::Conflict)
            }
        }
    }
}
