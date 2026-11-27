package com.voiceledger.app.speech

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.voiceledger.app.R
import com.voiceledger.app.core.DeviceProfiler
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.MutableStateFlow
import org.json.JSONObject

internal enum class LiveAsrModel(val id: String, val label: Int) {
    Whisper("whisper-tiny-multilingual-q5-1", R.string.live_whisper),
    SenseVoice("sensevoice-small-int8", R.string.live_sensevoice),
    SenseVoiceGguf("sensevoice-small-gguf-q8-0", R.string.live_sensevoice_gguf),
    SenseVoiceGgufQ6("sensevoice-small-gguf-q6-k", R.string.live_sensevoice_gguf_q6),
    Nemotron("nemotron-3-5-asr-streaming-0-6b-q4-k-m", R.string.live_nemotron),
}

internal data class LiveSpeechOptions(val language: String, val translate: Boolean, val model: LiveAsrModel)

internal data class LiveLine(
    val language: String,
    val text: String,
    val translation: String? = null,
    val stale: Boolean = false,
    val partial: Boolean = false,
)

internal data class LiveSpeechStatus(
    val running: Boolean = false,
    val draining: Boolean = false,
    val result: Int? = null,
    val stage: String? = null,
    val reason: String? = null,
    val processedChunks: Long = 0,
    val lines: List<LiveLine> = emptyList(),
    val partials: List<LiveLine> = emptyList(),
    val translationPending: Boolean = false,
)

internal object LiveSpeech {
    val state = MutableStateFlow(LiveSpeechStatus())
    private val main = Handler(Looper.getMainLooper())
    private var generation = 0
    private var worker: SpeechConnection? = null
    private var latestFinal: String? = null
    private val polling = object : Runnable {
        override fun run() {
            val connection = worker ?: return
            connection.poll()
            if (worker === connection) main.postDelayed(this, 600)
        }
    }
    private val timeout = Runnable { finish(-38) }

    fun reset() {
        generation++
        release()
        state.value = LiveSpeechStatus()
    }

    fun start(context: Context, sessionId: String, options: LiveSpeechOptions) {
        val app = context.applicationContext
        val attempt = ++generation
        state.value = LiveSpeechStatus(running = true)
        thread(name = "VoiceLedger live speech profile") {
            val ready = runCatching { DeviceProfiler.capture(app) }.isSuccess
            main.post {
                if (attempt != generation) return@post
                if (ready) bind(app, sessionId, options, attempt, retries = 2) else finish(-34)
            }
        }
    }

    fun drain() {
        if (state.value.running) {
            state.value = state.value.copy(draining = true)
            restartDrainTimeout()
        }
    }

    private fun restartDrainTimeout() {
        main.removeCallbacks(timeout)
        main.postDelayed(timeout, 120_000)
    }

    fun interrupt() {
        if (state.value.running) finish(-38)
    }

    private fun bind(app: Context, sessionId: String, options: LiveSpeechOptions, attempt: Int, retries: Int) {
        val action = JSONObject().put("op", "transcribe_live").put("session_id", sessionId)
            .put("model_id", options.model.id)
            .put("language", options.language).put("translate", options.translate).put("approved", true)
        val transcript = JSONObject().put("op", "live_transcript").put("session_id", sessionId)
        val request = SpeechConnection(
            app, speechRequest(app, action), { code ->
                if (attempt == generation) {
                    if (code == -37 && retries > 0) {
                        main.removeCallbacks(polling)
                        worker = null
                        main.postDelayed({
                            if (attempt == generation) bind(app, sessionId, options, attempt, retries - 1)
                        }, 500)
                    } else {
                        finish(code)
                    }
                }
            }, speechRequest(app, transcript),
        ) { partial, snapshot ->
            if (attempt == generation) {
                runCatching { update(partial, snapshot, options.translate) }
            }
        }
        worker = request
        try {
            request.bind()
            if (worker === request) main.post(polling)
        } catch (_: RuntimeException) {
            finish(-31)
        }
    }

    private fun update(partial: ByteArray?, snapshot: ByteArray?, translate: Boolean) {
        var current = state.value
        if (snapshot != null) {
            val report = JSONObject(snapshot.decodeToString())
            val lines = report.getJSONArray("lines")
            val revision = lines.optJSONObject(lines.length() - 1)?.getString("revision_id")
            if (current.draining && revision != null && revision != latestFinal) restartDrainTimeout()
            latestFinal = revision
            current = current.copy(
                lines = List(lines.length()) { index ->
                    val line = lines.getJSONObject(index)
                    val translation = line.optJSONObject("translation")
                    LiveLine(line.getString("language"), line.getString("text"), translation?.getString("text"),
                        translation?.optBoolean("stale") ?: false)
                },
                translationPending = translate && report.getBoolean("translation_pending"),
                reason = report.optJSONObject("job")?.optString("reason"),
            )
        }
        val progress = partial?.takeUnless { it.isEmpty() }?.let { JSONObject(it.decodeToString()) }
        val partials = progress?.getJSONArray("partials")
        val processed = progress?.optLong("processed_chunks", current.processedChunks) ?: current.processedChunks
        if (current.draining && processed > current.processedChunks) restartDrainTimeout()
        state.value = current.copy(
            stage = progress?.getString("stage") ?: current.stage,
            processedChunks = processed,
            partials = if (partials == null) emptyList() else List(partials.length()) { index ->
                val line = partials.getJSONObject(index)
                LiveLine(line.optString("language"), line.getString("text"), partial = true)
            },
        )
    }

    private fun finish(code: Int) {
        generation++
        release()
        state.value = state.value.copy(running = false, draining = false, result = code, partials = emptyList())
    }

    private fun release() {
        main.removeCallbacks(polling)
        main.removeCallbacks(timeout)
        worker?.close()
        worker = null
        latestFinal = null
    }
}
