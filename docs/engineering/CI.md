# Build and test traceability

The [CI workflow](../../.github/workflows/ci.yml) runs on pull requests to `main` and pushes to implementation branches. Require the `CI gate` check on `main` when repository administration is available. The gate succeeds only if all constituent jobs succeed. Both client builds run on shared-core and contract changes.

| Requirement | Actions job and entry point | Evidence |
| --- | --- | --- |
| G0 Windows WPF build | `windows-client` runs locked `dotnet restore`, `dotnet build` and `dotnet publish` for `windows/VoiceLedger.slnx` and its app | `windows-client` logs and framework-dependent publish output |
| G0 Android Kotlin/Compose build | `android-client` runs `./gradlew :app:lintDebug :app:testDebugUnitTest :app:assembleDebug` under `android/` and checks the APK manifest | `android-client` build log, lint report and debug APK |
| G0 shared-core host and targets | `rust-core` runs `cargo fmt`, `cargo clippy` and locked `cargo test` under `core/`. `rust-windows` and `rust-android` cross-target locked `cargo build` for MSVC x64 and NDK arm64 | `rust-core-report`, `voiceledger-core-windows-x64`, `voiceledger-core-android-arm64-v8a` |
| G0 native FFI | `windows-client` runs the WPF P/Invoke smoke test. `android-client` runs a Kotlin JVM integration test against the Linux-host JNI library and checks the arm64 library in the APK | Windows smoke log, Android JUnit reports and [shared ABI fixture](../../contracts/fixtures/ffi-v1.tsv) |
| Evidence format | `contracts` runs `python scripts/validate_evidence.py` against the JSON Schema and synthetic fixture | `contracts-report` |
| Domain v1 schema and client conformance | `contracts` runs `python scripts/validate_domain.py artifacts/domain-fixtures-v1`. The Windows and Android jobs each verify every case digest and run the same generated inputs through the native ABI | `contracts-report`, `domain-fixtures-v1`, Windows smoke log and Android JUnit reports |
| Windows WASAPI capture | `windows-client` exercises independent tracks, QPC mapping, loopback idle, endpoint loss, pause, reconnect, suspend and sealed WAV outputs using a synthetic endpoint source | `windows-client` smoke log and the [physical-device evidence procedure](WINDOWS-WASAPI-VERIFICATION.md) |
| Static validation | `security-static` runs actionlint, a secret scan and OSV Scanner against the committed dependency locks and pinned Python requirements | Dependency vulnerability and license report in `security-static-report`, job log and check annotations |

Exact SDK and dependency pins are in `global.json`, `rust-toolchain.toml`, `android/app/build.gradle.kts`, the Gradle Wrapper and the workflow. `minSdk` 26 and the NDK API 26 build are bootstrap choices pending the physical-device matrix. CI evidence should include the run URL, commit SHA, job results and non-sensitive artifact links.

The Android build uses strict dependency locking against `android/app/gradle.lockfile`. When dependency pins change, regenerate the lock through the debug and release build tasks in Actions. The `kotlin-stdlib-common` metadata-only module is excluded from lock validation because Gradle can omit it when applying the JVM runtime lock. The Kotlin JVM standard library is locked.

```sh
./gradlew --write-locks :app:lintDebug :app:testDebugUnitTest :app:assembleDebug \
  :app:lintRelease :app:testReleaseUnitTest :app:assembleRelease
```

Commit the generated lock and verify a fresh run without `--write-locks`.

## Device acceptance

- The Windows prototype acceptance decision records the available device evidence and remaining measurements. [#5](https://github.com/sagehou/VoiceLedger/issues/5) · [Acceptance decision](../adr/0003-accept-windows-wasapi-prototype-evidence.md)
- [#7](https://github.com/sagehou/VoiceLedger/issues/7) requires Android foreground-service evidence. The hardware-only checks are **BLOCKED** pending device testing. The [evidence schema](../../contracts/test-evidence-v1.schema.json) uses `blocked` for this state.
