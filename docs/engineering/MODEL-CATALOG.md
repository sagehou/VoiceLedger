# Model catalog and recommendation

Windows and Android open the same read-only catalog from the Models screen. The shared core reads the local device profile and returns compatibility and its reason. The screen distinguishes download bytes from peak RAM estimates. It defaults to Recording only while real runtime adapters and measured runs are pending. [Issue #9](https://github.com/sagehou/VoiceLedger/issues/9) · [Catalog v1](../../models/catalog-v1.json) · [Schema](../../contracts/model-catalog-v1.schema.json)

| Candidate | Download | Peak RAM | Model terms |
| --- | --- | --- | --- |
| Whisper Tiny multilingual Q5_1 | 32,152,673 bytes | Upstream 273 MB estimate for the Tiny family. Target measurements pending. | [MIT](https://github.com/openai/whisper/blob/6e3be77e1a105e59086e3e21ff5f609fd6fa89a5/LICENSE) |
| SenseVoice Small INT8 | 239,549,735 bytes, including the token table | Provisional 1 GB estimate. Hosted Linux x64 process peak was 786,332 KiB on the six-clip corpus. [Actions measurement](https://github.com/sagehou/VoiceLedger/actions/runs/45724680385) | [FunASR Model License](https://github.com/modelscope/FunASR/blob/58830eca4012644aac0c3218c3ccc7d98f003fda/MODEL_LICENSE) |

The catalog pins each artifact's SHA-256, size and upstream revision. The Windows x64 and Android arm64 runtime targets are integration candidates. The app displays the runtime as unavailable until a matching native adapter is packaged.

The shared installer checks user approval, idle capture state, OS/ABI/CPU requirements, runtime version, storage and memory headroom. It stages ranged streams locally, supports cancellation and resumption at the saved byte offset, verifies every artifact's size and SHA-256 and calls the runtime load-and-release probe before publishing the model directory. The transport supplies the actual response offset. The runtime integration in #10 will supply the transport and probe. [Issue #10](https://github.com/sagehou/VoiceLedger/issues/10)

The provisional memory budget is 30% of currently available RAM after accounting for app process memory and the OS low-memory threshold when reported. The storage check leaves the recorder's 64 MiB reserve. Low memory, moderate-or-higher thermal pressure, battery saver and reported charge at or below 15% prevent the benchmark from being scheduled. A short test targets 60 seconds with a 180-second ceiling. Unknown device measurements stay unknown.

The recommendation accepts evidence for the same model digest, runtime, device and workload. Each measured record retains corpus digest, stage, stability, cold/warm timings, RTF, peak RAM, final latency, CPU and thread use. Chinese CER, English WER, Japanese CER and mixed-language token error rate each have an independent acceptance limit. Measured RTF below 1 supports real-time use. At 1 or above, a candidate that passes the other gates can be recommended for post-session use. Manual mode selection uses the same gates. Windows Both uses its own workload evidence. [Model spec](../spec/06-AI-MODELS-AND-ADAPTIVE-RUNTIME.md)

GitHub Actions validates the catalog schema, Rust policy and installer fixtures, and the C#/JNI model preview. Synthetic transfers and probe responses exercise the installer. Physical runtime load, language quality and model benchmark evidence are **BLOCKED** for the #18 device campaign. [Issue #18](https://github.com/sagehou/VoiceLedger/issues/18)
