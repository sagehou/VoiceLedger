# Windows WASAPI prototype verification

The WPF client enumerates capture and render endpoints using `NAudio.Wasapi` 2.4.0. It shows the selected physical endpoint, active state, default designation and mix format. Start opens the chosen endpoint IDs in shared mode. Both opens two independent pipelines on one session-relative QPC clock.

If a driver withholds a friendly name or mix format, the list uses a fallback label for that field. A discovery error for one endpoint or direction leaves the other entries usable. The status line gives the operation and HRESULT for a discovery error and reports when there are no active endpoints. Include the exact status line and whether the endpoint appears in Windows Sound settings when reporting an empty list.

The capture thread copies packets into a bounded 32-packet channel with a 1 MiB per-packet upper bound. Each stored packet keeps its raw device frame position, WASAPI QPC timestamp in 100 ns units and normalized session time. Estimated timestamps are marked. Silent buffers carry time without synthesized samples. A healthy loopback stream can advance through silence without packets. Endpoint loss, clock discontinuities, suspend, format changes and queue overflow produce explicit loss boundaries.

Each source writes to its own directory under `%LOCALAPPDATA%\VoiceLedger\Recordings`. The [prototype output decision](../adr/0002-windows-wasapi-prototype-output.md) describes the sink boundary. The `session.json` snapshot links stable track IDs to the shared clock. Each track has `timeline.jsonl` and sealed WAV segments. A numbered WAV segment holds up to ten seconds of samples and 512 packet records. Idle checkpoints seal the last audio during a quiet interval. The journal includes a SHA-256 for each sealed file and the byte offset of each packet in the WAV data. The writer reserves 64 MiB of free space. Any write failure interrupts the affected track and reports the saved boundary. `.partial` files represent uncommitted output.

`windows-client` runs native Windows endpoint discovery through the WPF view model and records the runner's endpoint counts. It also exercises metadata errors and executes the WASAPI packet queue, state coordination and WAV writer through a scripted source. The smoke checks run the Rust bindings, compare synthetic frame bytes and timestamps, and exercise default changes, simulated disconnect, pause, reconnect and suspend. The resulting CI artifact contains the test log and Windows package.

## Physical Windows x64 acceptance

Extract the `windows-client` artifact from the linked Actions run and open `windows/VoiceLedger.exe` on a physical Windows x64 device with the .NET 10 Desktop Runtime installed. Record the build SHA, OS build, architecture, endpoint roles, sample formats, duration, expected and observed outcomes, and evidence location in the [evidence schema](../../contracts/test-evidence-v1.schema.json). Attach redacted metadata to [Issue #5](https://github.com/sagehou/VoiceLedger/issues/5). Keep user audio local.

| Test | Procedure and expected outcome | Status |
| --- | --- | --- |
| `wasapi.mic` | Select a capture endpoint, start Mic, speak or use a synthetic test tone, then stop. Check the microphone WAV and packet metadata. | **BLOCKED** |
| `wasapi.system` | Select a render endpoint, play a synthetic tone, allow an idle interval, play a second tone and stop. Check that the session clock advances during idle and that the journal labels the observed idle interval Silence. | **BLOCKED** |
| `wasapi.both.alignment` | Record Both using a short synthetic transient played through speakers and recorded by the selected mic. Locate the transient in each WAV. Use packet start times and sample offsets to calculate relative timing. Report the measured delta and acoustic conditions. Initial target is under 100 ms. | **BLOCKED** |
| `wasapi.endpoint-loss` | While Both is recording, unplug or disable one selected endpoint. Confirm its track shows a gap and the other track keeps recording. Restore the same endpoint and reconnect explicitly. Confirm the epoch advances only on that track. | **BLOCKED** |
| `wasapi.default-and-format` | Change the OS default during recording. Check that the bound endpoint ID stays fixed. Change the selected endpoint format and confirm the transition records a format change or invalidation. | **BLOCKED** |
| `wasapi.background-and-suspend` | Minimize or move the window to tray, then stop from the tray. Start a new session and suspend and wake Windows. Check that affected streams close and reconnect requires user action. | **BLOCKED** |

Run a short functional recording first. A 1–2 hour soak and an extended representative soak remain release evidence. Test reporting uses physical measurements for endpoint behavior and alignment.
