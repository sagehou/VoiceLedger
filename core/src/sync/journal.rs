use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::outbox::{lock_directory, read_entry, storage, write_entry};
use super::{
    LinkedEvent, ObjectKey, PendingEvent, SyncBackend, SyncError, SyncedTranscript, device_prefix,
    hex, read_linked, validate,
};
use crate::contracts::{StableId, SyncOperation, UtteranceKind};
use crate::vault::{MAX_PLAINTEXT_BYTES, Vault};

const MAX_AHEAD: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receive {
    Duplicate,
    Waiting { next: u64 },
    Advanced { next: u64 },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EntityState {
    pub heads: Vec<StableId>,
    pub tombstoned: bool,
    pub waiting_for_parents: usize,
}

#[derive(Default)]
struct Stream {
    next: u64,
    records: BTreeMap<u64, StableId>,
}

pub struct Journal {
    vault_id: [u8; 16],
    generation: u32,
    streams: BTreeMap<StableId, Stream>,
    events: BTreeMap<StableId, LinkedEvent>,
    revisions: BTreeMap<StableId, StableId>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint<T> {
    version: u32,
    vault_id: [u8; 16],
    generation: u32,
    records: Vec<T>,
}

impl Journal {
    pub fn new(vault: &Vault) -> Self {
        Self {
            vault_id: *vault.id(),
            generation: vault.generation(),
            streams: BTreeMap::new(),
            events: BTreeMap::new(),
            revisions: BTreeMap::new(),
        }
    }

    pub fn receive(
        &mut self,
        vault: &Vault,
        backend: &impl SyncBackend,
        key: &ObjectKey,
    ) -> Result<Receive, SyncError> {
        self.check_vault(vault)?;
        self.apply(read_linked(vault, backend, key)?)
    }

    fn check_vault(&self, vault: &Vault) -> Result<(), SyncError> {
        if self.vault_id == *vault.id() && self.generation == vault.generation() {
            Ok(())
        } else {
            Err(SyncError::Integrity)
        }
    }

    fn admit(&self, record: &LinkedEvent) -> Result<bool, SyncError> {
        validate(record)?;
        let event = &record.event;
        if let Some(prior) = self.events.get(&event.id) {
            return if prior == record {
                Ok(false)
            } else {
                Err(SyncError::Conflict)
            };
        }
        let sequence = event.sequence.get();
        let stream = self.streams.get(&event.device_id);
        if self.revisions.contains_key(&event.revision_id)
            || stream.is_some_and(|stream| stream.records.contains_key(&sequence))
        {
            return Err(SyncError::Conflict);
        }
        if sequence.saturating_sub(stream.map_or(0, |stream| stream.next)) > MAX_AHEAD {
            return Err(SyncError::Retry);
        }
        Ok(true)
    }

    fn apply(&mut self, record: LinkedEvent) -> Result<Receive, SyncError> {
        if !self.admit(&record)? {
            return Ok(Receive::Duplicate);
        }
        let event = &record.event;
        let stream = self.streams.entry(event.device_id.clone()).or_default();
        let sequence = event.sequence.get();
        stream.records.insert(sequence, event.id.clone());
        self.revisions
            .insert(event.revision_id.clone(), event.id.clone());
        self.events.insert(event.id.clone(), record);
        if sequence != stream.next {
            return Ok(Receive::Waiting { next: stream.next });
        }
        while stream.records.contains_key(&stream.next) {
            stream.next += 1;
        }
        Ok(Receive::Advanced { next: stream.next })
    }

    fn accepted(&self) -> impl Iterator<Item = &LinkedEvent> {
        self.streams.values().flat_map(|stream| {
            stream
                .records
                .range(..stream.next)
                .map(|(_, id)| &self.events[id])
        })
    }

    pub fn state(&self, entity: &StableId) -> EntityState {
        let tombstones: BTreeMap<_, _> = self
            .accepted()
            .filter(|item| item.event.operation == SyncOperation::Tombstone)
            .map(|item| (&item.event.id, &item.event))
            .collect();
        if tombstones.values().any(|event| &event.entity_id == entity) {
            return EntityState {
                tombstoned: true,
                ..EntityState::default()
            };
        }
        let mut ready = VecDeque::new();
        let mut dependencies = BTreeMap::new();
        let mut children = BTreeMap::<&StableId, Vec<&StableId>>::new();
        let mut waiting_for_restore = 0;
        for item in self
            .accepted()
            .filter(|item| &item.event.entity_id == entity)
        {
            if item.event.operation == SyncOperation::Restore
                && item
                    .event
                    .restored_from_tombstone_id
                    .as_ref()
                    .and_then(|id| tombstones.get(id))
                    .is_none_or(|tombstone| {
                        tombstone.entity_id == item.event.entity_id
                            || tombstone.entity_kind != item.event.entity_kind
                    })
            {
                waiting_for_restore += 1;
                continue;
            }
            let revision = &item.event.revision_id;
            if let std::collections::btree_map::Entry::Vacant(entry) = dependencies.entry(revision)
            {
                entry.insert(item.parents.as_slice());
                if item.parents.is_empty() {
                    ready.push_back(revision);
                }
                for parent in &item.parents {
                    children.entry(parent).or_default().push(revision);
                }
            }
        }
        let mut counts: BTreeMap<_, _> = dependencies
            .iter()
            .map(|(&revision, parents)| (revision, parents.len()))
            .collect();
        let mut heads = BTreeSet::new();
        let mut applied = 0;
        while let Some(revision) = ready.pop_front() {
            applied += 1;
            heads.insert(revision);
            let manual = self
                .transcript(revision)
                .is_some_and(|item| item.revision.kind == UtteranceKind::Manual);
            for parent in dependencies[revision] {
                let confirmed = self
                    .transcript(parent)
                    .is_some_and(|item| item.revision.kind == UtteranceKind::Manual);
                if manual || !confirmed {
                    heads.remove(parent);
                }
            }
            if let Some(dependents) = children.get(revision) {
                for &child in dependents {
                    let count = counts.get_mut(child).expect("indexed dependency");
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(child);
                    }
                }
            }
        }
        EntityState {
            heads: heads.into_iter().cloned().collect(),
            tombstoned: false,
            waiting_for_parents: dependencies.len() - applied + waiting_for_restore,
        }
    }

    fn transcript(&self, revision: &StableId) -> Option<&SyncedTranscript> {
        self.events
            .get(self.revisions.get(revision)?)?
            .transcript
            .as_deref()
    }

    pub fn transcript_heads(&self, entity: &StableId) -> Vec<&SyncedTranscript> {
        self.state(entity)
            .heads
            .iter()
            .filter_map(|revision| self.transcript(revision))
            .collect()
    }

    pub fn checkpoint(&self) -> Result<Vec<u8>, SyncError> {
        let records = self
            .streams
            .values()
            .flat_map(|stream| stream.records.values().map(|id| &self.events[id]))
            .collect();
        let checkpoint = Checkpoint {
            version: 1,
            vault_id: self.vault_id,
            generation: self.generation,
            records,
        };
        let bytes = serde_json::to_vec(&checkpoint).map_err(|_| SyncError::Integrity)?;
        if bytes.len() > MAX_PLAINTEXT_BYTES {
            return Err(SyncError::Retry);
        }
        Ok(bytes)
    }

    pub fn from_checkpoint(vault: &Vault, data: &[u8]) -> Result<Self, SyncError> {
        if data.len() > MAX_PLAINTEXT_BYTES {
            return Err(SyncError::Integrity);
        }
        let checkpoint: Checkpoint<LinkedEvent> =
            serde_json::from_slice(data).map_err(|_| SyncError::Integrity)?;
        if checkpoint.version != 1
            || checkpoint.vault_id != *vault.id()
            || checkpoint.generation != vault.generation()
        {
            return Err(SyncError::Integrity);
        }
        let mut journal = Self::new(vault);
        for record in checkpoint.records {
            journal.apply(record)?;
        }
        Ok(journal)
    }
}

/// A generation-specific device journal backed by the authenticated envelopes received from the provider.
pub struct JournalStore {
    directory: PathBuf,
    journal: Journal,
    _lock: File,
}

impl JournalStore {
    pub fn open(root: &Path, vault: &Vault) -> Result<Self, SyncError> {
        if !root.is_absolute() {
            return Err(SyncError::Configuration);
        }
        let directory = root
            .join("journal")
            .join(hex(vault.id()))
            .join(vault.generation().to_string());
        let lock = lock_directory(&directory, "journal.lock")?;
        let mut journal = Journal::new(vault);
        let mut files = fs::read_dir(&directory)
            .map_err(storage)?
            .map(|entry| entry.map(|entry| entry.path()).map_err(storage))
            .collect::<Result<Vec<_>, _>>()?;
        files.retain(|path| path.extension().and_then(|item| item.to_str()) == Some("entry"));
        files.sort_unstable();
        for file in files {
            let (_, record) =
                PendingEvent::reopen(vault, read_entry(File::open(&file).map_err(storage)?)?)?;
            if file != entry_path(&directory, &record) {
                return Err(SyncError::Integrity);
            }
            journal.apply(record)?;
        }
        Ok(Self {
            directory,
            journal,
            _lock: lock,
        })
    }

    pub fn state(&self, entity: &StableId) -> EntityState {
        self.journal.state(entity)
    }

    pub fn transcript_heads(&self, entity: &StableId) -> Vec<&SyncedTranscript> {
        self.journal.transcript_heads(entity)
    }

    pub fn receive(
        &mut self,
        vault: &Vault,
        backend: &impl SyncBackend,
        key: &ObjectKey,
    ) -> Result<Receive, SyncError> {
        self.journal.check_vault(vault)?;
        let (pending, record) = PendingEvent::download(vault, backend, key)?;
        if !self.journal.admit(&record)? {
            return Ok(Receive::Duplicate);
        }
        let path = entry_path(&self.directory, &record);
        let envelope = pending.envelope.envelope();
        if path.try_exists().map_err(storage)? {
            if read_entry(File::open(&path).map_err(storage)?)? != envelope {
                return Err(SyncError::Integrity);
            }
        } else {
            write_entry(&self.directory, &path, envelope)?;
        }
        self.journal.apply(record)
    }

    pub fn pull_device_page(
        &mut self,
        vault: &Vault,
        backend: &impl SyncBackend,
        device: &StableId,
        continuation: Option<&str>,
    ) -> Result<Option<String>, SyncError> {
        self.journal.check_vault(vault)?;
        let prefix = device_prefix(vault, device)?;
        let parent = format!("{}/", prefix.value());
        let page = backend.list(&prefix, continuation)?;
        if page.continuation.is_some() && page.continuation.as_deref() == continuation {
            return Err(SyncError::ProviderUnsupported);
        }
        for key in &page.keys {
            if key
                .value()
                .strip_prefix(&parent)
                .is_none_or(|name| name.is_empty() || name.contains('/'))
            {
                return Err(SyncError::Integrity);
            }
            self.receive(vault, backend, key)?;
        }
        Ok(page.continuation)
    }
}

fn entry_path(directory: &Path, record: &LinkedEvent) -> PathBuf {
    let device = String::from(record.event.device_id.clone());
    let sequence = record.event.sequence.get();
    directory.join(format!("{device}-{sequence:020}.entry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{SyncEntity, SyncEvent};
    use crate::sync::{Page, PendingEvent, PutOutcome};
    use crate::vault::{SealedObject, SyncScopes};
    use std::cell::RefCell;

    #[derive(Default)]
    struct Server(RefCell<BTreeMap<ObjectKey, Vec<u8>>>);

    impl SyncBackend for Server {
        fn put_immutable(
            &self,
            key: &ObjectKey,
            body: &SealedObject,
        ) -> Result<PutOutcome, SyncError> {
            if self.0.borrow().contains_key(key) {
                return Ok(PutOutcome::Exists);
            }
            self.0
                .borrow_mut()
                .insert(key.clone(), body.envelope().to_vec());
            Ok(PutOutcome::Created)
        }

        fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, SyncError> {
            self.0.borrow().get(key).cloned().ok_or(SyncError::NotFound)
        }

        fn list(&self, prefix: &ObjectKey, _: Option<&str>) -> Result<Page, SyncError> {
            let keys = self
                .0
                .borrow()
                .keys()
                .filter(|key| key.value().starts_with(prefix.value()))
                .cloned()
                .collect();
            Ok(Page {
                keys,
                continuation: None,
            })
        }

        fn delete(&self, key: &ObjectKey) -> Result<(), SyncError> {
            self.0.borrow_mut().remove(key);
            Ok(())
        }
    }

    fn id(n: u32) -> StableId {
        format!("00000000-0000-4000-8000-{n:012}")
            .try_into()
            .unwrap()
    }

    fn event(device: u32, sequence: i64, index: u32, entity: u32) -> SyncEvent {
        SyncEvent {
            id: id(index),
            device_id: id(device),
            sequence: sequence.try_into().unwrap(),
            entity_kind: SyncEntity::UtteranceRevision,
            entity_id: id(entity),
            operation: SyncOperation::Upsert,
            restored_from_tombstone_id: None,
            revision_id: id(index + 100),
            object_refs: Vec::new(),
            created_at_unix_ms: 1i64.try_into().unwrap(),
        }
    }

    #[test]
    fn clean_device_recovers_conflicts_and_tombstones_after_reordered_uploads() {
        let vault = Vault::create().unwrap();
        let other = Vault::recover(&vault.recovery_code()).unwrap();
        let scopes = SyncScopes {
            enabled: true,
            ..SyncScopes::default()
        };
        let server = Server::default();
        let root = event(1, 0, 10, 99);
        let left = event(1, 1, 11, 99);
        let right = event(2, 0, 12, 99);
        let merge = event(2, 1, 13, 99);
        let publish = |event: &SyncEvent, parents: &[StableId]| {
            let pending = PendingEvent::seal_revision(&vault, scopes, event, parents).unwrap();
            pending.publish(&server).unwrap();
            pending.key().clone()
        };
        let root_key = publish(&root, &[]);
        let left_key = publish(&left, std::slice::from_ref(&root.revision_id));
        let right_key = publish(&right, std::slice::from_ref(&root.revision_id));
        let mut journal = Journal::new(&other);
        assert_eq!(
            journal.receive(&other, &server, &left_key),
            Ok(Receive::Waiting { next: 0 })
        );
        assert_eq!(
            journal.receive(&other, &server, &right_key),
            Ok(Receive::Advanced { next: 1 })
        );
        assert_eq!(journal.state(&id(99)).waiting_for_parents, 1);
        let bytes = journal.checkpoint().unwrap();
        let mut journal = Journal::from_checkpoint(&other, &bytes).unwrap();
        assert_eq!(
            journal.receive(&other, &server, &root_key),
            Ok(Receive::Advanced { next: 2 })
        );
        assert_eq!(
            journal.receive(&other, &server, &left_key),
            Ok(Receive::Duplicate)
        );
        assert_eq!(
            journal.state(&id(99)).heads,
            vec![left.revision_id.clone(), right.revision_id.clone()]
        );
        let merge_key = publish(&merge, &[left.revision_id, right.revision_id]);
        journal.receive(&other, &server, &merge_key).unwrap();
        assert_eq!(journal.state(&id(99)).heads, vec![merge.revision_id]);

        let mut deleted = event(1, 2, 15, 99);
        deleted.operation = SyncOperation::Tombstone;
        let mut restored = event(2, 2, 16, 100);
        restored.operation = SyncOperation::Restore;
        restored.restored_from_tombstone_id = Some(deleted.id.clone());
        journal
            .receive(&other, &server, &publish(&restored, &[]))
            .unwrap();
        assert_eq!(journal.state(&id(100)).waiting_for_parents, 1);
        journal
            .receive(&other, &server, &publish(&deleted, &[]))
            .unwrap();
        assert_eq!(journal.state(&id(100)).heads, vec![restored.revision_id]);
        let stale = event(3, 0, 17, 99);
        journal
            .receive(&other, &server, &publish(&stale, &[]))
            .unwrap();
        assert!(journal.state(&id(99)).tombstoned);
        assert!(journal.state(&id(99)).heads.is_empty());

        let forged_sequence = event(3, 0, 18, 99);
        assert_eq!(
            journal.receive(&other, &server, &publish(&forged_sequence, &[])),
            Err(SyncError::Conflict)
        );
        let rotated = vault.rotate().unwrap();
        assert_eq!(
            journal.receive(&rotated, &server, &root_key),
            Err(SyncError::Integrity)
        );
        assert!(Journal::from_checkpoint(&rotated, &bytes).is_err());
        assert_eq!(server.0.borrow().len(), 8);
    }

    #[test]
    fn durable_receive_keeps_offline_branches_across_restart() {
        let vault = Vault::create().unwrap();
        let second = Vault::recover(&vault.recovery_code()).unwrap();
        let root = std::env::temp_dir().join(format!("voiceledger-journal-{}", hex(vault.id())));
        let scopes = SyncScopes {
            enabled: true,
            ..SyncScopes::default()
        };
        let server = Server::default();

        let ancestor = event(1, 0, 30, 20);
        let first = event(1, 1, 31, 20);
        let other = event(2, 0, 32, 20);
        let parents = [ancestor.revision_id.clone()];
        {
            let mut outgoing =
                crate::sync::Outbox::open(&root, &vault, ancestor.device_id.clone()).unwrap();
            outgoing.stage(&vault, scopes, &ancestor).unwrap();
            outgoing
                .stage_revision(&vault, scopes, &first, &parents)
                .unwrap();
            assert_eq!(
                outgoing.stage_revision(&vault, scopes, &first, &[]),
                Err(SyncError::Conflict)
            );
            outgoing.publish_next(&vault, scopes, &server).unwrap();
            outgoing.publish_next(&vault, scopes, &server).unwrap();
        }
        let pending = PendingEvent::seal_revision(&vault, scopes, &other, &parents).unwrap();
        pending.publish(&server).unwrap();
        {
            let mut journal = JournalStore::open(&root, &second).unwrap();
            assert_eq!(
                journal
                    .pull_device_page(&second, &server, &other.device_id, None)
                    .unwrap(),
                None
            );
            assert_eq!(journal.state(&other.entity_id).waiting_for_parents, 1);
        }
        let mut journal = JournalStore::open(&root, &second).unwrap();
        assert_eq!(
            journal.receive(&second, &server, pending.key()),
            Ok(Receive::Duplicate)
        );
        journal
            .pull_device_page(&second, &server, &first.device_id, None)
            .unwrap();
        let expected = vec![first.revision_id, other.revision_id];
        assert_eq!(journal.state(&other.entity_id).heads, expected);
        drop(journal);
        let journal = JournalStore::open(&root, &second).unwrap();
        assert_eq!(journal.state(&other.entity_id).heads, expected);
        drop(journal);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn encrypted_transcripts_restore_both_manual_branches_and_a_resolution() {
        use crate::contracts::{DomainBatch, Source};
        use crate::sync::Outbox;

        let batch: DomainBatch =
            serde_json::from_str(include_str!("../../../contracts/fixtures/domain-v1.json"))
                .unwrap();
        let mut lines = batch.utterance_revisions.into_iter();
        let partial = lines.next().unwrap();
        let original = lines.next().unwrap();
        let manual = lines.next().unwrap();
        let mut machine = lines.next().unwrap();
        machine.parent_revision_ids = vec![manual.id.clone()];
        let concurrent = lines.next().unwrap();
        let session_id = batch.sessions[0].id.clone();
        let entity = original.utterance_id.clone();
        let vault = Vault::create().unwrap();
        let reader = Vault::recover(&vault.recovery_code()).unwrap();
        let name = hex(vault.id());
        let sending = std::env::temp_dir().join(format!("voiceledger-text-send-{name}"));
        let receiving = std::env::temp_dir().join(format!("voiceledger-text-receive-{name}"));
        let scopes = SyncScopes {
            enabled: true,
            ..SyncScopes::default()
        };
        let server = Server::default();
        let stage = |device, sequence, revision: &crate::contracts::UtteranceRevision| {
            let mut record = event(device, sequence, 10 * device + sequence as u32, 1);
            record.entity_id = entity.clone();
            record.revision_id = revision.id.clone();
            let mut outbox = Outbox::open(&sending, &vault, record.device_id.clone()).unwrap();
            outbox.stage_transcript(
                &vault,
                scopes,
                &record,
                &SyncedTranscript {
                    session_id: session_id.clone(),
                    source: Source::Mic,
                    revision: revision.clone(),
                },
            )?;
            outbox
                .publish_next(&vault, scopes, &server)?
                .ok_or(SyncError::LocalStorage)
        };
        assert_eq!(stage(3, 0, &partial), Err(SyncError::Integrity));
        let original_key = stage(1, 0, &original).unwrap();
        let manual_key = stage(1, 1, &manual).unwrap();
        let machine_key = stage(1, 2, &machine).unwrap();
        let concurrent_key = stage(2, 0, &concurrent).unwrap();
        assert!(server.0.borrow().values().all(|bytes| {
            !bytes
                .windows(manual.text.len())
                .any(|slice| slice == manual.text.as_bytes())
        }));
        let mut journal = JournalStore::open(&receiving, &reader).unwrap();
        for key in [&concurrent_key, &machine_key, &manual_key, &original_key] {
            journal.receive(&reader, &server, key).unwrap();
        }
        drop(journal);
        let mut journal = JournalStore::open(&receiving, &reader).unwrap();
        let recovered = journal.transcript_heads(&entity);
        assert_eq!(
            recovered
                .iter()
                .map(|item| item.revision.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                manual.text.as_str(),
                machine.text.as_str(),
                concurrent.text.as_str()
            ]
        );
        assert!(
            recovered
                .iter()
                .all(|item| item.session_id == session_id && item.source == Source::Mic)
        );

        let mut resolution = manual.clone();
        resolution.id = id(900);
        resolution.revision = 4.try_into().unwrap();
        resolution.parent_revision_ids = journal.state(&entity).heads;
        resolution.text = "Synthetic reviewed transcript".into();
        let resolved_key = stage(2, 1, &resolution).unwrap();
        journal.receive(&reader, &server, &resolved_key).unwrap();
        let recovered = journal.transcript_heads(&entity);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].revision.text, resolution.text);
        drop(journal);
        fs::remove_dir_all(sending).unwrap();
        fs::remove_dir_all(receiving).unwrap();
    }
}
