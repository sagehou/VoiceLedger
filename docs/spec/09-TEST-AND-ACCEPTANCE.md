# Test Plan and Acceptance Gates

All thresholds are provisional until measured. Record device model, OS/build, ABI, available RAM, storage, model/runtime/hash, language corpus hash, configuration, duration and evidence path.

## GitHub Actions is the authoritative build gate
- Every compile/build/test validation must run in GitHub Actions; local compile output is not acceptance evidence.
- CI should build Windows WPF on a Windows runner, Android Kotlin/Gradle on a Linux runner with the pinned JDK/Android SDK, and the shared Rust core for Windows x64 plus Android arm64-v8a.
- Run Rust unit tests, C# tests, Kotlin tests, FFI smoke tests, format/lint/static analysis and package smoke tests in Actions.
- Shared-core or contract changes must trigger both Windows and Android integration jobs.
- Upload build logs, test reports, benchmark metadata and package artifacts. Do not upload real user audio, transcripts, voice embeddings, credentials or recovery keys.
- Hardware-in-the-loop acceptance is separate: Actions cannot prove microphone capture, screen-off Android recording, battery drain, thermal behavior or 8-hour stability. Those tests require real devices and evidence linked from issues.

## Gate G0 — Build and architecture
- Windows WPF builds from clean checkout.
- Android Kotlin app builds from clean checkout.
- Shared Rust core builds for Windows x64 and Android arm64; FFI smoke tests run from C# and Kotlin.
- If Rust vertical slice fails, document exact blocker and compare alternatives before changing the shared-core decision.
- Dependency versions and lockfiles committed.

## Gate G1 — Recording durability (tiered)
Windows:
- Verify Mic/System/Both on at least one real Windows device; unit/integration tests cover state transitions and error paths.
- Confirm independent tracks and measure alignment; <100ms is a target, not a blocker for the initial prototype if the measured offset is documented and correctable.
- Exercise device unplug, default change, sleep/resume, silence and low storage.
- Development: short functional recording and roughly 1–2 hour soak. Pre-release: one extended soak (target 8 hours) on a representative supported device.

Android:
- Verify user-started screen-off recording on at least one physical device early; a 15–30 minute smoke test is sufficient for initial integration.
- Development: progress to a 1–2 hour soak on a representative low-resource phone.
- Pre-release: target one extended 8-hour run on a representative supported low-resource phone. Do not require every OEM/model variant to pass an 8-hour run before continuing.
- Verify notification Stop, permission denial/revocation, route changes, audio focus, low storage, low memory, thermal state and process-death recovery as applicable.
- Never auto-reopen microphone after reboot/process death.

Both:
- Crash recovery indexes committed chunks and detects orphan/missing files.
- Bounded writer queue and uncommitted tail-loss window are measured and documented.
- AI failure and sync outage do not block capture.

## Gate G2 — Models and recommendation (evidence-led)
- Compare multilingual Whisper Tiny quantized and SenseVoice INT8 on representative Windows and Android hardware; start with short, reproducible samples.
- Report Chinese CER, English WER, Japanese CER and mixed-language behavior separately. Start with a small manually checked corpus (e.g. 10–20 utterances per language); expand only where results are ambiguous or before a wider release claim.
- Measure cold/warm latency, RTF, peak process memory, CPU/thread count and device thermal/battery effects. Do not require every candidate to meet one universal score.
- For real-time recommendation, RTF below 1 is the basic feasibility signal; <=0.7 is a desirable safety margin, not a universal hard gate. Slower models may still be recommended for deferred/post-session processing.
- Benchmark only when no recording is active. Test runtime degradation with representative low-memory, thermal and backlog conditions.
- If no model is safe or useful on a device, Recording-only is a valid outcome.

## Gate G3 — Session notes, translation and speaker
- Record or import a saved session with more than 40 final entries. Reopen it after restarting both native clients, browse the full chronological note, filter by track, edit a line and export it.
- Generate a summary across multiple transcript batches from the selected service. Verify the send disclosure, persistence after reopening, editing, stale status after a transcript change and preservation of a manual summary when a new draft arrives. Exercise cancellation and provider failure with a saved note.
- Evaluate the language directions selected for MVP; do not block initial recording/transcription on all six directions.
- Use a modest representative set per direction initially (e.g. 10–20 sentences), then expand for release confidence and terminology coverage.
- Translation references the exact source revision and becomes stale after source edits.
- Report speaker DER/JER, false merges/splits and Unknown/reject behavior on representative 2–6 speaker samples. Metrics inform suitability; no arbitrary universal DER/JER threshold is imposed before corpus results exist.
- CAM++ remains a candidate; do not hardcode a similarity threshold without evaluation.
- Human corrections remain authoritative after rerun.

## Gate G4 — Encrypted sync (incremental provider rollout)
- Validate two physical clients against one selected provider (S3-compatible or WebDAV) for the first end-to-end MVP path.
- Keep the second provider adapter in scope, but complete its full compatibility matrix before advertising that provider as supported; it need not block unrelated capture/AI work.
- Exercise offline event creation, reconnect, duplicate upload, pagination, interrupted transfer, conflict and tombstone behavior.
- Cross-platform crypto vectors, wrong-key/tamper rejection and recovery on a clean device are required before enabling real user sync data.
- Verify default sync scope and raw-audio/profile opt-ins.

## Gate G5 — MVP release
- Required capabilities pass on the intentionally small declared supported device matrix; do not claim universal OEM/hardware support.
- Installation, upgrade, SQLite migration, export and basic backup/restore are tested.
- Provide extended recording soak evidence on representative Windows and Android devices; this is not an 8-hour test on every declared device.
- Publish known limitations and untested configurations.
- A feature may be deferred/unsupported with a clear UX state; optional translation directions or enhanced speaker accuracy must not block the entire MVP.
- MVP release requires recording durability, local ASR on a supported tier, saved notes with editable meeting summary generation, and sync recovery on the selected provider.

## Reporting
Every test has ID, status (not_run/pass/fail/blocked), build commit, environment, input/corpus hash, steps, expected/actual, metrics, evidence path, and unresolved risks. Synthetic tests do not replace real-device evidence.
