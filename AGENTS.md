# VoiceLedger — Agent Instructions

## Read first
1. `docs/spec/00-PRODUCT-OVERVIEW.md`
2. `docs/spec/01-FUNCTIONAL-SPEC.md`
3. `docs/spec/02-UX-FLOWS.md`
4. `docs/spec/03-SHARED-CORE-ARCHITECTURE.md`
5. Platform specs, AI/model spec, data/sync/security specs, then acceptance spec.

## Non-negotiable product constraints
- Windows UI remains native WPF; Android UI remains native Kotlin/Jetpack Compose.
- Prefer one shared business core. Rust is the proposed implementation language for domain state, model recommendation, AI scheduling, data semantics, and sync protocol. Platform APIs remain native adapters.
- No dedicated VoiceLedger backend, inference server, database server, Docker, Kubernetes, or public AI fallback is required.
- Audio must be persisted locally before any AI or sync side effect. Recording must not block on model inference, network, UI, or sync.
- User manually starts recording. Recording continues in the background until the user stops it, subject to OS permissions and lifecycle limits. Never auto-open the microphone after reboot or process death.
- Windows supports selectable Mic, System loopback, and Both, with separate tracks.
- Android records microphone audio through a user-started microphone foreground service and continues while screen is off.
- Local AI is hardware-adaptive. Recording-only mode must always remain available. Never auto-load a model that fails compatibility or memory safety checks.
- Optional sync uses client-side authenticated encryption over WebDAV or S3-compatible object storage. Cloud storage is untrusted and receives ciphertext only.
- No transcript, audio, speaker embedding, credential, or recovery key in ordinary logs.
- Never overwrite human transcript edits, confirmed speaker names, or human translations with machine reruns.

## Engineering rules
- Keep audio capture callbacks short and allocation-light. Use bounded queues and explicit backpressure.
- Use stable IDs, source track/time references, revisions, checksums, and crash recovery.
- Preserve microphone and loopback source identity. Do not destructively deduplicate tracks.
- Model files, runtime compatibility, licenses, SHA-256, peak memory, RTF, and benchmark provenance must be catalogued.
- Treat model sizes and performance thresholds in specs as candidates/targets until measured.
- Do not claim a hardware tier is supported based on emulator/unit tests alone.
- Keep platform-specific code behind narrow interfaces. Avoid per-sample FFI calls; batch audio frames/chunks.
- Every deviation requires an ADR with rationale, impact, migration, and rollback.
- Do not silently broaden scope into summaries, cloud AI, team collaboration, or generic call recording.

## Delivery
- Build and test both clients independently.
- Provide reproducible commands, dependency locks, tests, and evidence.
- Mark real-device tests as BLOCKED when hardware is unavailable; continue unrelated work.
- Update requirement/test traceability and unresolved risks with each milestone.
