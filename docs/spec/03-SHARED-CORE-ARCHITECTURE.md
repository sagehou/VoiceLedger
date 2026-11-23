# Shared Core and Platform Architecture

## Decision
Keep native presentation and OS integration:
- Windows: WPF/C#.
- Android: Kotlin/Jetpack Compose.

Build one shared business core, with Rust as the preferred implementation candidate. It is not yet a proven decision: first deliver a thin vertical slice for Windows P/Invoke and Android JNI, compare build complexity, memory, crash behavior and debugging. If Rust cross-target cost is disproportionate, keep language-neutral contracts and revisit.

## Shared core responsibilities
- Domain entities and stable IDs.
- Session and track state machines.
- Chunk manifest and recovery reconciliation rules.
- Utterance/translation/speaker revision semantics.
- Hardware capability normalization and model recommendation scoring.
- AI job scheduling policy and resource governor policy.
- Model catalog validation and benchmark result interpretation.
- Sync event format, conflict semantics, tombstones and merge policy.
- Cryptographic envelope/protocol orchestration, subject to security review.
- Export serialization and shared validation.

## Platform adapter responsibilities
Windows:
- WASAPI capture/loopback and endpoint enumeration.
- QPC/endpoint clock sampling and source epoch management.
- WPF UI, tray, hotkeys, Windows notifications.
- DPAPI credential wrapping, filesystem paths, process management.
- .NET runtime integration and native library loading.

Android:
- AudioRecord, microphone FGS, permission lifecycle, notification actions.
- Android clock/timebase mapping, audio route events.
- Compose UI, lifecycle and system back handling.
- Keystore key wrapping, app-private storage, WorkManager for deferred sync.
- JNI loading and Android ABI packaging.

## Proposed module map
- `voiceledger-core`: pure domain and policies; no OS calls.
- `voiceledger-ffi`: stable C ABI, versioned handles, opaque contexts.
- `voiceledger-audio-contracts`: platform-neutral audio frame/chunk metadata.
- `voiceledger-models`: model catalog, compatibility, recommendation and benchmark types.
- `voiceledger-ai-scheduler`: durable jobs and governor decisions.
- `voiceledger-sync`: event journal, object manifest and conflict rules.
- `voiceledger-crypto`: cryptographic primitives/wrappers only after independent review.
- Windows adapters: C# projects for WASAPI, UI, filesystem and DPAPI.
- Android adapters: Kotlin modules for AudioRecord, FGS, Room/SQLite, UI and Keystore.

## FFI contract rules
- Never call across FFI for each sample. Pass bounded batches or opaque buffer handles.
- Version every public struct/API; avoid ABI-unstable Rust types crossing the boundary.
- Use explicit ownership, release functions, cancellation tokens and structured error codes.
- Do not expose Rust panics across FFI; catch and convert.
- Keep callbacks short and avoid callbacks from arbitrary Rust threads into UI frameworks.
- No async runtime may be assumed to exist in host process; define who owns executor/threading.
- Audio capture must continue if AI FFI module crashes or unloads.

## Shared database ownership
Use one logical schema and migration contract. Platform implementations may use different SQLite bindings, but must pass identical migration and behavior tests. Prefer shared-core repository functions for domain invariants; native UI can query read models. Avoid two independent business repositories that drift.
Room may be used for Android persistence plumbing, but must not own divergent business semantics.

## Cross-platform conformance
- Same synthetic fixture suite for session transitions, revision conflicts, model recommendation, sync event validation and crypto envelope.
- Rust unit tests plus C# and Kotlin integration tests.
- Golden test vectors must be consumed by both platform clients.
- Test at least one end-to-end recording → persisted chunk → transcript revision → encrypted object → second device import path.
