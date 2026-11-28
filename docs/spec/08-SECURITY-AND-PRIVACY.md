# Security and Privacy Requirements

## Threat model
Assume WebDAV/S3 storage is untrusted and may read, replace, replay, reorder, or delete stored objects. Protect content confidentiality and authenticity. Do not claim to hide traffic patterns, object sizes, access times, IP addresses or directory structure unless separately mitigated.

## Data classes
- Original audio: sensitive personal/meeting data.
- Transcript, translation and session summary: sensitive content.
- Speaker embedding/profile: identity-linked biometric-like data; separate opt-in.
- Device profile/benchmark: potentially identifying metadata; keep local unless user explicitly exports.
- Credentials/recovery keys: secrets; never log or sync in plaintext.

## Controls
- TLS to storage provider.
- Client-side authenticated encryption before upload.
- Unique nonce per AEAD key; authenticated envelope version and metadata.
- Key wrapping with OS keystore; offline recovery material.
- Separate sync toggles for text, raw audio and speaker profiles.
- Sync disabled by default; raw audio and speaker profile sync disabled by default.
- Diagnostic logs redact text, audio, embeddings, keys, tokens and endpoint credentials.
- Explicit user consent before registering a speaker profile.
- Export warns that plaintext export is outside vault encryption.
- Local at-rest encryption must be separately specified; E2EE upload alone does not encrypt local database or files.

## Operational boundaries
- Provider requests originate from an explicit send action that displays the service address and the type of data sent. Summary generation sends saved transcript text. Provider API keys are used in memory for the current request.
- No telemetry containing content or stable device fingerprint.
- Device revocation cannot erase already copied plaintext or old keys; document key rotation and vault migration semantics.
- Deletion uses tombstones; remote physical purge is not promised in MVP.
- Crypto implementation requires review and cross-platform vectors before production data.

## Security gates
- C#/Kotlin clients produce identical encrypted envelope test vectors for same inputs.
- Tampered ciphertext, nonce reuse, wrong vault key, replay and malformed envelope are rejected.
- Recovery works on a clean second device with only user recovery material.
- Logs and crash reports contain no prohibited data.
- Device removal and key rotation behavior is documented and tested.
