package com.voiceledger.app.recording

import java.util.concurrent.atomic.AtomicReference

internal const val SAMPLE_RATE = 48_000
internal const val PACKET_FRAMES = SAMPLE_RATE / 50
internal const val SECOND_NS = 1_000_000_000L

internal data class CaptureTimestamp(val position: Long, val atNs: Long)
internal data class PacketTime(
    val startNs: Long,
    val endNs: Long,
    val position: Long,
    val frames: Int,
    val estimated: Boolean,
)

internal class CaptureTimeline(private val streamStartNs: Long) {
    private var position = 0L
    private var originNs = streamStartNs
    private var estimated = true

    fun next(frames: Int, observedAtNs: Long, timestamp: CaptureTimestamp?): PacketTime {
        if (position == 0L) {
            val latestOrigin = (observedAtNs - frameTime(frames.toLong())).coerceAtLeast(streamStartNs)
            val hardwareOrigin = timestamp?.takeIf { it.position >= 0 && it.atNs <= observedAtNs }
                ?.let { it.atNs - frameTime(it.position) }
                ?.takeIf { it in streamStartNs..latestOrigin }
            originNs = hardwareOrigin ?: latestOrigin
            estimated = hardwareOrigin == null
        }
        val startNs = originNs + frameTime(position)
        val packetPosition = position
        position += frames
        return PacketTime(startNs, originNs + frameTime(position), packetPosition, frames, estimated)
    }

    private fun frameTime(frames: Long): Long =
        frames / SAMPLE_RATE * SECOND_NS + frames % SAMPLE_RATE * SECOND_NS / SAMPLE_RATE
}

internal enum class CaptureEnd(val code: String) {
    User("user_stop"),
    Permission("permission_revoked"),
    Notification("notification_unavailable"),
    Route("route_changed"),
    Contention("microphone_contended"),
    Input("capture_unavailable"),
    Backpressure("capture_queue_full"),
    LowStorage("low_storage"),
    Storage("storage_error"),
    Service("service_ended"),
}

internal data class StopRequest(val reason: CaptureEnd, val atNs: Long)

internal class RecordingControl(private val clock: () -> Long, val origin: Long = clock()) {
    private val requested = AtomicReference<StopRequest?>()
    val stop: StopRequest? get() = requested.get()

    fun elapsedNs(): Long = (clock() - origin).coerceAtLeast(0)

    fun request(reason: CaptureEnd) {
        requested.compareAndSet(null, StopRequest(reason, elapsedNs()))
    }
}
