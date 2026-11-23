package com.voiceledger.app.core

import java.io.File

internal class AudioLibrary(root: File) : AutoCloseable {
    private var handle = NativeCore.openAudio(root.absolutePath.encodeToByteArray())

    fun request(command: String, pcm: FloatArray? = null, count: Int = pcm?.size ?: 0): ByteArray? {
        check(handle != 0L)
        return NativeCore.requestAudio(handle, command.encodeToByteArray(), pcm, count)
    }

    override fun close() {
        if (handle != 0L) {
            NativeCore.releaseAudio(handle)
            handle = 0L
        }
    }
}
