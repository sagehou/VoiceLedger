package com.voiceledger.app.speech

import android.app.Service
import android.content.Intent
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.os.Process
import android.os.RemoteException
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import kotlin.concurrent.thread
import kotlin.concurrent.timer

internal const val RUN = 1
internal const val RESULT = 2
internal const val PARTIAL = 3

class SpeechWorkerService : Service() {
    private val main = Handler(Looper.getMainLooper())
    private var working = false
    private var owner: Messenger? = null
    private val messenger = Messenger(Handler(Looper.getMainLooper()) { message ->
        when (message.what) {
            RUN -> runRequest(message)
            PARTIAL -> {
                val bytes = if (owner == message.replyTo) runCatching { NativeCore.speechPartial() }.getOrNull() else null
                val reply = Message.obtain(null, PARTIAL)
                reply.data.putByteArray("partial", bytes)
                reply.data.putByteArray("transcript", readTranscript(message.data.getString("transcript")))
                replyTo(message.replyTo, reply)
            }
        }
        true
    })

    override fun onBind(intent: Intent?): IBinder = messenger.binder

    override fun onUnbind(intent: Intent?): Boolean {
        if (working) runCatching { NativeCore.cancelSpeech() }
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        super.onDestroy()
        if (working) Process.killProcess(Process.myPid())
    }

    private fun runRequest(message: Message) {
        val reply = message.replyTo
        val request = message.data.getString("request")
        val transcript = message.data.getString("transcript")
        if (request == null || working) {
            replyTo(reply, Message.obtain(null, RESULT, if (working) -37 else -1, 0))
            return
        }
        working = true
        owner = reply
        thread(name = "VoiceLedger speech") {
            val sampling = timer(name = "VoiceLedger speech resources", daemon = true, initialDelay = 1000L, period = 1000L) {
                runCatching { NativeCore.updateSpeechResources(DeviceProfiler.speechResources(applicationContext)) }
            }
            val outcome = try {
                val priority = if (transcript != null) {
                    Process.THREAD_PRIORITY_DEFAULT + Process.THREAD_PRIORITY_LESS_FAVORABLE
                } else {
                    Process.THREAD_PRIORITY_BACKGROUND
                }
                Process.setThreadPriority(priority)
                NativeCore.updateSpeechResources(DeviceProfiler.speechResources(applicationContext))
                NativeCore.speechRequest(request)
                0
            } catch (failure: CoreException) {
                failure.code
            } catch (_: Exception) {
                -31
            } catch (_: LinkageError) {
                -31
            } finally {
                sampling.cancel()
            }
            val snapshot = readTranscript(transcript)
            main.post {
                working = false
                owner = null
                val completed = Message.obtain(null, RESULT, outcome, 0)
                completed.data.putByteArray("transcript", snapshot)
                replyTo(reply, completed)
            }
        }
    }

    private fun readTranscript(request: String?): ByteArray? =
        request?.let { runCatching { NativeCore.speechRequest(it) }.getOrNull() }

    private fun replyTo(peer: Messenger?, value: Message) {
        try {
            peer?.send(value)
        } catch (_: RemoteException) {
        }
    }
}
