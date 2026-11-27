# Audio durability and recovery

The shared Rust store uses one versioned SQLite manifest and separate Ogg Opus files for each source track. Windows keeps the selected WASAPI endpoint, original format, raw device frames and QPC packet positions in the manifest. It converts PCM to 48 kHz for Opus and retains channel order. Both streams use the same session-relative clock. [Issue #6](https://github.com/sagehou/VoiceLedger/issues/6) · [Manifest v1](../../core/audio/src/manifest.sql)

The Windows producer copies packets into a 32-packet queue and reports the depth on each track. Each packet is bounded at 1 MiB. The writer rolls segments around two seconds or 8 MiB of buffered source samples, with a ten-second cap on a single segment. It seals at pause, stop, and a Silence or Gap boundary. Disk reserve is 64 MiB.

## Commit and recovery

1. Allocate a chunk ID and sequence in a pending row. Every 250 ms of processed audio, checkpoint the end of the known range.
2. Encode to a private `.opus.partial` file. Close the Opus stream with its exact granule position, decode to verify the format and frame count, hash the bytes with SHA-256 and flush the file.
3. Save the verified size and hash on the pending row. Rename the file in its existing directory, then publish the committed chunk and processing job in one SQLite transaction.
4. On startup, finish publication when the verified file exists at either name. Mark absent or damaged data as a Gap in the timeline. Quarantine unindexed files inside the managed audio tree. Reconcile interrupted streams and sessions using their last durable boundary. The library holds an exclusive file lock during capture and startup recovery.

The local data lives under `%LOCALAPPDATA%\VoiceLedger\Recordings`. `manifest.sqlite3` holds the timeline, source metadata and saved availability. A managed segment lives at `audio/<session ID>/<track ID>/<chunk ID>.opus`. Legacy WAV sessions can be opened from their original directories and read with the [WAV evidence helper](../../scripts/Measure-WasapiEvidence.ps1).

An interrupted session is flagged for its potentially unknown tail beyond the last checkpoint. The recovery report measures the known uncommitted range and the wall time of reconciliation. A processing job is a local database record. Committing audio requires only the local writer and SQLite.

**Saved track** lists committed or interrupted sessions by source. **Time (s)** seeks on the session clock. Playback reads verified contiguous chunks until an unavailable interval or a format change. Pick a later saved time to continue after a Gap or Silence.

## Acceptance evidence

GitHub Actions runs the forced process-exit scenarios at temporary creation, encoding, flush, preparation, rename, database commit and publication. It reopens the store twice, checks the reported loss and recovery time, and exercises absent and damaged files, unindexed data, per-track seek and the pending processing job transaction. The Windows client job passes scripted WASAPI capture through the same FFI writer and decoder used by WPF.

Physical Windows recording, recovery and playback are **BLOCKED** pending device evidence. From the Actions ZIP, run `windows\VoiceLedger.exe`, start a short recording and wait until a saved segment appears. End VoiceLedger in Task Manager while recording. Reopen it and record the Recovery status, then play the recovered track and seek to a saved position. Report the build, Windows version, device roles, duration and visible metrics to Issue #6. Keep recordings on the device. [Evidence schema](../../contracts/test-evidence-v1.schema.json)
