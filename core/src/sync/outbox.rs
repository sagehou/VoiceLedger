use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{
    LinkedEvent, ObjectKey, PendingEvent, SyncBackend, SyncError, SyncedTranscript, check_scope,
    device_prefix,
};
use crate::contracts::{StableId, SyncEvent};
use crate::vault::{MAX_PLAINTEXT_BYTES, SyncScopes, Vault};

pub struct Outbox {
    directory: PathBuf,
    prefix: ObjectKey,
    device: StableId,
    _lock: File,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Outbox {
    pub fn open(root: &Path, vault: &Vault, device: StableId) -> Result<Self, SyncError> {
        if !root.is_absolute() {
            return Err(SyncError::Configuration);
        }
        let prefix = device_prefix(vault, &device)?;
        let directory = root.join(prefix.value());
        let lock = lock_directory(&directory, "outbox.lock")?;
        Ok(Self {
            directory,
            prefix,
            device,
            _lock: lock,
            failures: 0,
            retry_at: None,
        })
    }

    /// Stage one durable encrypted event. Retries and sent receipts keep the original object ID.
    pub fn stage(
        &mut self,
        vault: &Vault,
        scopes: SyncScopes,
        event: &SyncEvent,
    ) -> Result<ObjectKey, SyncError> {
        self.stage_revision(vault, scopes, event, &[])
    }

    pub fn stage_revision(
        &mut self,
        vault: &Vault,
        scopes: SyncScopes,
        event: &SyncEvent,
        parents: &[StableId],
    ) -> Result<ObjectKey, SyncError> {
        self.stage_record(
            vault,
            scopes,
            LinkedEvent {
                event: event.clone(),
                parents: parents.to_vec(),
                transcript: None,
            },
        )
    }

    pub fn stage_transcript(
        &mut self,
        vault: &Vault,
        scopes: SyncScopes,
        event: &SyncEvent,
        transcript: &SyncedTranscript,
    ) -> Result<ObjectKey, SyncError> {
        self.stage_record(
            vault,
            scopes,
            LinkedEvent {
                event: event.clone(),
                parents: transcript.revision.parent_revision_ids.clone(),
                transcript: Some(transcript.clone()),
            },
        )
    }

    fn stage_record(
        &mut self,
        vault: &Vault,
        scopes: SyncScopes,
        record: LinkedEvent,
    ) -> Result<ObjectKey, SyncError> {
        let event = &record.event;
        self.check_vault(vault)?;
        check_scope(scopes, event.entity_kind)?;
        if event.device_id != self.device {
            return Err(SyncError::Configuration);
        }
        let sequence = event.sequence.get();
        for state in ["sent", "pending"] {
            if let Some((saved, prior)) = self.load(vault, sequence, state)? {
                return if prior == record {
                    Ok(saved.key().clone())
                } else {
                    Err(SyncError::Conflict)
                };
            }
        }
        let pending = PendingEvent::seal_record(vault, scopes, &record)?;
        write_entry(
            &self.directory,
            &self.path(sequence, "pending"),
            pending.envelope.envelope(),
        )?;
        Ok(pending.key().clone())
    }

    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_at
            .and_then(|at| at.checked_duration_since(Instant::now()))
    }

    /// Publish at most one staged event on a worker. A transport failure leaves the exact bytes queued.
    pub fn publish_next(
        &mut self,
        vault: &Vault,
        scopes: SyncScopes,
        backend: &impl SyncBackend,
    ) -> Result<Option<ObjectKey>, SyncError> {
        self.check_vault(vault)?;
        if !scopes.enabled {
            return Err(SyncError::Scope);
        }
        if self.retry_after().is_some() {
            return Err(SyncError::Retry);
        }
        let mut first = None;
        for item in fs::read_dir(&self.directory).map_err(storage)? {
            let path = item.map_err(storage)?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("pending") {
                continue;
            }
            let Some(sequence) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| value.len() == 20)
                .and_then(|value| value.parse::<u64>().ok())
            else {
                return Err(SyncError::Integrity);
            };
            first = Some(first.map_or(sequence, |prior: u64| prior.min(sequence)));
        }
        let Some(sequence) = first else {
            return Ok(None);
        };
        let (pending, event) = self
            .load(vault, sequence, "pending")?
            .ok_or(SyncError::LocalStorage)?;
        check_scope(scopes, event.event.entity_kind)?;
        match pending.publish(backend) {
            Ok(()) => {
                install(
                    &self.directory,
                    &self.path(sequence, "pending"),
                    &self.path(sequence, "sent"),
                )?;
                self.failures = 0;
                self.retry_at = None;
                Ok(Some(pending.key().clone()))
            }
            Err(error @ (SyncError::Retry | SyncError::Transport)) => {
                self.failures = self.failures.saturating_add(1);
                let seconds = (1_u64 << self.failures.min(9)).min(300);
                self.retry_at = Some(Instant::now() + Duration::from_secs(seconds));
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn check_vault(&self, vault: &Vault) -> Result<(), SyncError> {
        if device_prefix(vault, &self.device)? != self.prefix {
            return Err(SyncError::Configuration);
        }
        Ok(())
    }

    fn path(&self, sequence: u64, state: &str) -> PathBuf {
        self.directory.join(format!("{sequence:020}.{state}"))
    }

    fn load(
        &self,
        vault: &Vault,
        sequence: u64,
        state: &str,
    ) -> Result<Option<(PendingEvent, LinkedEvent)>, SyncError> {
        let file = match File::open(self.path(sequence, state)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage(error)),
        };
        let (pending, record) = PendingEvent::reopen(vault, read_entry(file)?)?;
        if record.event.device_id != self.device || record.event.sequence.get() != sequence {
            return Err(SyncError::Integrity);
        }
        Ok(Some((pending, record)))
    }
}

pub(super) fn lock_directory(directory: &Path, name: &str) -> Result<File, SyncError> {
    fs::create_dir_all(directory).map_err(storage)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(name))
        .map_err(storage)?;
    file.try_lock().map_err(|_| SyncError::Retry)?;
    Ok(file)
}

