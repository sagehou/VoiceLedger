# VoiceLedger Product Overview

Version: 1.0-draft  
Status: implementation baseline; hardware and model validation remain open.

## Product vision
VoiceLedger is a local-first voice ledger for Windows and Android. A user manually starts a recording session and leaves it running in the background until manually stopped. The application stores original audio locally and gives each saved session a note with a durable Chinese, English or Japanese transcript and an editable meeting summary. The note can also show translations and user-confirmed speaker names.

Cross-device continuity is optional and uses client-side encrypted objects in a user-provided WebDAV or S3-compatible store. There is no required VoiceLedger backend.

## Product principles
1. **Capture first.** Audio durability is more important than live AI latency.
2. **Local-first.** Recordings and session notes live on the device. Local transcription adapts to the hardware. Users can choose external processing and encrypted sync.
3. **Resource-aware.** Hardware detection and measured benchmarks recommend models and modes. Recording-only always works.
4. **Native experience.** WPF on Windows; Kotlin/Compose on Android.
5. **Shared behavior.** Keep domain rules and data semantics in one shared core where practical; isolate OS capabilities in adapters.
6. **Privacy by default.** No automatic third-party upload, no cloud inference fallback, no sync until configured and enabled.
7. **Human authority.** Machine output is revisable; user edits and confirmed identities are never silently overwritten.

## Confirmed product decisions
- Windows and Android clients.
- Windows source modes: microphone, system loopback, both.
- Android: user-initiated microphone recording continues with screen off via foreground service.
- Recording start mode: manual start; continue in background until user stops.
- Chinese, English and Japanese recognition.
- Saved session notes with the complete final transcript, playback, edits and export.
- Editable meeting summaries. Users can write a summary or generate one on demand with a configured text model. Include key points, decisions and action items when the transcript supports them.
- Translation is optional and target language is user-selected.
- Speaker diarization and user-confirmed name mapping.
- Local-first storage and processing.
- No dedicated VoiceLedger sync/inference server.
- Optional WebDAV/S3 encrypted sync.
- Device-aware model recommendation; users can override among compatible choices.

## Proposed engineering choices (not yet proven)
- Windows: C#/.NET 10 LTS + WPF/MVVM; NAudio WASAPI adapter.
- Android: Kotlin + Jetpack Compose + AudioRecord + microphone foreground service + Room/SQLite.
- Shared core: Rust library with C ABI for .NET and JNI/Kotlin bindings, subject to a vertical-slice build spike.
- AI: WebRTC VAD or Silero VAD; Whisper.cpp multilingual Tiny quantized and sherpa-onnx SenseVoice INT8 as ASR candidates; CAM++ as speaker candidate; translation candidates evaluated separately.
- Local database: SQLite; original audio in bounded rolling segments.
- Sync: immutable per-device encrypted event objects and optional encrypted audio blobs.

## Non-goals for MVP
- General-purpose LLM chat.
- Team collaboration, multi-tenant server, central database.
- Automatic remote inference fallback. The user can send a saved track to a configured service for [on-demand transcription](../adr/0004-on-demand-external-transcription.md).
- Generic Android internal audio / third-party call recording.
- Hidden recording or automatic microphone activation after reboot.
- Destructive merging of microphone and system tracks.
- Browser/PWA client.

## MVP success statement
A user can manually start a session on either platform, record reliably, pause/resume/stop, inspect and play back local history, and recover committed audio after app restart. On devices that pass model gates, the user can run local Chinese, English and Japanese transcription with optional translation and speaker analysis. A saved note can be reopened and edited after restart. The user can generate a summary of its finalized transcript with a configured provider, review and edit it, and export the note. Optional WebDAV/S3 sync exchanges encrypted data between devices without a VoiceLedger backend. When AI, storage, or sync fails, the app accurately reports the condition and never pretends missing audio was saved.
