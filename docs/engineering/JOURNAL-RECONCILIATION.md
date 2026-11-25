# Journal reconciliation

Each device stream starts at sequence zero. An authenticated event ahead of the next sequence waits for the gap to close. The index ignores an exact duplicate and rejects a different event in an occupied sequence. Checkpoints bind to the vault ID and key generation. [Issue #17](https://github.com/sagehou/VoiceLedger/issues/17)

Encrypted revision events carry parent IDs. A revision waits for its parents, and independent revisions remain separate heads. A resolution references both heads. A tombstone permanently hides the old entity ID. Restore uses a fresh ID referencing the tombstone, even when the restore arrives first. Older vault generations cannot be imported into the current index.

The Actions tests exercise a synthetic second device with the recovery code, out-of-order and duplicate events, two divergent edits, conflict resolution, tombstone replay and generation changes. The local journal stores verified envelopes before advancing its in-memory index. Restart rebuilds the same per-device watermarks and conflict heads from those envelopes. The two-device synthetic test uploads through the outbox and verifies that a receiver can restart while a parent is missing. Native device join, import and conflict resolution are available in the integrated candidate. Physical two-device and provider evidence is tracked in #18. [Vault](VAULT-PROTOCOL.md) · [Device acceptance](https://github.com/sagehou/VoiceLedger/issues/18)

A text journal entry can contain the complete durable utterance revision with its session, source track, interval, model provenance and parents, all inside the authenticated envelope. Partial text is rejected. A machine descendant keeps its user-confirmed parent visible until an explicit manual resolution references the competing heads. The text recovery case uses independent sender and receiver directories and contains synthetic revisions only. [Issue #17](https://github.com/sagehou/VoiceLedger/issues/17)


## Windows and Android

Choose a provider under **Sync & privacy** and run **Check provider** against the intended configuration. Create the vault key on the first device and import the recovery key on the other device. **Sync text** exchanges committed transcript revisions and the accompanying session and track metadata. Sessions available only as text show audio availability separately. Provider credentials remain in the current screen. Audio, translations and speaker data stay local in this transfer path.

When both devices edit the same line offline, **Sync text** retrieves both branches. In **Sessions**, open the returning session and use **Resolve conflict** to save a new manual revision with both branches as parents. A stale editor must reload if a third revision arrives. Earlier revisions remain stored. The next sync publishes the resolution.

The device list is built from authenticated vault membership. Select the other device, then confirm removal. VoiceLedger publishes an encrypted baseline for the successor generation and retires the old one. The new key stays wrapped with DPAPI or Android Keystore during the operation. **Resume device removal** reuses that key if the operation was interrupted. On trusted devices, use **Join updated vault** to verify and install the new recovery key.

The successor baseline includes the synchronized revisions and tombstones at the rotation boundary. Synchronize trusted devices before rotation to capture their pending changes. Old namespace data remains encrypted with the old key. Device removal cannot erase earlier copies, and a provider can withhold journal or retirement objects. If the baseline exceeds the envelope size limit, rotation stops before retirement. [Vault design](VAULT-PROTOCOL.md) · [Issue #18](https://github.com/sagehou/VoiceLedger/issues/18)