pub(super) fn read_entry(file: File) -> Result<Vec<u8>, SyncError> {
    let mut bytes = Vec::new();
    file.take((MAX_PLAINTEXT_BYTES + 65) as u64)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    Ok(bytes)
}

pub(super) fn write_entry(directory: &Path, to: &Path, bytes: &[u8]) -> Result<(), SyncError> {
    let staging = directory.join("staging");
    let mut file = File::create(&staging).map_err(storage)?;
    file.write_all(bytes).map_err(storage)?;
    file.sync_all().map_err(storage)?;
    drop(file);
    install(directory, &staging, to)
}

fn install(_directory: &Path, from: &Path, to: &Path) -> Result<(), SyncError> {
    if to.try_exists().map_err(storage)? {
        return Err(SyncError::Integrity);
    }
    fs::rename(from, to).map_err(storage)?;
    #[cfg(unix)]
    File::open(_directory)
        .and_then(|directory| directory.sync_all())
        .map_err(storage)?;
    Ok(())
}

pub(super) fn storage(_: std::io::Error) -> SyncError {
    SyncError::LocalStorage
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::sync::{Page, PutOutcome, hex, read_event};
    use crate::vault::SealedObject;

    type Stored = Option<(ObjectKey, Vec<u8>)>;

    #[derive(Default)]
    struct Provider(RefCell<Stored>);

    impl SyncBackend for Provider {
        fn put_immutable(
            &self,
            key: &ObjectKey,
            body: &SealedObject,
        ) -> Result<PutOutcome, SyncError> {
            let mut stored = self.0.borrow_mut();
            if let Some((prior_key, bytes)) = &*stored {
                assert_eq!(key, prior_key);
                assert_eq!(body.envelope(), bytes);
                return Ok(PutOutcome::Exists);
            }
            *stored = Some((key.clone(), body.envelope().to_vec()));
            // The provider stored the body before the connection dropped.
            Err(SyncError::Transport)
        }

        fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, SyncError> {
            self.0
                .borrow()
                .as_ref()
                .filter(|(saved, _)| saved == key)
                .map(|(_, bytes)| bytes.clone())
                .ok_or(SyncError::NotFound)
        }

        fn list(&self, _: &ObjectKey, _: Option<&str>) -> Result<Page, SyncError> {
            unreachable!()
        }

        fn delete(&self, _: &ObjectKey) -> Result<(), SyncError> {
            unreachable!()
        }
    }

    #[test]
    fn reconnect_preserves_event_ciphertext_and_recovers_the_sent_receipt() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../contracts/fixtures/domain-v1.json"))
                .unwrap();
        let event: SyncEvent = serde_json::from_value(fixture["sync_events"][0].clone()).unwrap();
        let vault = Vault::create().unwrap();
        let root = std::env::temp_dir().join(format!("voiceledger-outbox-{}", hex(vault.id())));
        let scopes = SyncScopes {
            enabled: true,
            raw_audio: true,
            ..SyncScopes::default()
        };
        let provider = Provider::default();
        let mut outbox = Outbox::open(&root, &vault, event.device_id.clone()).unwrap();
        let key = outbox.stage(&vault, scopes, &event).unwrap();
        let mut collision = event.clone();
        collision.id = event.entity_id.clone();
        assert_eq!(
            outbox.stage(&vault, scopes, &collision),
            Err(SyncError::Conflict)
        );
        assert_eq!(
            outbox.publish_next(&vault, scopes, &provider),
            Err(SyncError::Transport)
        );
        assert!(outbox.retry_after().is_some());
        assert_eq!(
            outbox.publish_next(&vault, scopes, &provider),
            Err(SyncError::Retry)
        );
        drop(outbox);

        let restored = Vault::recover(&vault.recovery_code()).unwrap();
        let mut outbox = Outbox::open(&root, &restored, event.device_id.clone()).unwrap();
        assert_eq!(outbox.stage(&restored, scopes, &event).unwrap(), key);
        assert_eq!(
            outbox.publish_next(&restored, SyncScopes::default(), &provider),
            Err(SyncError::Scope)
        );
        assert_eq!(
            outbox.publish_next(&restored, scopes, &provider).unwrap(),
            Some(key.clone())
        );
        assert_eq!(outbox.stage(&restored, scopes, &event).unwrap(), key);
        assert_eq!(
            outbox.publish_next(&restored, scopes, &provider).unwrap(),
            None
        );
        assert_eq!(read_event(&restored, &provider, &key).unwrap(), event);
        drop(outbox);
        fs::remove_dir_all(root).unwrap();
    }
}
