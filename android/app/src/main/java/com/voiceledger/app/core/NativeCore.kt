package com.voiceledger.app.core

internal enum class SessionCommand(val code: Int) {
    Prepare(1), Start(2), Pause(3), Resume(4), Stop(5), Finish(6), Interrupt(7)
}

internal enum class SessionState(val code: Int) {
    Idle(0), Preparing(1), Recording(2), Paused(3), Stopping(4), Completed(5), Interrupted(6)
}

internal enum class Compatibility(val code: Int) { Unknown(0), Incompatible(1), Compatible(2) }
internal enum class Recommendation(val code: Int) { RecordingOnly(0), CandidateForBenchmark(1) }

internal class CoreException(val code: Int) : IllegalStateException("Shared core error $code.")

internal class CoreSession : AutoCloseable {
    private var handle = NativeCore.create()

    @Synchronized
    fun apply(command: SessionCommand, atNs: Long): SessionState =
        NativeCore.transition(requireHandle(), command, atNs)

    @Synchronized
    fun cancel() = NativeCore.cancel(requireHandle())

    @Synchronized
    override fun close() {
        if (handle != 0L) {
            NativeCore.release(handle)
            handle = 0L
        }
    }

    private fun requireHandle(): Long {
        check(handle != 0L) { "The shared core session is closed." }
        return handle
    }
}

internal object NativeCore {
    val abiVersion: Int

    init {
        System.loadLibrary("voiceledger_ffi")
        abiVersion = nativeAbiVersion()
        check(abiVersion == 1) { "Unsupported shared core ABI $abiVersion." }
    }

    fun create(): Long {
        val result = nativeCreate()
        if (result < 0L) throw CoreException(result.toInt())
        return result
    }

    fun transition(handle: Long, command: SessionCommand, atNs: Long): SessionState {
        val result = requireSuccess(nativeTransition(handle, command.code, atNs))
        return SessionState.entries.single { it.code == result }
    }

    fun cancel(handle: Long) {
        requireSuccess(nativeCancel(handle))
    }

    fun release(handle: Long) {
        requireSuccess(nativeRelease(handle))
    }

    fun validateDomain(data: ByteArray): Int = requireSuccess(nativeValidateDomain(data))

    fun openAudio(path: ByteArray): Long {
        val result = nativeAudioOpenV1(path)
        if (result < 0L) throw CoreException(result.toInt())
        return result
    }

    fun requestAudio(handle: Long, command: ByteArray, pcm: FloatArray?, count: Int): ByteArray? {
        val output = arrayOfNulls<ByteArray>(1)
        requireSuccess(nativeAudioRequestV1(handle, command, pcm, count, output))
        return output[0]
    }

    fun releaseAudio(handle: Long) {
        requireSuccess(nativeAudioReleaseV1(handle))
    }

    fun recommend(compatibility: Compatibility, budgetBytes: Long?, peakBytes: Long?): Recommendation {
        val result = requireSuccess(nativeRecommend(compatibility.code, budgetBytes ?: -1L, peakBytes ?: -1L))
        return Recommendation.entries.single { it.code == result }
    }

    private fun requireSuccess(result: Int): Int {
        if (result < 0) throw CoreException(result)
        return result
    }

    private external fun nativeAbiVersion(): Int
    private external fun nativeValidateDomain(data: ByteArray): Int
    private external fun nativeCreate(): Long
    private external fun nativeTransition(handle: Long, command: Int, atNs: Long): Int
    private external fun nativeCancel(handle: Long): Int
    private external fun nativeRelease(handle: Long): Int
    private external fun nativeAudioOpenV1(path: ByteArray): Long
    private external fun nativeAudioRequestV1(
        handle: Long,
        command: ByteArray,
        pcm: FloatArray?,
        count: Int,
        output: Array<ByteArray?>,
    ): Int
    private external fun nativeAudioReleaseV1(handle: Long): Int
    private external fun nativeRecommend(compatibility: Int, budgetBytes: Long, peakBytes: Long): Int
}
