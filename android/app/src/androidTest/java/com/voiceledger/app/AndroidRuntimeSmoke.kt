package com.voiceledger.app

import android.app.Activity
import android.app.Instrumentation
import android.os.Bundle
import com.voiceledger.app.core.AudioLibrary
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.CaptureEnd
import com.voiceledger.app.recording.CaptureTimeline
import com.voiceledger.app.recording.PACKET_FRAMES
import com.voiceledger.app.recording.RecordingManifest
import com.voiceledger.app.recording.StopRequest
import com.voiceledger.app.recording.recoverLibrary
import com.voiceledger.app.speech.SpeechConnection
import com.voiceledger.app.speech.speechRequest
import java.io.File
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
            verifyModelDownload()
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

    private fun verifyModelDownload() {
        val app = targetContext
        val model = "whisper-tiny-multilingual-q5-1"
        fun query(op: String) = JSONObject(NativeCore.speechRequest(speechRequest(app, JSONObject().put("op", op))).decodeToString())
        check(query("live_model").getString("status") == "not_installed")
        val choices = query("inspect").getJSONArray("models")
        val candidate = (0 until choices.length()).map(choices::getJSONObject).first { it.getString("id") == model }
        check(candidate.isNull("blocked_by")) { "Whisper blocked by ${candidate.getString("blocked_by")}." }
        val action = JSONObject().put("op", "install").put("model_id", model).put("approved", true)
        val result = ArrayBlockingQueue<Int>(1)
        val worker = SpeechConnection(app, speechRequest(app, action), { result.offer(it) }) { _, _ -> }
        try {
            runOnMainSync(worker::bind)
            val code = result.poll(150, TimeUnit.SECONDS)
            check(code == 0) { "Whisper installation returned $code." }
        } finally {
            runOnMainSync(worker::close)
        }
        check(query("live_model").getString("status") == "installed")
    }
}
