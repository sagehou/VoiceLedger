# Retain Rust for the shared core

The Windows and Android clients share session transitions and the initial model recommendation probe through one Rust core crate. Keep Rust after the vertical slice because the C ABI and JNI paths use one FFI crate, build for both required targets and pass the same fixture without third-party Rust dependencies. [Issue #3](https://github.com/sagehou/VoiceLedger/issues/3)

The initial [hosted slice run](https://github.com/sagehou/VoiceLedger/actions/runs/45106171056) measured the following raw native library sizes and hosted job wall times, including toolchain setup and artifact upload.

| Rust job | Wall time | Native artifact size |
| --- | --- | --- |
| `rust-core` on Linux x64 | 19 s | 423,672 bytes |
| `rust-windows` on Windows x64 | 28 s | 125,952 bytes |
| `rust-android` for Android arm64 | 40 s | 463,432 bytes |

The WPF P/Invoke test and the Android JVM tests each passed 25 cases using fixture SHA-256 `331e8d3314632dfd3feddf534707cc08691f8e52c47f86ce82216b307bf74969`, plus 128 create, cancel and release cycles per client. Rust tests exercised stale handles and panic conversion. The Android packaging check found the arm64 library in the APK. The first run generated the Gradle test lock and required it to be committed before the CI gate could pass. [Windows report](https://github.com/sagehou/VoiceLedger/actions/runs/45106171056/artifacts/14787591459) · [Android report](https://github.com/sagehou/VoiceLedger/actions/runs/45106171056/artifacts/14787697272)

The measured sizes are build outputs. Production runtime memory, device debugging and recording behavior still require representative Windows and Android hardware. The [versioned ABI](../engineering/SHARED-CORE-ABI-V1.md) and fixture provide a conformance baseline if the cross-target cost grows. This pre-persistence slice can be rolled back by reverting its code and packaging changes.
