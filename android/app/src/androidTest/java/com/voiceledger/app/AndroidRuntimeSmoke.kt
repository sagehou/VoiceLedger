package com.voiceledger.app

import android.app.Activity
import android.app.Instrumentation
import android.os.Bundle
import android.os.SystemClock
import com.voiceledger.app.core.AudioLibrary
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.CaptureEnd
import com.voiceledger.app.recording.CaptureTimeline
import com.voiceledger.app.recording.PACKET_FRAMES
import com.voiceledger.app.recording.RecordingManifest
import com.voiceledger.app.recording.SAMPLE_RATE
import com.voiceledger.app.recording.SECOND_NS
import com.voiceledger.app.recording.StopRequest
import com.voiceledger.app.recording.recoverLibrary
import com.voiceledger.app.speech.LiveAsrModel
import com.voiceledger.app.speech.LiveLine
import com.voiceledger.app.speech.LiveSpeech
import com.voiceledger.app.speech.LiveSpeechOptions
import com.voiceledger.app.speech.SpeechConnection
import com.voiceledger.app.speech.speechRequest
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import org.json.JSONObject

class AndroidRuntimeSmoke : Instrumentation() {
    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        start()
    }

    override fun onStart() {
        val result = Bundle()
        var stage = "recording"
        try {
            verifyRecording()
            result.putString(stage, "passed")
            stage = "device_profile"
            val app = targetContext
            DeviceProfiler.capture(app)
            val profile = JSONObject(File(app.noBackupFilesDir, "device-profile-v1.json").readText())
            check(profile.getString("platform") == "android" && profile.getString("abi") == "arm64-v8a")
            result.putString(stage, "passed")
            stage = "model_download"
            LiveAsrModel.entries.forEach(::verifyModelDownload)
            result.putString(stage, "passed")
            var livePassed = true
            for (model in listOf(LiveAsrModel.SenseVoice, LiveAsrModel.Whisper)) {
                stage = "live_${model.name.lowercase()}"
                try {
                    val report = verifyLiveSpeech(model)
                    result.putString("${stage}_timing", report.toString())
                    val passed = report.optInt("result", -99) == 0 && report.getLong("first_visible_text_ms") >= 0 &&
                        report.getJSONObject("first_final_ms_from_clip_start").length() == 3
                    result.putString(stage, if (passed) "passed" else "failed")
                    livePassed = livePassed && passed
                } catch (error: Exception) {
                    result.putString(stage, "${error.javaClass.simpleName}: ${error.message}")
                    livePassed = false
                }
            }
            finish(if (livePassed) Activity.RESULT_OK else Activity.RESULT_CANCELED, result)
        } catch (error: Throwable) {
            result.putString("failure", "$stage: ${error.javaClass.simpleName}: ${error.message}")
            finish(Activity.RESULT_CANCELED, result)
        }
    }

    private fun verifyRecording() {
        val root = File(targetContext.cacheDir, "recording-smoke-${UUID.randomUUID()}")
        try {
            AudioLibrary(root).use { library ->
                val conflict = runCatching { AudioLibrary(root).close() }.exceptionOrNull()
                check(conflict is CoreException && conflict.code == -21) { "Expected a single recording writer." }
                RecordingManifest(library) { _, _ -> }.use { manifest ->
                    manifest.prepare(0)
                    manifest.beginStream(0, null)
                    val time = CaptureTimeline(0).next(PACKET_FRAMES, 20_000_000, null)
                    manifest.append(ShortArray(PACKET_FRAMES) { (it % 40 * 100).toShort() }, time)
                    val saved = manifest.finish(StopRequest(CaptureEnd.User, time.endNs))
                    check(saved.chunks == 1)
                }
            }
            check(recoverLibrary(root).first?.chunks == 1) { "Committed audio missing after reopen." }
        } finally {
            root.deleteRecursively()
        }
    }

    private fun verifyModelDownload(model: LiveAsrModel) {
        val app = targetContext
        fun query(op: String): JSONObject {
            val action = JSONObject().put("op", op)
            if (op == "live_model") action.put("model_id", model.id)
            return JSONObject(NativeCore.speechRequest(speechRequest(app, action)).decodeToString())
        }
        check(query("live_model").getString("status") == "not_installed") { "${model.id} already installed." }
        val choices = query("inspect").getJSONArray("models")
        val candidate = (0 until choices.length()).map(choices::getJSONObject).first { it.getString("id") == model.id }
        check(candidate.isNull("blocked_by")) { "${model.id} blocked by ${candidate.getString("blocked_by")}." }
        val action = JSONObject().put("op", "install").put("model_id", model.id).put("approved", true)
        val result = ArrayBlockingQueue<Int>(1)
        val worker = SpeechConnection(app, speechRequest(app, action), { result.offer(it) }) { _, _ -> }
        try {
            runOnMainSync(worker::bind)
            val code = result.poll(150, TimeUnit.SECONDS)
            check(code == 0) { "${model.id} installation returned $code." }
        } finally {
            runOnMainSync(worker::close)
        }
        check(query("live_model").getString("status") == "installed")
    }

    private fun verifyLiveSpeech(model: LiveAsrModel): JSONObject {
        val app = targetContext
        val clips = listOf("en" to "english", "ja" to "japanese", "zh" to "mandarin")
            .associate { (language, name) ->
                val bytes = context.assets.open("live-speech-$name.f32").use { it.readBytes() }
                val floats = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).asFloatBuffer()
                language to ShortArray(floats.remaining()) {
                    (floats.get() * 32768).toInt().coerceIn(-32768, 32767).toShort()
                }
            }
        val clipStarts = mutableMapOf<String, Long>()
        val firstFinal = mutableMapOf<String, Long>()
        val seen = mutableSetOf<LiveLine>()
        val stages = mutableMapOf<String, Long>()
        val timeline = CaptureTimeline(0)
        val packet = ShortArray(PACKET_FRAMES)
        val silence = ShortArray(SAMPLE_RATE * 2)
        var frames = 0L
        var committed = 0
        var firstCommitMs = -1L
        var firstTextMs = -1L
        val recordingRoot = File(app.filesDir, "recordings")
        try {
            AudioLibrary(recordingRoot).use { library ->
                var origin = 0L
                RecordingManifest(library) { saved, _ ->
                    committed = saved.chunks
                    if (committed > 0 && firstCommitMs < 0) {
                        firstCommitMs = (SystemClock.elapsedRealtimeNanos() - origin) / 1_000_000
                    }
                }.use { manifest ->
                    manifest.prepare(0)
                    manifest.beginStream(0, null)
                    origin = SystemClock.elapsedRealtimeNanos()
                    runOnMainSync {
                        LiveSpeech.reset()
                        LiveSpeech.start(app, manifest.sessionId, LiveSpeechOptions("auto", false, model))
                    }
                    fun append(samples: ShortArray) {
                        for (offset in samples.indices step PACKET_FRAMES) {
                            val count = minOf(PACKET_FRAMES, samples.size - offset)
                            samples.copyInto(packet, endIndex = offset + count, startIndex = offset)
                            frames += count
                            val time = timeline.next(count, frames * SECOND_NS / SAMPLE_RATE, null)
                            val waitNs = origin + time.endNs - SystemClock.elapsedRealtimeNanos()
                            if (waitNs > 0) SystemClock.sleep((waitNs + 999_999) / 1_000_000)
                            manifest.append(packet, time)
                            val state = LiveSpeech.state.value
                            check(state.running) { "Worker exited during capture with ${state.result}; stage ${state.stage}." }
                            val elapsed = (SystemClock.elapsedRealtimeNanos() - origin) / 1_000_000
                            state.stage?.let { stages.putIfAbsent(it, elapsed) }
                            if (firstTextMs < 0 && (state.lines.isNotEmpty() || state.partials.isNotEmpty())) {
                                check(committed > 0) { "Text appeared before the first commit." }
                                firstTextMs = elapsed
                            }
                            for (line in state.lines) {
                                if (seen.add(line) && line.language in clipStarts) {
                                    firstFinal.putIfAbsent(line.language, elapsed)
                                }
                            }
                        }
                    }
                    for ((language, samples) in clips) {
                        clipStarts[language] = frames * 1000 / SAMPLE_RATE
                        append(samples)
                        append(silence)
                    }
                    repeat(4) {
                        if (firstFinal.size < clips.size) append(silence)
                    }
                    val action = JSONObject().put("op", "live_transcript").put("session_id", manifest.sessionId)
                    fun snapshot(): JSONObject =
                        JSONObject(NativeCore.speechRequest(speechRequest(app, action)).decodeToString())
                    val before = snapshot()
                    val stop = SystemClock.elapsedRealtime()
                    val saved = manifest.finish(StopRequest(CaptureEnd.User, frames * SECOND_NS / SAMPLE_RATE))
                    runOnMainSync(LiveSpeech::drain)
                    while (LiveSpeech.state.value.running && SystemClock.elapsedRealtime() - stop < 50_000) {
                        SystemClock.sleep(50)
                    }
                    val after = snapshot()
                    return JSONObject().put("first_commit_ms", firstCommitMs)
                        .put("first_visible_text_ms", firstTextMs)
                        .put("stages_ms", JSONObject(stages))
                        .put("native_finals_before_stop", before.getJSONArray("lines").length())
                        .put("native_job_before_stop", before.optJSONObject("job"))
                        .put("native_finals_after_stop", after.getJSONArray("lines").length())
                        .put("native_job_after_stop", after.optJSONObject("job"))
                        .put("visible_finals_after_stop", LiveSpeech.state.value.lines.size)
                        .put("result", LiveSpeech.state.value.result)
                        .put("first_final_ms_from_clip_start", JSONObject(firstFinal.mapValues { (language, elapsed) ->
                            elapsed - clipStarts.getValue(language)
                        }))
                        .put("completion_after_stop_ms", SystemClock.elapsedRealtime() - stop)
                        .put("committed_segments", saved.chunks)
                        .put("audio_ms", frames * 1000 / SAMPLE_RATE)
                }
            }
        } finally {
            runOnMainSync(LiveSpeech::reset)
        }
    }
}
