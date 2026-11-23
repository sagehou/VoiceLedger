# UX Flows and Native Interaction Specification

## Windows information architecture
1. **Recording** — primary start/stop surface, source selectors, per-track meters/status, duration, disk reserve, audio commit indicator, AI state, transcript lag.
2. **Sessions** — searchable history, session status, track availability, transcript/translation completeness, sync state.
3. **Session detail** — timeline, track filter, playback, transcript revisions, speaker assignments, translations, export/delete.
4. **Models & device** — hardware profile, model catalog, download/install/remove, benchmark history, recommendation and mode override.
5. **People** — local speaker profiles, consent, sample management, matching review.
6. **Sync & privacy** — WebDAV/S3 config, test, vault setup/join, key recovery, sync scope, device list.
7. **Settings & diagnostics** — language, storage, logging, export, diagnostics.

WPF uses native navigation and a persistent recording status surface. Recording host may continue when main window is minimized/closed to tray; explicit Stop remains accessible from tray. Closing the window must not silently stop or silently continue without a clear first-run choice. Default close behavior while recording: prompt with Continue in background / Stop and close / Cancel.

## Android information architecture
1. **Record** — prominent Start/Stop, source status, elapsed recording time, persistent FGS notification, AI status.
2. **Sessions** — local history and sync state.
3. **Session detail** — timeline, playback, transcript edit, translation and speaker correction.
4. **Models** — device capability and model recommendation.
5. **Settings** — sync, storage, language, privacy, diagnostic export.

Android recording service must not depend on Activity lifetime. Navigation away or screen-off cannot imply Stop. System notification always indicates active microphone capture and offers Stop. If notification Stop is tapped, perform the same idempotent Stop transition as in-app Stop.

## First-run flow
1. Explain microphone capture and local storage.
2. Request microphone permission only when the user initiates setup/start.
3. Detect device capabilities without uploading hardware identifiers.
4. Show recording-only mode as immediately usable.
5. Show model recommendations as candidates, with download size and measured/estimated labels.
6. User chooses model and downloads; verify hash and runtime compatibility.
7. Run benchmark only when idle and user-approved.
8. Present final recommendation with language accuracy status, RTF, peak memory and test provenance.
9. Sync setup is optional and separate. Do not require account or cloud setup to record.

## Recording flow
- User selects Mic/System/Both and physical endpoints before Start.
- Preparing shows permission, device, and storage checks.
- Recording shows actual track states and meters, not a cosmetic timer alone.
- Pause/Resume preserves time gaps and starts new chunks.
- Mark button inserts a timestamped marker.
- Stop asks for confirmation only if there is an unresolved destructive choice; normal Stop is immediate and idempotent.
- After Stop, show audio saved/partial/missing separately from AI pending/completed and sync pending/completed.

## Transcript interaction
- Partial lines are visually distinct and not editable.
- Final line can be edited; save creates a manual revision.
- Rerun ASR opens a comparison or revision history; user explicitly accepts replacement.
- Filter by Mic/System track; show track chip on every line.
- Translation appears below or beside source text, with target language label and stale indicator.
- Speaker labels can be renamed within session; global profile creation is a separate explicit action.

## Model recommendation screen
For every candidate show:
- Compatibility: compatible / incompatible / unknown.
- Model and runtime version, language support, download size, license.
- Peak RAM: estimate vs measured; never conflate model file size with runtime memory.
- Cold start, warm RTF, final latency, CPU/threads, thermal/battery notes.
- Benchmark device and timestamp.
- Recommendation explanation and mode: recording-only/lightweight/balanced/enhanced.
- Download, cancel, remove, re-test, manual selection.

If no candidate passes safe loading or quality gates, recommend Recording only. Manual choice cannot bypass ABI or memory safety hard gates.

## Error and degraded states
- Permission denied/revoked.
- Source disconnected / one track unavailable.
- Storage low/full.
- Audio writer lag/failure.
- Model absent/corrupt/incompatible.
- Low memory / thermal / battery saver.
- AI backlog, partial delay, failed job.
- Sync offline / auth expired / provider incompatible / quota exceeded / conflict / key unavailable.
Each state includes: what still works, what is paused, what data is safe, and next action. Avoid generic “Something went wrong”.
