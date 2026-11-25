# Vault envelope review draft

The vault module uses AES-256-GCM and HKDF-SHA256 through the locked ring dependency. A vault has a random 16-byte ID, a 32-byte random secret and a positive generation. Rotation keeps the vault ID and generates a new secret under the next generation. Each seal operation draws a new object ID and a 96-bit nonce from the operating system. Retries reuse the original sealed bytes. [Issue #15](https://github.com/sagehou/VoiceLedger/issues/15)

| Offset | Bytes | Field |
| ---: | ---: | --- |
| 0 | 4 | ASCII `VLO1` |
| 4 | 4 | Generation as an unsigned big-endian integer |
| 8 | 16 | Random object ID with UUIDv4 version and variant bits |
| 24 | 12 | AES-GCM nonce |
| 36 | variable | Encrypted payload followed by a 16-byte tag |

HKDF uses the vault ID as salt and the vault secret as input key material. Its info is ASCII `VoiceLedger/object-key/v1`, the generation bytes and object ID. Associated data is ASCII `VoiceLedger/object/v1`, the vault ID and the complete 36-byte header. The decrypting caller supplies the expected object ID from its local event or request. Maximum plaintext size is 16 MiB. Event type, metadata, names and contents belong inside the encrypted payload.

Recovery codes use `vl1_` followed by unpadded Base64URL for the vault ID, generation, secret and six checksum bytes. The checksum is the leading six bytes of SHA-256 over ASCII `VoiceLedger/recovery/v1` and the preceding fields. It catches transcription errors. Successful authenticated decryption confirms possession of the correct recovery material. Keep the code outside provider configuration. The native Windows adapter wraps it with current-user DPAPI and the Android adapter uses AES-GCM with an app-scoped Android Keystore key.

The journal ingest layer must enforce the per-device sequence and immutable object ID, and handle authenticated duplicates without reapplying them. Rotation requires sharing the new recovery material with retained devices. Removed devices can retain old keys and already downloaded data. Provider-visible sizes, timing and network metadata remain observable. Local SQLite and audio are outside the scope of the vault envelope.

Sync defaults to disabled. Text is the default scope after setup. Raw audio and profiles require separate opt-ins. The shared vector contains synthetic inputs. Production sync requires cryptographic review and clean-device recovery evidence in #18. [Vault vector](../../contracts/fixtures/vault-v1.json) · [Device acceptance](https://github.com/sagehou/VoiceLedger/issues/18)
