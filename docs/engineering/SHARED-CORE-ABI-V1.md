# Shared core ABI v1

The [C header](../../core/ffi/include/voiceledger_ffi.h) defines fixed-width scalar inputs, numeric results and the `v1` exports. Windows P/Invoke and Kotlin JNI call the same Rust session and recommendation functions. Both adapters check the ABI version on first use.

## Session ownership and threading

- A positive handle returned by `vl_session_create_v1` owns a session in the current process. Release it once after use. Windows uses `SafeHandle` and Kotlin uses `AutoCloseable`.
- Session operations are synchronous. Rust serializes handle access inside a mutex. The mutex covers in-memory state transitions and is released before returning to the caller.
- Transition timestamps are session-relative monotonic nanoseconds supplied by the platform. The Rust session remembers the last state change with its time and reason. Repeating Stop returns the existing state.
- Cancellation is idempotent. Further transitions on a cancelled handle return `VL_CANCELLED`. Release remains available after cancellation. Calls on a released handle return `VL_UNKNOWN_HANDLE`.
- Errors are negative return values. Rust catches unwinding panics at the ABI boundary and returns `VL_INTERNAL_ERROR`. The adapters surface the numeric error code in a platform exception.

## Recommendation probe

The input is platform compatibility, available inference memory budget and candidate peak memory in bytes. The ABI encodes an unknown memory value as `-1`. The probe returns `CandidateForBenchmark` when compatibility is confirmed and a positive peak estimate fits the budget. Other valid inputs return `RecordingOnly`. The benchmark, model quality gates and download or load decisions belong to the adaptive runtime.

## Integration evidence

The [same fixture](../../contracts/fixtures/ffi-v1.tsv) runs through the WPF assembly's P/Invoke bindings and the Android app's JNI bindings. The latter runs on a hosted Linux JVM with the Linux Rust target. The Android NDK job independently produces an arm64 library that the Android packaging smoke check finds in the APK.

`vl_domain_validate_v1` accepts a borrowed UTF-8 [domain v1 batch](DOMAIN-CONTRACT-V1.md) of up to 16 MiB and returns a numeric validation result synchronously. Windows passes a byte array and Android JNI copies the bounded Java byte array into Rust once per call. Both consume the same schema-checked fixture artifact.
