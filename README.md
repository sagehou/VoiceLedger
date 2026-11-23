# VoiceLedger

Local-first voice recording and multilingual meeting transcription for Windows and Android.

## Product baseline
- Windows native WPF client; Android native Kotlin/Jetpack Compose client.
- User manually starts recording; capture continues in the background until the user stops, subject to OS limits.
- Windows Mic / System loopback / Both, with independent tracks.
- Android user-started microphone foreground service with screen-off recording.
- Local AI with hardware-aware model recommendations and recording-only fallback.
- Optional client-side encrypted WebDAV/S3 sync; no dedicated VoiceLedger server.
- Shared business logic is preferred; Rust is a candidate pending a cross-platform vertical slice.

## Specification
Read [AGENTS.md](AGENTS.md) first, then the product and technical specifications in [docs/spec](docs/spec/).

## Project management
- [Implementation tracker](https://github.com/sagehou/VoiceLedger/issues/20)
- [All implementation issues](https://github.com/sagehou/VoiceLedger/issues)

All authoritative compile/build/test validation is performed in GitHub Actions. Real-device recording, thermal, battery and long-run behavior require physical-device evidence attached to the relevant issue.

See [CI and test traceability](docs/engineering/CI.md) for the build entry points, artifacts and acceptance gates.

Hardware compatibility, model accuracy/performance and cryptographic interoperability remain validation items until supported by test evidence.
