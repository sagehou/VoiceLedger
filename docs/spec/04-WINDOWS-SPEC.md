# Windows Client Specification

## Stack
C# / .NET 10 LTS / WPF / MVVM. NAudio WASAPI capture is an initial candidate. Lock exact SDK and package versions during engineering bootstrap.

## Capture
- Enumerate capture endpoints and render endpoints.
- Mic uses selected capture endpoint.
- System uses selected render endpoint loopback.
- Both starts two independent capture pipelines under one session clock.
- Keep original track format metadata; create separate 16 kHz mono inference input as needed.
- Capture callbacks only copy frames, timestamp and enqueue to bounded writer buffers.
- Use monotonic/QPC timing and sample positions; UTC is metadata, not the primary alignment clock.
- Handle default device changes, device removal, sleep/resume, audio format changes and loopback silence.

## Background behavior
- User manually starts recording.
- Recording continues when WPF window is minimized or sent to tray.
- On window close while recording, prompt: continue in tray / stop / cancel.
- Tray icon and menu expose current recording state and Stop.
- No auto-start recording at Windows login. Optional launch-on-login may only open the UI, never activate microphone.

## Audio durability
- Per-track bounded segment writer, target chunk ≤10s pending measurement.
- Atomic seal and SQLite manifest commit.
- Disk reserve and writer lag warnings.
- App restart reconciles committed and orphan chunks; never auto-start capture.

## AI
- Shared model catalog/recommender and scheduler.
- Native inference backend selected per model/runtime; one ASR model instance by default.
- Both mode keeps independent track contexts and queues work against shared model instance unless benchmarks justify safe parallelism.
- UI displays AI queue lag independently from recording health.

## Acceptance
- Mic/System/Both capture verified on real Windows x64 hardware.
- Two tracks align within target (<100 ms initial target, measured).
- No app-level chunk gaps in 8-hour test; every missing source callback is represented as Gap.
- UI close/tray behavior, sleep/resume and device removal have tested transitions.
- AI process failure does not stop capture.
