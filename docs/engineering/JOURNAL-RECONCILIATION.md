# Journal reconciliation

Each device stream starts at sequence zero. An authenticated event ahead of the next sequence waits for the gap to close. The index ignores an exact duplicate and rejects a different event in an occupied sequence. Checkpoints bind to the vault ID and key generation. [Issue #17](https://github.com/sagehou/VoiceLedger/issues/17)

Encrypted revision events carry parent IDs. A revision waits for its parents, and independent revisions remain separate heads. A resolution references both heads. A tombstone permanently hides the old entity ID. Restore uses a fresh ID referencing the tombstone, even when the restore arrives first. Older vault generations cannot be imported into the current index.

The Actions tests exercise a synthetic second device with the recovery code, out-of-order and duplicate events, two divergent edits, conflict resolution, tombstone replay and generation changes. The index exposes a checkpoint for integration with local storage. Native device join and removal controls, durable app integration and physical two-device/provider evidence remain open. [Vault](VAULT-PROTOCOL.md) · [Device acceptance](https://github.com/sagehou/VoiceLedger/issues/18)
