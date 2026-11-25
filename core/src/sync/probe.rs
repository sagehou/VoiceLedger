use serde::Deserialize;
use zeroize::Zeroize;

use super::{ObjectKey, PutOutcome, S3, SyncBackend, SyncError, WebDav};
use crate::vault::Vault;

const SAMPLE: &[u8] = b"VoiceLedger provider compatibility check";

#[derive(Deserialize)]
#[serde(transparent)]
struct Secret(String);

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
enum Connection {
    Webdav {
        endpoint: String,
        name: String,
        password: Secret,
    },
    S3 {
        endpoint: String,
        bucket: String,
        region: String,
        access_key: Secret,
        secret_key: Secret,
        session_token: Option<Secret>,
    },
}

/// Test one configured endpoint with encrypted synthetic data under a new private prefix.
pub fn check_provider(data: &[u8]) -> Result<(), SyncError> {
    if data.len() > 32 * 1024 {
        return Err(SyncError::Configuration);
    }
    let settings: Connection =
        serde_json::from_slice(data).map_err(|_| SyncError::Configuration)?;
    match settings {
        Connection::Webdav {
            endpoint,
            name,
            password,
        } => verify(&WebDav::new(&endpoint, &name, &password.0)?),
        Connection::S3 {
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            session_token,
        } => verify(&S3::new(
            &endpoint,
            &bucket,
            &region,
            &access_key.0,
            &secret_key.0,
            session_token.as_ref().map(|token| token.0.as_str()),
        )?),
    }
}

fn verify(backend: &impl SyncBackend) -> Result<(), SyncError> {
    let vault = Vault::create().map_err(|_| SyncError::Integrity)?;
    let namespace = format!("voiceledger-probe/{}", super::hex(vault.id()));
    let prefix = ObjectKey::new(namespace.clone())?;
    let initial = backend.list(&prefix, None)?;
    if !initial.keys.is_empty() || initial.continuation.is_some() {
        return Err(SyncError::Conflict);
    }

    let object = vault.seal(SAMPLE).map_err(|_| SyncError::Integrity)?;
    let key = ObjectKey::new(format!("{namespace}/{}", super::hex(object.id())))?;
    if backend.put_immutable(&key, &object)? != PutOutcome::Created {
        return Err(SyncError::Conflict);
    }
    let verify_upload = || {
        let replacement = vault
            .seal(b"synthetic replacement")
            .map_err(|_| SyncError::Integrity)?;
        if backend.put_immutable(&key, &replacement)? != PutOutcome::Exists {
            return Err(SyncError::ProviderUnsupported);
        }
        let listing = backend.list(&prefix, None)?;
        if listing.keys.as_slice() != std::slice::from_ref(&key) || listing.continuation.is_some() {
            return Err(SyncError::ProviderUnsupported);
        }
        let stored = backend.get(&key)?;
        let data = vault
            .open(object.id(), &stored)
            .map_err(|_| SyncError::Integrity)?;
        if data.as_slice() != SAMPLE {
            return Err(SyncError::Integrity);
        }
        Ok(())
    };
    let result = verify_upload();
    let deleted = backend.delete(&key);
    result.and(deleted)?;
    match backend.get(&key) {
        Err(SyncError::NotFound) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(SyncError::ProviderUnsupported),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::sync::Page;
    use crate::vault::SealedObject;

    struct Fixture {
        overwrite: bool,
        data: RefCell<BTreeMap<ObjectKey, Vec<u8>>>,
    }

    impl SyncBackend for Fixture {
        fn put_immutable(
            &self,
            key: &ObjectKey,
            body: &SealedObject,
        ) -> Result<PutOutcome, SyncError> {
            let mut objects = self.data.borrow_mut();
            if !self.overwrite && objects.contains_key(key) {
                return Ok(PutOutcome::Exists);
            }
            assert_ne!(body.envelope(), SAMPLE);
            objects.insert(key.clone(), body.envelope().to_vec());
            Ok(PutOutcome::Created)
        }

        fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, SyncError> {
            self.data
                .borrow()
                .get(key)
                .cloned()
                .ok_or(SyncError::NotFound)
        }

        fn list(&self, prefix: &ObjectKey, _: Option<&str>) -> Result<Page, SyncError> {
            let path = format!("{}/", prefix.value());
            Ok(Page {
                keys: self
                    .data
                    .borrow()
                    .keys()
                    .filter(|item| item.value().starts_with(&path))
                    .cloned()
                    .collect(),
                continuation: None,
            })
        }

        fn delete(&self, key: &ObjectKey) -> Result<(), SyncError> {
            self.data.borrow_mut().remove(key);
            Ok(())
        }
    }

    #[test]
    fn probe_tests_conditional_writes_and_cleans_only_the_test_object() {
        let existing = ObjectKey::new("unrelated/existing".into()).unwrap();
        for overwrite in [false, true] {
            let fixture = Fixture {
                overwrite,
                data: RefCell::new(BTreeMap::from([(existing.clone(), b"unrelated".to_vec())])),
            };
            let expected = if overwrite {
                Err(SyncError::ProviderUnsupported)
            } else {
                Ok(())
            };
            assert_eq!(verify(&fixture), expected);
            assert_eq!(
                fixture.data.borrow().keys().collect::<Vec<_>>(),
                vec![&existing]
            );
        }
    }
}
