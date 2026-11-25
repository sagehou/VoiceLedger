use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::{LinkedEvent, ObjectKey, SyncBackend, SyncError, read_linked, validate};
use crate::contracts::{StableId, SyncOperation};
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
    revisions: BTreeSet<StableId>,
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
            revisions: BTreeSet::new(),
        }
    }

    pub fn receive(
        &mut self,
        vault: &Vault,
        backend: &impl SyncBackend,
        key: &ObjectKey,
    ) -> Result<Receive, SyncError> {
        if self.vault_id != *vault.id() || self.generation != vault.generation() {
            return Err(SyncError::Integrity);
        }
        self.apply(read_linked(vault, backend, key)?)
    }

    fn apply(&mut self, record: LinkedEvent) -> Result<Receive, SyncError> {
        validate(&record)?;
        let event = &record.event;
        if let Some(prior) = self.events.get(&event.id) {
            return if prior == &record {
                Ok(Receive::Duplicate)
            } else {
                Err(SyncError::Conflict)
            };
        }
        if self.revisions.contains(&event.revision_id) {
            return Err(SyncError::Conflict);
        }
        let stream = self.streams.entry(event.device_id.clone()).or_default();
        let sequence = event.sequence.get();
        if stream.records.contains_key(&sequence) {
            return Err(SyncError::Conflict);
        }
        if sequence.saturating_sub(stream.next) > MAX_AHEAD {
            return Err(SyncError::Retry);
        }
        stream.records.insert(sequence, event.id.clone());
        self.revisions.insert(event.revision_id.clone());
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
            for parent in dependencies[revision] {
                heads.remove(parent);
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
}
