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

internal class SpeechConnection(
    private val context: Context,
    private val request: String,
    private val result: (Int) -> Unit,
    private val partial: (ByteArray?) -> Unit,
) : ServiceConnection, AutoCloseable {
    private var bound = false
    private var service: Messenger? = null
    private val reply = Messenger(Handler(Looper.getMainLooper()) { message ->
        if (bound) {
            when (message.what) {
                RESULT -> finish(message.arg1)
                PARTIAL -> partial(message.data.getByteArray("partial"))
            }
        }
        true
    })

    fun bind() {
        bound = context.bindService(Intent(context, SpeechWorkerService::class.java), this, Context.BIND_AUTO_CREATE)
        if (!bound) result(-31)
    }

    override fun onServiceConnected(name: ComponentName, binder: IBinder) {
        if (!bound) return
        service = Messenger(binder)
        val call = Message.obtain(null, RUN)
        call.data.putString("request", request)
        send(call)
    }

    override fun onServiceDisconnected(name: ComponentName) = finish(-31)
    override fun onBindingDied(name: ComponentName) = finish(-31)
    override fun onNullBinding(name: ComponentName) = finish(-31)

    fun poll() = send(Message.obtain(null, PARTIAL))

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
