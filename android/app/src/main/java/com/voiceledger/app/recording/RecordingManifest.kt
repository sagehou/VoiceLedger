package com.voiceledger.app.recording

import com.voiceledger.app.core.AudioLibrary
import com.voiceledger.app.core.CoreSession
import com.voiceledger.app.core.SessionCommand
import java.io.File
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject

internal const val STORAGE_RESERVE_BYTES = 64L * 1024 * 1024

private fun json(vararg fields: Pair<String, Any>) = JSONObject(mapOf(*fields))

internal fun recoverLibrary(directory: File): Pair<SavedAudio?, RecoveryReport?> =
    AudioLibrary(directory).use { library ->
        val report = JSONObject(requireNotNull(library.request("""{"op":"recovery"}""")).decodeToString())
        val history = JSONArray(requireNotNull(library.request("""{"op":"history"}""")).decodeToString())
        val latest = history.optJSONObject(0)?.let {
            SavedAudio(
                chunks = it.getInt("committed_chunks"),
                throughNs = it.getLong("last_ns"),
                gaps = it.getInt("loss_intervals"),
                unknownTail = it.getBoolean("interrupted_tail"),
            )
        }
        val recovery = RecoveryReport(
            interrupted = report.getInt("interrupted_sessions"),
            recoveredChunks = report.getInt("restored_chunks"),
            knownTailLossNs = report.getLong("known_tail_loss_ns"),
            elapsedMs = report.getLong("elapsed_ms"),
        )
        latest to recovery.takeIf { it.interrupted != 0 || it.recoveredChunks != 0 }
    }

internal class RecordingManifest(
    private val library: AudioLibrary,
    private val progress: (SavedAudio, Long) -> Unit,
) : AutoCloseable {
    private val session = CoreSession()
    val sessionId = UUID.randomUUID().toString()
    private val trackId = UUID.randomUUID().toString()
    private val transitions = JSONArray()
    private val pcm = FloatArray(2 * SAMPLE_RATE)
    private var packets = JSONArray()
    private var frameCount = 0
    private var chunkId: String? = null
    private var checkpointNs = 0L
    private var streamStartNs: Long? = null
    private var lastPacketEndNs = 0L
    private var saved = SavedAudio()

    fun prepare(origin: Long) {
        write(
            "create_session", "id" to sessionId, "created_at_ms" to System.currentTimeMillis(),
            "clock" to "android_boottime", "clock_origin" to origin, "tracks" to JSONArray().put(
                json("id" to trackId, "source" to "mic", "endpoint_id" to "android_default_mic"),
            ),
        )
        transition(SessionCommand.Prepare, 0, "user_start")
    }

    fun beginStream(startNs: Long, routeType: Int?) {
        val format = json("encoding" to "pcm_s16", "rate" to SAMPLE_RATE, "channels" to 1)
        if (routeType != null) format.put("android_route_type", routeType)
        write(
            "begin_stream", "track_id" to trackId, "epoch" to 0, "start_ns" to startNs,
            "rate" to SAMPLE_RATE, "channels" to 1, "sample_format" to "pcm_s16",
            "native_format" to format.toString(),
        )
        streamStartNs = startNs
        lastPacketEndNs = startNs
        transition(SessionCommand.Start, startNs, "microphone_started")
    }

    fun append(samples: ShortArray, time: PacketTime) {
        if (frameCount + time.frames > pcm.size || packets.length() >= 256) seal()
        if (chunkId == null) {
            val id = UUID.randomUUID().toString()
            write(
                "begin_chunk", "id" to id, "track_id" to trackId, "epoch" to 0,
                "start_ns" to time.startNs, "end_ns" to time.endNs,
            )
            chunkId = id
            checkpointNs = time.endNs
        }
        for (frame in 0 until time.frames) {
            pcm[frameCount + frame] = samples[frame] / 32768f
        }
        frameCount += time.frames
        lastPacketEndNs = time.endNs
        packets.put(
            json(
                "start_ns" to time.startNs, "end_ns" to time.endNs, "frames" to time.frames,
                "device_position_frames" to time.position, "qpc100ns" to 0, "flags" to 0,
                "estimated_timestamp" to time.estimated,
            ),
        )
        if (frameCount == pcm.size) {
            seal()
        } else if (time.endNs - checkpointNs >= 250_000_000L) {
            write("checkpoint", "id" to requireNotNull(chunkId), "end_ns" to time.endNs)
            checkpointNs = time.endNs
            progress(saved, time.endNs)
        }
    }

    fun finish(stop: StopRequest): SavedAudio {
        val endNs = maxOf(lastPacketEndNs, stop.atNs)
        transition(SessionCommand.Stop, endNs, stop.reason.code)
        seal()
        if (streamStartNs != null) {
            if (stop.reason != CaptureEnd.User && endNs > lastPacketEndNs) {
                write(
                    "interval", "track_id" to trackId, "epoch" to 0, "start_ns" to lastPacketEndNs,
                    "end_ns" to endNs, "kind" to "gap", "cause" to stop.reason.code,
                )
                saved = saved.copy(gaps = saved.gaps + 1)
            }
            write("end_stream", "track_id" to trackId, "epoch" to 0, "end_ns" to endNs, "reason" to stop.reason.code)
        }
        val command = if (stop.reason == CaptureEnd.User) SessionCommand.Finish else SessionCommand.Interrupt
        transition(command, endNs, stop.reason.code)
        return saved
    }

    fun checkpointUnwrittenTail(capturedThroughNs: Long) {
        val id = chunkId ?: return
        if (capturedThroughNs > checkpointNs) {
            write("checkpoint", "id" to id, "end_ns" to capturedThroughNs)
        }
    }

    override fun close() = session.close()

    private fun seal() {
        val id = chunkId ?: return
        val command = json("op" to "seal_chunk", "id" to id, "end_ns" to lastPacketEndNs, "packets" to packets)
        library.request(command.toString(), pcm, frameCount)
        frameCount = 0
        chunkId = null
        packets = JSONArray()
        saved = saved.copy(chunks = saved.chunks + 1, throughNs = lastPacketEndNs)
        progress(saved, lastPacketEndNs)
    }

    private fun transition(command: SessionCommand, atNs: Long, reason: String) {
        val state = session.apply(command, atNs)
        transitions.put(json("state" to state.code, "at_ns" to atNs, "reason" to reason))
        val snapshot = json("schema_version" to 1, "transitions" to transitions)
        write(
            "session_state", "id" to sessionId, "state" to state.code, "elapsed_ns" to atNs,
            "snapshot" to snapshot,
        )
    }

    private fun write(operation: String, vararg fields: Pair<String, Any>) {
        library.request(json("op" to operation, *fields).toString())
    }
}
