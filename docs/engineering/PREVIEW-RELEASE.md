# VoiceLedger v0.1.0-preview.1

This build provides local recording, segmented Opus storage, restart reconciliation and on-device transcription of saved audio. VoiceLedger starts in recording-only mode. The Models screen downloads weights after an explicit action. Whisper Tiny is the first transcription candidate and availability depends on the device profile. SenseVoice availability is pending memory measurements.

## Install

- **Windows x64.** Extract the ZIP and open `VoiceLedger.exe`. Install the x64 .NET 10 Desktop Runtime first. The portable executable is unsigned. [Microsoft .NET 10](https://dotnet.microsoft.com/en-us/download/dotnet/10.0)
- **Android arm64, Android 8 or later.** Open the signed preview APK. The preview installs separately from `com.voiceledger.app` as `com.voiceledger.app.preview`. The preview uses a one-time signing certificate and a separate local library. Android requests microphone and notification permissions when you start recording.

## Preview coverage

- Windows offers microphone, system loopback and Both modes. Android offers microphone recording with a foreground service and a Stop notification action.
- Audio, transcripts and model files stay on the device. Model downloads use the pinned upstream URLs in the catalog. The packages contain the native components and their notices. [Model catalog](https://github.com/sagehou/VoiceLedger/blob/v0.1.0-preview.1/docs/engineering/MODEL-CATALOG.md)
- Physical-device verification, extended recording, model performance, upgrade and data transfer are pending. Optional translation, speaker features, encrypted sync and the complete session editor are in development. [Integrated acceptance](https://github.com/sagehou/VoiceLedger/issues/18) · [Release issue](https://github.com/sagehou/VoiceLedger/issues/19)
