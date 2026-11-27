# Build and test traceability

The CI workflow runs on pull requests to `main` and pushes to implementation branches. Require the `CI gate` check on `main`. Both client builds run on shared-core and contract changes. When fixed Android signing is configured, its packaging job is part of the gate. [Workflow](../../.github/workflows/ci.yml)

| Requirement | Actions job and entry point | Evidence |
| --- | --- | --- |
| G0 Windows WPF build | `windows-client` runs locked `dotnet restore`, `dotnet build` and `dotnet publish` for `windows/VoiceLedger.slnx` and its app | `windows-client` logs and framework-dependent publish output |
| G0 Android Kotlin/Compose build | `android-client` runs `./gradlew :app:lintDebug :app:testDebugUnitTest :app:assembleDebug` under `android/` and checks the APK manifest. An API 35 Google APIs emulator loads the ARM64 APK and exercises the recording manifest, device profile and pinned Whisper download through JNI. | `android-client` build log, lint report and debug APK |
| G0 shared-core host and targets | `rust-core` runs `cargo fmt`, `cargo clippy` and locked `cargo test` under `core/`. `rust-windows` and `rust-android` cross-target locked `cargo build` for MSVC x64 and NDK arm64 | `rust-core-report`, `voiceledger-core-windows-x64`, `voiceledger-core-android-arm64-v8a` |
| G0 native FFI | `windows-client` runs the WPF P/Invoke smoke test. `android-client` runs a Kotlin JVM integration test against the Linux-host JNI library and checks the arm64 library in the APK | Windows smoke log, Android JUnit reports and [shared ABI fixture](../../contracts/fixtures/ffi-v1.tsv) |
| Evidence format | `contracts` runs `python scripts/validate_evidence.py` against the JSON Schema and synthetic fixture | `contracts-report` |
| Domain v1 schema and client conformance | `contracts` runs `python scripts/validate_domain.py artifacts/domain-fixtures-v1`. The Windows and Android jobs each verify every case digest and run the same generated inputs through the native ABI | `contracts-report`, `domain-fixtures-v1`, Windows smoke log and Android JUnit reports |
| Windows WASAPI capture | `windows-client` exercises independent tracks, QPC mapping, loopback idle, endpoint loss, pause, reconnect, suspend and committed Opus playback from a synthetic endpoint source | `windows-client` smoke log and the [physical-device evidence procedure](WINDOWS-WASAPI-VERIFICATION.md) |
| Audio durability | Linux and Windows Rust tests exit the writer at each file and SQLite boundary, then check recovery, seek and known tail loss | `rust-core-report`, `windows-audio-recovery`, [durability procedure](AUDIO-DURABILITY.md) |
| Android microphone recording | `android-client` tests sample timing, duplicate Stop, audio persistence and restart reconciliation through JNI, and checks the packaged microphone service | `android-client` JUnit reports, APK manifest and [Android recording](ANDROID-RECORDING.md) |
| Device capability profile | `windows-client` samples the runner through the Windows profiler. `android-client` checks thermal mapping and saves synthetic profiles through JNI. | Windows smoke log, Android JUnit results and [device profile](DEVICE-PROFILE.md) |
| Model catalog and recommendation | `contracts` validates the catalog schema. Rust tests cover eligibility, download verification and measured workload policy. Both clients read the shared preview through their native binding. | Contracts report, Rust tests, Windows smoke log and Android JUnit results. [Model catalog](MODEL-CATALOG.md) |
| Local VAD and transcription | Rust tests exercise committed audio, WebRTC VAD, ephemeral revisions and durable results. The native jobs build both inference adapters. Client jobs check speech worker and package integration. | Rust reports, native smoke logs, Windows and Android packages and [local transcription](LOCAL-TRANSCRIPTION.md) |
| Live transcription and final translation | `rust-core` feeds synthetic speech through active audio manifests and the packaged adapters in an isolated network namespace. It checks track attribution, English-only translation, tail drain and low-memory resumption. `windows-client` checks a final transcript through the published worker while capture is active. `android-client` compares Nemotron Q4_K_M, SenseVoice Q8_0, and SenseVoice Q6_K on the same paced audio, timing the first visible caption in the packaged app. | `live-speech`, `asr-automatic-languages`, and `android-asr-comparison.json`. Physical evidence remains **BLOCKED** in [#18](https://github.com/sagehou/VoiceLedger/issues/18). |
| AI scheduling | Rust fixtures exercise pressure thresholds and recovery, speech queue migration, durable progress and worker interruption. Windows and Android exercise resource samples through the native bindings. | Rust, Windows and Android reports and [AI scheduling](AI-SCHEDULER.md) |
| Static validation | `security-static` runs actionlint, a secret scan and OSV Scanner against the committed dependency locks and pinned Python requirements | Dependency vulnerability and license report in `security-static-report`, job log and check annotations |

Exact SDK and dependency pins are in `global.json`, `rust-toolchain.toml`, `android/app/build.gradle.kts`, the Gradle Wrapper and the workflow. `minSdk` 26 and the NDK API 26 build are bootstrap choices pending the physical-device matrix. CI evidence should include the run URL, commit SHA, job results and non-sensitive artifact links.

The Android build uses strict dependency locking against `android/app/gradle.lockfile`. When dependency pins change, regenerate the lock through the debug and release build tasks in Actions. The `kotlin-stdlib-common` metadata-only module is excluded from lock validation because Gradle can omit it when applying the JVM runtime lock. The Kotlin JVM standard library is locked.

```sh
./gradlew --write-locks :app:lintDebug :app:testDebugUnitTest :app:assembleDebug :app:assembleDebugAndroidTest \
  :app:lintRelease :app:testReleaseUnitTest :app:assembleRelease
```

Commit the generated lock and verify a fresh run without `--write-locks`.

## Android debug signing

The `android-client` job signs a pair of test APKs with a disposable certificate and verifies that installing the higher version code preserves a file in the app sandbox. `android-debug-package` runs the same upgrade check with the project's fixed debug certificate and publishes `android-debug-upgradeable`. The application ID stays `com.voiceledger.app` and the distributed APK uses the CI run number plus one as its version code.

In `sagehou/VoiceLedger`, create the `android-debug-publish` GitHub environment and allow deployments from `integration/vl-017-device-candidate` and `main`. The debug-only PKCS#12 uses the alias `androiddebugkey`. Add two secrets to that environment.

- `VOICELEDGER_ANDROID_DEBUG_PKCS12_BASE64` contains the Base64 text of the `.p12` file.
- `VOICELEDGER_ANDROID_DEBUG_PKCS12_PASSWORD` contains the PKCS#12 export password.

Set `VOICELEDGER_ANDROID_DEBUG_CERT_SHA256` to the public DER certificate's lowercase SHA-256. The repository variable enables fixed signing on eligible builds. If the fingerprint is saved as an Environment Secret, run CI on the upstream candidate branch with `sign_android_debug` enabled. Retain the keystore in the project's private credential storage for future updates.

The `SageAutoman/VoiceLedger` fork can publish the same candidate commit when upstream administration is unavailable. Only its `integration/vl-017-device-candidate` branch can use the signing environment. Record the signed artifact and public certificate fingerprint in #18. Use the same publishing branch and certificate for subsequent upgrades.

APK upgrades require the installed package's signing identity. A device with a build from before fixed signing needs its existing recordings and keys preserved before the initial transition.

## Device acceptance

- The Windows prototype acceptance decision records the available device evidence and remaining measurements. [#5](https://github.com/sagehou/VoiceLedger/issues/5) · [Acceptance decision](../adr/0003-accept-windows-wasapi-prototype-evidence.md)
- Physical Windows capture, restart and playback evidence for [#6](https://github.com/sagehou/VoiceLedger/issues/6) is **BLOCKED** pending device testing. [Durability procedure](AUDIO-DURABILITY.md)
- Android screen-off, notification Stop, input interruptions and process-death evidence for [#7](https://github.com/sagehou/VoiceLedger/issues/7) are **BLOCKED** pending device testing. [Android recording](ANDROID-RECORDING.md) · [Evidence schema](../../contracts/test-evidence-v1.schema.json)
