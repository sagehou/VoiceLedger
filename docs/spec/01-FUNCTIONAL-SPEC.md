# Functional Specification

## 1. Session lifecycle and manual recording
Recording is never automatic. The user explicitly selects sources and presses Start. Once started, the recording host/service continues when the main UI is minimized or the screen turns off. It remains active until the user presses Stop or the OS/device interrupts it. After reboot or process death, never reopen the microphone automatically.

States: Idle → Preparing → Recording ↔ Paused → Stopping → Completed; abnormal termination produces Interrupted. All transitions have timestamps and reason codes.

- Preparing validates permissions, source availability, writable storage, and free-space reserve.
- Recording writes original source tracks to local rolling segments. AI is an asynchronous bounded side channel.
- Pause stops capture and seals current segments; resume creates new segments while preserving wall-clock gaps.
- Stop is idempotent, stops capture, drains bounded buffers, seals files, and commits indexes. AI may continue afterwards.
- Interrupted recovery indexes committed segments, identifies incomplete tails and gaps, and offers explicit user action to start a new segment/session.

## 2. Windows capture
Source modes:
- Mic: user-selected capture endpoint.
- System: user-selected render endpoint WASAPI loopback.
- Both: two independent tracks with shared session timebase.

Requirements:
- Enumerate endpoints, show default/device state/format, allow source selection before starting.
- Both tracks preserve independent samples, timestamps, source metadata, and status.
- Never destructively mix tracks or silently switch physical devices.
- If one track disconnects, the other may continue; mark a Gap for the affected track and notify the user.
- If the only track disconnects, pause/interrupt the session safely.
- Loopback may produce no callbacks during silence; represent silence using the track clock, not invented audio samples.
- Reconnecting to the same endpoint requires explicit user confirmation and a new source epoch.
- System loopback is system mix, not per-remote-participant isolation.

## 3. Android capture
- User starts recording from a visible Activity after microphone permission is granted.
- Use AudioRecord inside a microphone foreground service; persistent notification includes Stop action.
- Declare required microphone foreground service type and permissions for target SDK.
- Continue capture while screen is off; UI may leave foreground.
- WorkManager is for deferred sync/maintenance, not microphone capture.
- Handle permission revocation, audio focus changes, route changes, storage exhaustion, thermal pressure, low memory, and process death.
- No boot auto-start, hidden recording, or promise to bypass vendor battery management.
- Real-device 8-hour test is a release gate.

## 4. Audio durability
- One logical track per source.
- Audio chunk target ≤10 seconds, configurable after P0 measurements.
- Write to private temp file; seal, validate length/format, flush, atomic rename, then commit DB manifest and processing job.
- Recovery reconciles orphan files and missing files referenced by DB.
- Every chunk has stable ID, sequence, time range, codec, sample rate, channels, byte size, checksum, and commit state.
- Distinguish Silence from Gap. A Gap represents missing/unknown capture, never synthetic silence.
- Audio capture and writer have bounded queues. On sustained write failure or disk exhaustion, stop affected capture and report exact loss boundary; do not claim success.
- Original audio is authoritative. ASR-normalized PCM is a temporary derived stream.

## 5. Transcription
- Languages: Chinese, English, Japanese; mixed-language accuracy is measured separately.
- Provide session language policy: Auto or explicit language selection.
- Model output includes utterance ID, track, start/end time, language, confidence where available, model/runtime version, and revision.
- Partial text is ephemeral and replaceable; do not sync or export partial by default.
- Final text is persisted. Audio persistence status is separate: pending, committed, missing.
- Human edits create manual revisions and remain authoritative over machine reruns.
- Rerun creates a new machine candidate and never silently overwrites manual edits.
- Windows Both mode retains source track on each utterance. Combined timeline is a presentation view only.

## 6. Translation
- Disabled by default.
- One target language per session in MVP.
- Translate stable final utterances only; do not translate every partial update.
- Translation revision references exact source utterance revision, target language, model/version, and timestamp.
- When source text changes, mark translation stale; keep human translation revisions.
- Support the six directed language pairs among zh/en/ja only after independent evaluation.
- No silent external translation provider or cloud fallback.

## 7. Speaker diarization and identity
- Separate diarization (anonymous turns) from identity (known profile/name).
- Default labels are session-local Speaker 1, Speaker 2, Unknown.
- CAM++ embedding + clustering is a candidate, not a guaranteed implementation choice.
- Short, overlapping, noisy utterances may remain Unknown.
- User can rename a speaker in the current session without creating a cross-session identity profile.
- Cross-session voice profile creation is explicit and requires user consent and clean samples.
- Machine identity is a suggestion; user confirmation is required.
- Reanalysis creates new assignments/revisions; confirmed names remain protected.

## 8. Model and hardware management
- First launch works with no model and no sync configuration.
- Detect OS/ABI/CPU capabilities, total and available RAM, low-memory state, storage, and platform-specific thermal/power state.
- Candidate selection has hard compatibility gates before scoring.
- User sees estimated model download size, compatibility, expected language coverage, benchmark status, and recommendation reason.
- Model download is user-approved, resumable, checksum-verified, license-attributed, and cancellable.
- Benchmark only when no recording is active and device is not in low-memory/thermal/power-constrained state.
- Distinguish pre-download estimate, short benchmark, sustained qualification, and 8-hour qualification.
- Modes: Recording only, Lightweight AI, Balanced AI, Enhanced AI. The app must explain that unsupported quality/performance is not promised.
- Runtime governor pauses translation/speaker first, then reduces partial cadence, defers ASR, and unloads AI if needed. Recording is never stopped merely to preserve AI.
- Never upgrade to a heavier model during active recording.

## 9. History, playback, search, export
- History sorted by creation time; show duration, device, tracks, transcript status, sync status, and audio availability.
- Search local titles, final text, manual edits, confirmed names. Pending AI content is clearly excluded.
- Playback selects track and time range; source labels remain visible.
- Export UTF-8 Markdown/TXT and structured JSON; export audio per track. SRT/VTT only after timing gate passes.
- Exported plaintext is outside automatic sync encryption and must be disclosed.

## 10. Sync settings
- Sync disabled by default.
- Configure WebDAV or S3-compatible endpoint and test a private random prefix before vault initialization.
- Default sync scope: encrypted text/results/event journal; raw audio backup disabled; speaker profile sync disabled.
- Show endpoint, last successful sync, queued objects, errors, key availability, device identity, and backup scope.
- Local recording remains functional with invalid credentials, provider outage, or no sync configuration.

## 11. Deletion and retention
- No automatic retention cleanup in MVP.
- Remove local audio is distinct from moving a session to synchronized trash.
- If audio has no verified backup, clearly warn that local removal deletes the only audio copy.
- Trash creates tombstone events. Other devices hide the item; restoration creates a new entity linked to the tombstone.
- MVP does not claim immediate remote physical erasure. Explain that encrypted remote objects may remain until safe garbage collection is implemented.
