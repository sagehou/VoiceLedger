# Android microphone recording

The Compose screen requests microphone permission after **Start recording**. VoiceLedger also requires recording notifications to be enabled so the foreground service can present **Stop recording**. The service owns AudioRecord and keeps running when the Activity is recreated or leaves the foreground. The notification and Activity use the same Stop path, which drains queued audio and seals the last segment. Each notification action targets its own recording. [Issue #7](https://github.com/sagehou/VoiceLedger/issues/7)

AudioRecord delivers mono 48 kHz PCM16 for the prototype. The source format, input route type when available, and sample positions are recorded in the shared manifest. The session clock uses elapsed realtime, including suspend. The first AudioRecord boot-time timestamp anchors the stream when valid. Otherwise the first read anchors the stream and the packets are marked estimated. Android uses the session-relative packet times and leaves the Windows QPC field at zero.

The capture thread passes 20 ms batches through a pool of 24 PCM buffers. The writer checks progress at roughly 250 ms intervals and seals at two seconds or 256 packets. It uses the shared SQLite manifest, Ogg Opus writer and SHA-256 validation in app-private storage. At the 64 MiB free-space reserve it stops input and drains the queue. Route changes, silenced input, a call, loss of permission, input stalls and a full buffer queue interrupt recording. The known missing interval is saved as a Gap when storage remains writable. Actual quiet microphone samples are kept as audio. Low memory and severe thermal pressure are shown in the recording screen.

Opening the app after process death or reboot reconciles the manifest and reports saved segments, gaps and any known tail loss. Android can stop the recording process without running service cleanup. The service is non-sticky, and starting again requires the visible Start action. On a write failure, the writer checkpoints the observed tail if SQLite remains writable.

Android backup and device-transfer rules exclude the recording library. [Android Auto Backup](https://developer.android.com/identity/data/autobackup)

Android battery management, including vendor-specific restrictions, can suspend or terminate a foreground service. The prototype relies on the platform microphone service without a partial wake lock. Screen-off reliability, timing and power use remain subject to measurement on representative phones.

Rust 1.98 provides the Android file locks used by the recording library, device profile and model installer. The Actions emulator loads the packaged ARM64 JNI library, checks exclusive manifest ownership, writes and reopens a synthetic Opus segment, refreshes the device profile and downloads and verifies Whisper through the private speech worker.

## Acceptance

| Check | Evidence |
| --- | --- |
| JVM and Rust JNI | Synthetic sample positions, duplicate Stop, bounded PCM transfer, Opus commit, seek and restart reconciliation |
| Android APK | Gradle lint, tests, arm64 package, microphone service type and permissions |
| Visible Start, 15–30 minutes screen off, notification Stop | **BLOCKED** pending Android device evidence |
| Process death and reboot, input policy, route changes, permissions, storage, thermal and power | **BLOCKED** pending Android device evidence |
| 1–2 hour development soak and representative 8-hour release run | **BLOCKED** pending Android device evidence |

Actions uploads the APK, JUnit reports and packaged manifest in the `android-client` artifact. Device evidence records the commit, phone and OS build, ABI, duration, saved segments, known loss, recovery time and result. Keep audio and the SQLite manifest on the device. [Evidence schema](../../contracts/test-evidence-v1.schema.json)
