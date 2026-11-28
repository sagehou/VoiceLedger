# AI Models and Adaptive Runtime

## Principles
- Recording-only mode always available.
- No model is downloaded or loaded without user consent.
- Model file size is not runtime RAM.
- A model is recommended for real-time use only after local benchmark evidence.
- Never benchmark or upgrade models during active recording.
- External processing starts on the user's send action. Meeting summary generation uses the selected text service and finalized transcript while recording is idle.

## Initial candidates
| Capability | Candidate | Notes |
| --- | --- | --- |
| VAD | WebRTC VAD / Silero VAD | Compare CPU cost and segmentation quality |
| Lightweight multilingual ASR | whisper.cpp multilingual Tiny quantized | Must not use tiny.en for Chinese/Japanese |
| Balanced multilingual ASR | sherpa-onnx SenseVoice Small INT8 | Offline VAD-segmented inference, not native token streaming |
| Speaker | 3D-Speaker CAM++ INT8 | Load on demand or after session; unknown/low-confidence allowed |
| Translation | Marian/Bergamot language-pair models; Android ML Kit as comparator | Validate six directed zh/en/ja pairs, licenses and true offline behavior |
| Higher-quality comparator | Whisper Base multilingual / Fun-ASR-Nano | Not a default dependency; only for devices with measured headroom |

Candidates are not final product guarantees. Keep engine, ABI, quantization, model hash, license, language coverage and runtime version in the catalog.

## Hardware profile
Windows: OS/build, architecture, CPU logical cores and instruction sets, total and currently available RAM, process private bytes, storage reserve, thermal/power state where available.
Android: API level, ABI, ActivityManager.MemoryInfo (total/available/lowMemory/threshold), isLowRamDevice, CPU cores, thermal status, battery saver/level, app PSS and storage.
Unknown measurements remain null/unknown, never zero.

## Recommendation pipeline
1. Hard compatibility filter: OS, ABI, runtime, required instruction set, package integrity/license and disk space.
2. Compute safe memory budget from current available RAM minus capture baseline and OS reserve. Initial 25–35% of currently available RAM is only a heuristic to tune, not a fixed guarantee.
3. Show pre-download candidates with estimated RAM and package size clearly labelled.
4. User chooses and downloads; verify SHA-256 and runtime loadability.
5. When idle and user-approved, run short representative benchmark (default 60 seconds, max 180 seconds wall clock).
6. Measure cold/warm load, process-tree peak memory, RTF, final latency, CPU/thread count, thermal/battery impact and error rate.
7. Rank only candidates passing safety and language-quality thresholds. Initial real-time target RTF ≤0.7 and final P95 ≤5s are goals to validate.
8. Sustained and 8-hour qualification are separate gates; short benchmark cannot certify long-session suitability.
9. User selects Recording-only / Lightweight / Balanced / Enhanced. Manual selection cannot bypass hard incompatibility or memory safety.
10. Runtime governor pauses translation/speaker first, reduces partial cadence, defers ASR, then unloads AI. Audio capture and persistence remain highest priority.

## Resource governor
Inputs: available RAM, memory pressure, RTF, backlog, thermal status, battery saver, battery level, storage reserve.
Outputs: recommended action + reason code + effective profile.
Avoid rapid oscillation: use hysteresis/cooldown. Never auto-upgrade during recording. Persist deferred jobs.

## Acceptance
- Recommendation includes provenance and separates estimates from measurements.
- No model loads if hard compatibility or memory safety gate fails.
- Low-memory/thermal events unload AI without stopping recording.
- Test languages separately: Chinese CER, English WER, Japanese CER, mixed-language error profile.
- Windows Both mode measures total wall-clock throughput with two active tracks; do not normalize by combined track audio duration in a way that hides queue backlog.
