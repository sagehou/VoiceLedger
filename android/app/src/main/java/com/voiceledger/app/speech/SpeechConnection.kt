package com.voiceledger.app.speech

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Message
import android.os.Messenger
import android.os.RemoteException
import java.io.File
import org.json.JSONObject

internal fun speechRequest(context: Context, action: JSONObject): String = JSONObject()
    .put("recordings", File(context.filesDir, "recordings").absolutePath)
    .put("profiles", context.noBackupFilesDir.absolutePath)
    .put("adapters", context.applicationInfo.nativeLibraryDir)
    .put("action", action)
    .toString()

internal class SpeechConnection(
    private val context: Context,
    private val request: String,
    private val result: (Int) -> Unit,
    private val transcript: String? = null,
    private val progress: (ByteArray?, ByteArray?) -> Unit,
) : ServiceConnection, AutoCloseable {
    private var bound = false
    private var service: Messenger? = null
    private val reply = Messenger(Handler(Looper.getMainLooper()) { message ->
        if (bound) {
            when (message.what) {
                RESULT -> {
                    if (transcript != null) progress(null, message.data.getByteArray("transcript"))
                    finish(message.arg1)
                }
                PARTIAL -> progress(message.data.getByteArray("partial"), message.data.getByteArray("transcript"))
            }
        }
        true
    })

    fun bind() {
        val flags = Context.BIND_AUTO_CREATE or (if (transcript != null) Context.BIND_NOT_FOREGROUND else 0)
        bound = context.bindService(Intent(context, SpeechWorkerService::class.java), this, flags)
        if (!bound) result(-31)
    }

    override fun onServiceConnected(name: ComponentName, binder: IBinder) {
        if (!bound) return
        service = Messenger(binder)
        val call = Message.obtain(null, RUN)
        call.data.putString("request", request)
        call.data.putString("transcript", transcript)
        send(call)
    }

    override fun onServiceDisconnected(name: ComponentName) = finish(-31)
    override fun onBindingDied(name: ComponentName) = finish(-31)
    override fun onNullBinding(name: ComponentName) = finish(-31)

    fun poll() {
        val message = Message.obtain(null, PARTIAL)
        message.data.putString("transcript", transcript)
        send(message)
    }

    private fun send(message: Message) {
        val peer = service ?: return
        message.replyTo = reply
        try {
            peer.send(message)
        } catch (_: RemoteException) {
            finish(-31)
        }
    }

    private fun finish(code: Int) {
        if (!bound) return
        close()
        result(code)
    }

    override fun close() {
        if (bound) {
            bound = false
            service = null
            context.unbindService(this)
        }
    }
}
