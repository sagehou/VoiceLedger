# Test Plan and Acceptance Gates

All thresholds are provisional until measured. Record device model, OS/build, ABI, available RAM, storage, model/runtime/hash, language corpus hash, configuration, duration and evidence path.

## Gate G0 — Build and architecture
- Windows WPF builds from clean checkout.
- Android Kotlin app builds from clean checkout.
- Shared Rust core builds for Windows x64 and Android arm64; FFI smoke tests run from C# and Kotlin.
- If Rust vertical slice fails, document exact blocker and compare alternatives before changing the shared-core decision.
- Dependency versions and lockfiles committed.

## Gate G1 — Recording durability
Windows:
- Mic/System/Both work with real endpoints.
- Both tracks retain independent samples and timebase; alignment target <100ms.
- Device unplug, default change, sleep/resume, silence and disk exhaustion tested.
- 8-hour test has no unexplained application-level missing chunks.

Android:
- User-started screen-off microphone capture for 8 hours on real target devices.
- FGS notification persists and Stop action works.
- Permission revocation, route changes, audio focus, low storage, low memory, thermal state and process death tested.
- Never auto-reopen microphone after reboot/process death.

Both:
- Crash recovery indexes committed chunks and detects orphan/missing files.
- Bounded writer queue and measured uncommitted tail-loss window.
- AI failure and sync outage do not block capture.

## Gate G2 — Models and recommendation
- Whisper Tiny multilingual quantized vs SenseVoice INT8 benchmark per platform.
- Chinese CER, English WER, Japanese CER, mixed-language error profile.
- Cold/warm latency, RTF, peak process-tree memory, CPU/thread count, thermal/battery impact.
- Windows dual-track total throughput.
- No recording active during benchmark.
- Candidate must pass hard compatibility, memory safety and language quality before real-time recommendation.
- Runtime governor tested with injected low memory, thermal pressure and backlog.
- If no model passes, Recording-only is valid and visible.

## Gate G3 — Translation and speaker
- Six directed language pairs evaluated separately.
- Translation references exact source revision and marks stale after edit.
- Speaker diarization DER/JER, false merge/split and Unknown/reject behavior measured on 2–6 speaker meetings.
- CAM++ is a candidate; no fixed similarity threshold accepted without corpus evidence.
- Human corrections remain authoritative after rerun.

## Gate G4 — Encrypted sync
- Two physical clients, one S3-compatible provider and one WebDAV provider.
- Offline event creation, reconnect, duplicate upload, pagination, partial upload, conflict and tombstone tests.
- Cross-platform crypto vectors, wrong key and tamper rejection, recovery on clean device.
- Default sync scope and audio/profile opt-ins verified.
- No production user data until crypto review and recovery gate pass.

## Gate G5 — MVP release
- G1–G4 required capabilities pass on declared supported device matrix.
- Installation, upgrade, SQLite migration, backup/restore, export and diagnostics tested.
- 8-hour recording evidence for each declared device class.
- Known limitations and non-supported configurations published.
- No claim of full MVP while any mandatory capability remains blocked or untested.

## Reporting
Every test has ID, status (not_run/pass/fail/blocked), build commit, environment, input/corpus hash, steps, expected/actual, metrics, evidence path, and unresolved risks. Synthetic tests do not replace real-device evidence.
