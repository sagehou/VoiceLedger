# GitHub Actions Build and Validation Policy

## Rule
All authoritative compile/build/test validation runs in GitHub Actions. Coding agents should not use local compilation as acceptance evidence. Push focused changes, wait for Actions, inspect failed job logs, fix, and push again.

## Required jobs as implementation appears
| Job | Runner | Scope |
| --- | --- | --- |
| rust-core | ubuntu-latest | fmt, clippy, unit tests, host tests |
| rust-windows | windows-latest | build shared core for x86_64-pc-windows-msvc and produce native library |
| rust-android | ubuntu-latest | install Android NDK; build aarch64-linux-android shared library |
| windows-client | windows-latest | restore/build WPF solution, run .NET tests, publish smoke package |
| android-client | ubuntu-latest | pinned JDK + Android SDK, Gradle lint/test/assemble |
| ffi-integration | windows-latest + ubuntu-latest | exercise C# P/Invoke and Kotlin/JNI against built shared core |
| contracts | ubuntu-latest | validate JSON schemas, golden fixtures and cross-platform serialization |
| security-static | ubuntu-latest | dependency/license/SAST checks and secret scanning |
| artifacts | per platform | upload logs, test reports and non-sensitive build outputs |

## Trigger policy
- Pull requests to main and pushes to implementation branches.
- Changes to shared core, contracts, model catalog, sync format or crypto must run both Windows and Android integration jobs.
- Documentation-only changes may use a lightweight docs validation job; do not claim application compilation passed if no app build ran.
- Pin SDK, JDK, Android Gradle Plugin, NDK, Rust toolchain, NuGet and Gradle dependencies. Use lockfiles and cache keyed by lockfile hashes.

## Hardware-in-the-loop boundary
GitHub-hosted runners do not prove WASAPI endpoint behavior, Android microphone foreground service survival, screen-off recording, vendor battery policy, thermal throttling, acoustic quality, or 8-hour recording reliability. These remain real-device tests. The test harness may build in Actions, but the result is marked blocked/not_run until device evidence is attached to the relevant issue.

## Agent workflow
1. Make a focused commit for one issue.
2. Push to a branch and wait for required Actions checks.
3. Fix failures using job logs; do not claim success from local build output.
4. Attach Actions run URL, commit SHA, job results and artifact links to the issue.
5. For hardware-only gates, attach device model, OS, build, ABI, test duration, metrics and redacted logs.
6. Close an issue only when its acceptance checklist and required evidence are complete.

## Security
Never upload real audio, transcripts, voice embeddings, access tokens, credentials, vault keys or recovery material as CI artifacts. Use synthetic fixtures only.
