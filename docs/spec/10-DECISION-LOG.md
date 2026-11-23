# Decision Log

## Confirmed
- Windows and Android native clients.
- Windows WPF UI retained.
- Android Kotlin/Compose UI retained.
- Manual user start; background recording continues until user stops, subject to OS limits.
- Windows Mic/System/Both selectable, separate tracks.
- Android screen-off microphone capture via user-started foreground service.
- Chinese, English and Japanese transcription target.
- Optional user-selected translation.
- Speaker diarization plus user-confirmed name mapping.
- Local-first recording and data.
- No required VoiceLedger server.
- Optional WebDAV/S3 client-encrypted sync.
- Device hardware detection and model recommendation.
- Recording-only mode must always remain available.

## Proposed, must validate
- Rust shared core with C ABI/JNI bridge.
- .NET 10 LTS, WPF, NAudio version.
- Kotlin/Compose, minSdk and target SDK.
- Audio chunk duration/codec/bitrate.
- Whisper Tiny multilingual quantized and SenseVoice INT8 candidates.
- CAM++ speaker candidate.
- Marian/Bergamot/ML Kit translation candidates.
- Exact crypto envelope, algorithms, event format and conflict protocol.
- Hardware memory budget and RTF thresholds.

## Rejected or not required
- Dedicated inference server or VoiceLedger sync API.
- Mandatory Docker/Kubernetes.
- Automatic public cloud AI fallback.
- Hidden or boot-time microphone activation.
- Destructive mixing or deduplication of Windows tracks.
- Mandatory local AI on every device irrespective of measured resources.

## Change record template
### YYYY-MM-DD — Decision title
- Status: proposed / accepted / superseded
- Context:
- Decision:
- Alternatives:
- Evidence:
- Consequences:
- Migration/rollback:
