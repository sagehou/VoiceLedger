# Data and Sync Specification

## Local source of truth
Each device has local SQLite metadata and local audio chunk files. SQLite must never be placed directly on WebDAV/S3 or a network share. Audio is stored per track and per chunk; metadata points to immutable chunk IDs and checksums.

Core entities:
- Session: title, created/start/end, device, state, language policy.
- Track: source type, device endpoint, sample format, timebase/source epoch.
- AudioChunk: sequence, range, codec, channels/rate, bytes, checksum, commit state.
- Gap: track, time range, cause, recoverability.
- UtteranceRevision: track, source chunk/range, time range, language, text, machine/manual revision.
- TranslationRevision: source revision, target language, text, model/version, stale state.
- SpeakerAssignment: session speaker ID, interval, confidence, source model revision.
- SpeakerProfile: explicit user-managed identity and encrypted voice samples/embeddings.
- AiJob: input ranges, model/runtime, state, attempts and error.
- SyncEvent: per-device append sequence, entity ID, operation, revision, object refs.

## Revision rules
- IDs are random stable IDs created offline.
- Partial ASR is transient and not synced by default.
- Final and manual revisions are distinct.
- Machine reruns never overwrite manual edits or confirmed speaker names.
- Translation binds to exact source revision; source edit marks it stale.
- Concurrent edits preserve branches and require user merge.
- Speaker IDs are session-local until user explicitly maps them to a global profile.
- Tombstones prevent old devices from silently resurrecting deleted entities.

## Sync architecture
No VoiceLedger backend. Client connects directly to a user-configured WebDAV or S3-compatible object store.
- Sync disabled by default.
- Each device writes immutable encrypted event objects under its own namespace and monotonic sequence.
- Devices discover other device journals, maintain local watermarks and apply unseen events.
- Objects use random IDs; no shared mutable global index is required.
- Device-specific heads/manifests use conditional writes where available; otherwise immutable revisions and deterministic reconciliation.
- S3 adapter: list/get/put/delete, pagination and optional conditional put.
- WebDAV adapter: PROPFIND/GET/PUT/DELETE/ETag; validate each provider's behavior.
- ETag is not an integrity checksum. Verify authenticated encryption tag and content hash.
- Default sync: encrypted text and event journal. Raw audio backup off by default. Speaker profile sync off by default.
- No automatic remote garbage collection in MVP.

## Encryption and key recovery
Proposed v1 cryptographic envelope: AEAD (AES-256-GCM or XChaCha20-Poly1305 after review), HKDF-SHA256, unique nonce per key, random object IDs, authenticated metadata/version. Exact format and test vectors are frozen only after crypto review and cross-platform interoperability tests.

- Device protects wrapped vault key with DPAPI (Windows) or Android Keystore.
- Recovery material is user-held and must be tested before production sync.
- Never log plaintext, voice, embeddings, credentials or recovery secrets.
- E2EE sync does not automatically encrypt local SQLite/audio at rest; specify local-at-rest protection separately.
- Removing a device cannot erase data/key material it already obtained. Revocation requires key rotation/new vault generation and re-sharing with trusted devices.
- Store metadata such as object size, access time and IP may remain observable.

## Sync state and conflict behavior
States: NotConfigured, Configured, Idle, Syncing, Offline, WaitingKey, AuthenticationRequired, ProviderUnsupported, QuotaExceeded, ConflictNeedsReview, RetryScheduled.
Retries use backoff. Recording never waits for sync. Conflict resolution preserves both revisions. Audio availability and transcript sync status are displayed separately.

## Provider compatibility
Before vault setup, test a private random prefix using only test objects. Verify list pagination, PUT, GET, overwrite/conditional semantics, DELETE, redirects, auth, large object handling and retries. Never point initial compatibility tests at an existing user media directory.
