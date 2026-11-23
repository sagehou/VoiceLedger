# Device capability profile

At app startup, each native client samples device capabilities on a background thread. The shared Rust core adds the running library ABI and supported CPU instructions, validates the existing v1 contract and atomically replaces the latest local snapshot. The device UUID is generated locally and kept across refreshes. Each observation has its own UUID and timestamp. [Issue #8](https://github.com/sagehou/VoiceLedger/issues/8) · [Domain v1](DOMAIN-CONTRACT-V1.md)

| Field | Windows | Android |
| --- | --- | --- |
| OS and CPU count | Windows version and build, logical processors available to the process | Android API level and logical processors available to the process |
| Physical memory | Win32 total and available RAM, low-memory notification state | ActivityManager total and available RAM, low-memory state and threshold, low-RAM classification |
| Process memory | Private bytes | Process PSS converted from KiB to bytes |
| Storage | Available bytes at the local library root | Available bytes at the app files directory |
| Thermal and power | Windows power saver and battery percentage. Thermal pressure and the low-memory threshold remain unknown. | Audio-independent OS thermal status on API 29+, power saver and battery percentage |
| Local snapshot | `%LOCALAPPDATA%\VoiceLedger\device-profile-v1.json` | `no_backup/device-profile-v1.json` in app-private storage |

`null` represents unavailable measurements. A measured zero, a normal thermal state and a false pressure flag are kept as reported. Storage reports available space on the local volume. The recorder applies its reserve when writing audio.

Snapshots contain the local IDs and capability measurements. The profiler uses local OS APIs and has no upload path. A failed refresh keeps the existing snapshot. Consumers can use the observation timestamp to judge freshness. The ABI uses `vl_device_profile_save_v1` and accepts an absolute UTF-8 directory plus an observation JSON object. Rust supplies `abi` and `cpu_features`.

GitHub Actions exercises Win32 queries on the Windows runner and verifies synthetic measurements, thermal mapping and local persistence through Android's JVM and host JNI tests. Physical profile evidence for one Windows device and two Android devices is **BLOCKED** pending the #18 device run. [Issue #18](https://github.com/sagehou/VoiceLedger/issues/18)
