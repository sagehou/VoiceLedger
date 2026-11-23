# Android Client Specification

## Stack
Kotlin, Jetpack Compose, AudioRecord, microphone Foreground Service, SQLite/Room persistence. minSdk 26 is an initial candidate only; confirm dependencies and target devices before freezing.

## Recording lifecycle
- User starts recording from visible Activity.
- Validate RECORD_AUDIO and storage availability.
- Start microphone foreground service with correct service type and notification.
- AudioRecord lifecycle belongs to the service, not Activity.
- Screen-off, navigation away and UI process recreation must not stop recording.
- Notification Stop action invokes idempotent stop and seals active chunks.
- No boot auto-start or hidden background microphone activation.
- After process death/reboot, recover committed chunks and mark Interrupted; never automatically reopen microphone.

## Platform constraints
- Follow Android background FGS start restrictions and while-in-use microphone permission rules.
- Declare microphone foreground service type and required foreground service permissions for target SDK.
- Handle Android 12+ background FGS restrictions and Android 14+ microphone FGS checks.
- Test vendor-specific battery optimization behavior; do not claim universal immunity from process killing.
- WorkManager handles deferred sync/maintenance only, not microphone capture.
- Use PARTIAL_WAKE_LOCK only if P0 demonstrates it is required; measure battery impact and release correctly.

## Resource-aware AI
- No model required for recording.
- Query ActivityManager memory state, low-RAM device status, ABI, thermal status, power saver and battery.
- Use onTrimMemory and thermal callbacks to reduce AI work before capture.
- ASR and other AI work must not block AudioRecord read/write path.
- Android process PSS, service lifetime, battery drain and thermal throttling are release measurements.

## Acceptance
- Real-device user-started screen-off recording for 8 hours.
- Notification remains visible and Stop works.
- Permission revocation, audio route changes, phone call/audio focus, low storage, low memory, thermal pressure and process death tested.
- AI model unload/failure does not stop recording.
