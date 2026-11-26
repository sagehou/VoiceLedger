package com.voiceledger.app

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.Activity
import android.app.Instrumentation
import android.content.Intent
import android.os.Bundle
import android.os.SystemClock
import android.view.accessibility.AccessibilityNodeInfo
import com.voiceledger.app.core.AudioLibrary
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.CaptureEnd
import com.voiceledger.app.recording.CaptureTimeline
import com.voiceledger.app.recording.PACKET_FRAMES
import com.voiceledger.app.recording.RecordingManifest
import com.voiceledger.app.recording.RecordingPhase
import com.voiceledger.app.recording.RecordingStatusStore
import com.voiceledger.app.recording.SAMPLE_RATE
import com.voiceledger.app.recording.SECOND_NS
import com.voiceledger.app.recording.StopRequest
import com.voiceledger.app.recording.recoverLibrary
import com.voiceledger.app.speech.LiveAsrModel
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
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread
import org.json.JSONArray
import org.json.JSONObject

class AndroidRuntimeSmoke : Instrumentation() {
    private var installModel: String? = null
    private var benchmarkModel: String? = null

    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        installModel = arguments?.getString("install_model")
        benchmarkModel = arguments?.getString("benchmark_model")
        start()
    }

    override fun onStart() {
        val result = Bundle()
        var stage = "recording"
        try {
            val app = targetContext
            val id = installModel ?: benchmarkModel
            if (id != null) {
                val model = LiveAsrModel.entries.single { it.id == id }
                if (installModel != null) {
                    stage = "model_download"
                    DeviceProfiler.capture(app)
                    verifyModelDownload(model)
                } else {
                    stage = "live_asr"
                    val report = verifyLiveSpeech(model)
                    File(app.cacheDir, "asr-comparison.json").writeText(report.toString())
                }
                result.putString(stage, "passed")
                finish(Activity.RESULT_OK, result)
                return
            }
            verifyRecording()
            result.putString(stage, "passed")
            stage = "device_profile"
            DeviceProfiler.capture(app)
            val profile = JSONObject(File(app.noBackupFilesDir, "device-profile-v1.json").readText())
            check(profile.getString("platform") == "android" && profile.getString("abi") == "arm64-v8a")
            result.putString(stage, "passed")
            stage = "sensevoice_memory"
            val available = profile.getLong("available_memory_bytes")
            val systemReserve = profile.getLong("low_memory_threshold_bytes")
            val appReserve = profile.getLong("process_memory_bytes")
            val remaining = (available - systemReserve - appReserve).coerceAtLeast(0)
            val oldBudget = remaining / 10 * 3
            result.putString("sensevoice_memory_profile", JSONObject()
                .put("available_bytes", available)
                .put("system_reserve_bytes", systemReserve)
                .put("app_reserve_bytes", appReserve)
                .put("previous_budget_bytes", oldBudget)
                .put("post_reserve_bytes", remaining).toString())
            check(remaining > 0 && systemReserve > 0 && appReserve > 0) { "Missing memory reserves." }
            result.putString(stage, "passed")
            stage = "model_download"
            verifyModelDownload(LiveAsrModel.Whisper)
            result.putString(stage, "passed")
            finish(Activity.RESULT_OK, result)
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
        val corpus = context.assets.open("live-speech-corpus.json").bufferedReader().use { JSONArray(it.readText()) }
        val clips = JSONArray()
        val stages = mutableMapOf<String, Long>()
        val timeline = CaptureTimeline(0)
        val packet = ShortArray(PACKET_FRAMES)
        val silence = ShortArray(SAMPLE_RATE * 2)
        var frames = 0L
        var committed = 0
        var firstCommitMs = -1L
        var firstFinalMs = -1L
        var maxCommitDelayMs = 0L
        val firstTextMs = AtomicLong(-1)
        val firstPartialMs = AtomicLong(-1)
        val observing = AtomicBoolean(true)
        val recordingRoot = File(app.filesDir, "recordings")
        val screen = startActivitySync(Intent(app, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        try {
            val readyBy = SystemClock.elapsedRealtime() + 5_000
            while (RecordingStatusStore.state.value.phase == RecordingPhase.Recovering &&
                SystemClock.elapsedRealtime() < readyBy) SystemClock.sleep(25)
            check(RecordingStatusStore.state.value.phase == RecordingPhase.Idle) { "Recording screen did not finish recovery." }
            val accessibility = uiAutomation
            accessibility.serviceInfo = accessibility.serviceInfo.apply {
                flags = flags or AccessibilityServiceInfo.FLAG_REPORT_VIEW_IDS
            }
            AudioLibrary(recordingRoot).use { library ->
                var origin = 0L
                RecordingManifest(library) { saved, throughNs ->
                    if (saved.chunks > committed) {
                        committed = saved.chunks
                        val elapsed = (SystemClock.elapsedRealtimeNanos() - origin) / 1_000_000
                        if (firstCommitMs < 0) firstCommitMs = elapsed
                        maxCommitDelayMs = maxOf(maxCommitDelayMs, elapsed - throughNs / 1_000_000)
                    }
                }.use { manifest ->
                    manifest.prepare(0)
                    manifest.beginStream(0, null)
                    origin = SystemClock.elapsedRealtimeNanos()
                    runOnMainSync {
                        RecordingStatusStore.update { it.copy(phase = RecordingPhase.Recording) }
                        LiveSpeech.reset()
                        LiveSpeech.start(app, manifest.sessionId, LiveSpeechOptions("auto", false, model))
                    }
                    val startedAt = origin
                    // Observe the Compose captions without putting UI queries on the audio writer.
                    thread(name = "Observe live captions", isDaemon = true) {
                        while (observing.get()) {
                            val state = LiveSpeech.state.value
                            if (state.lines.isNotEmpty() || state.partials.isNotEmpty()) {
                                val visible = mutableSetOf<String>()
                                collectVisibleCaptions(accessibility.rootInActiveWindow, visible)
                                val partial = LIVE_PARTIAL_TAG in visible
                                val elapsed = (SystemClock.elapsedRealtimeNanos() - startedAt) / 1_000_000
                                if (partial || LIVE_FINAL_TAG in visible) firstTextMs.compareAndSet(-1, elapsed)
                                if (partial) firstPartialMs.compareAndSet(-1, elapsed)
                                if (firstTextMs.get() >= 0 && (model != LiveAsrModel.Nemotron || partial)) break
                            }
                            SystemClock.sleep(80)
                        }
                    }
                    fun observe() {
                        val state = LiveSpeech.state.value
                        val elapsed = (SystemClock.elapsedRealtimeNanos() - origin) / 1_000_000
                        state.stage?.let { stages.putIfAbsent(it, elapsed) }
                        if (state.lines.isNotEmpty() || state.partials.isNotEmpty()) {
                            check(committed > 0) { "Text appeared before the first commit." }
                        }
                        if (firstFinalMs < 0 && state.lines.isNotEmpty()) firstFinalMs = elapsed
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
                            observe()
                        }
                    }
                    for (index in 0 until corpus.length()) {
                        val fixture = corpus.getJSONObject(index)
                        val file = fixture.getString("file")
                        val bytes = context.assets.open(file).use { it.readBytes() }
                        val floats = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN).asFloatBuffer()
                        check(floats.remaining() == fixture.getInt("frames"))
                        val samples = ShortArray(floats.remaining()) {
                            (floats.get() * 32768).toInt().coerceIn(-32768, 32767).toShort()
                        }
                        clips.put(JSONObject().put("file", file).put("start_ns", frames * SECOND_NS / SAMPLE_RATE))
                        append(samples)
                        append(silence)
                    }
                    observing.set(false)
                    check(firstTextMs.get() >= firstCommitMs && firstCommitMs >= 0) { "No visible text during recording." }
                    if (model == LiveAsrModel.Nemotron) {
                        check(firstPartialMs.get() >= firstCommitMs) { "No visible streaming partial during recording." }
                    }
                    check(firstFinalMs >= firstCommitMs) { "No final text during recording." }
                    val stop = SystemClock.elapsedRealtime()
                    val saved = manifest.finish(StopRequest(CaptureEnd.User, frames * SECOND_NS / SAMPLE_RATE))
                    check(saved.gaps == 0 && !saved.unknownTail) { "Paced capture lost audio." }
                    runOnMainSync(LiveSpeech::drain)
                    while (LiveSpeech.state.value.running && SystemClock.elapsedRealtime() - stop < 240_000) {
                        SystemClock.sleep(50)
                    }
                    val finished = LiveSpeech.state.value
                    check(finished.result == 0) {
                        "Transcript result ${finished.result}, reason ${finished.reason}, processed ${finished.processedChunks} of ${saved.chunks} chunks."
                    }
                    val completedMs = SystemClock.elapsedRealtime() - stop
                    val export = JSONObject().put("op", "export").put("session_id", manifest.sessionId).put("format", "json")
                    val transcript = JSONObject(NativeCore.speechRequest(speechRequest(app, export)).decodeToString())
                    return JSONObject().put("model", model.id)
                        .put("first_commit_ms", firstCommitMs)
                        .put("first_visible_text_ms", firstTextMs.get())
                        .put("first_visible_partial_ms", firstPartialMs.get().takeIf { it >= 0 } ?: JSONObject.NULL)
                        .put("first_final_ms", firstFinalMs)
                        .put("stages_ms", JSONObject(stages))
                        .put("completion_after_stop_ms", completedMs)
                        .put("max_commit_delay_ms", maxCommitDelayMs)
                        .put("committed_segments", saved.chunks)
                        .put("clips", clips)
                        .put("lines", transcript.getJSONArray("lines"))
                        .put("audio_ms", frames * 1000 / SAMPLE_RATE)
                }
            }
        } finally {
            observing.set(false)
            runOnMainSync {
                RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle) }
                LiveSpeech.reset()
                screen.finish()
            }
        }
    }

    private fun collectVisibleCaptions(node: AccessibilityNodeInfo?, captions: MutableSet<String>) {
        if (node == null || !node.isVisibleToUser) return
        val tag = node.viewIdResourceName
        if ((tag == LIVE_PARTIAL_TAG || tag == LIVE_FINAL_TAG) && !node.text.isNullOrBlank()) {
            captions.add(tag)
        }
        for (index in 0 until node.childCount) collectVisibleCaptions(node.getChild(index), captions)
    }
}
