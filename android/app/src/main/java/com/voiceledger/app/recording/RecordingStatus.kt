package com.voiceledger.app.recording

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update

internal enum class RecordingPhase { Recovering, Idle, Preparing, Recording, Pausing, Paused, Stopping }

internal data class SavedAudio(
    val chunks: Int = 0,
    val throughNs: Long = 0,
    val gaps: Int = 0,
    val unknownTail: Boolean = false,
    val sessionId: String? = null,
)

internal data class RecoveryReport(
    val interrupted: Int,
    val recoveredChunks: Int,
    val knownTailLossNs: Long,
    val elapsedMs: Long,
)

internal data class RecordingStatus(
    val phase: RecordingPhase = RecordingPhase.Recovering,
    val saved: SavedAudio? = null,
    val elapsedNs: Long = 0,
    val failure: CaptureEnd? = null,
    val thermal: Boolean = false,
    val lowMemory: Boolean = false,
    val recovery: RecoveryReport? = null,
)

internal object RecordingStatusStore {
    val state = MutableStateFlow(RecordingStatus())

    fun update(transform: (RecordingStatus) -> RecordingStatus) = state.update(transform)
}
