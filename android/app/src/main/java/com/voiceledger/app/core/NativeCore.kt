package com.voiceledger.app.core

internal enum class SessionCommand(val code: Int) {
    Prepare(1), Start(2), Pause(3), Resume(4), Stop(5), Finish(6), Interrupt(7)
}

internal enum class SessionState { Idle, Preparing, Recording, Paused, Stopping, Completed, Interrupted }
internal enum class Compatibility { Unknown, Incompatible, Compatible }
internal enum class Recommendation { RecordingOnly, CandidateForBenchmark }

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

    fun transition(handle: Long, command: SessionCommand, atNs: Long): SessionState =
        SessionState.entries[requireSuccess(nativeTransition(handle, command.code, atNs))]

    fun cancel(handle: Long) {
        requireSuccess(nativeCancel(handle))
    }

    fun release(handle: Long) {
        requireSuccess(nativeRelease(handle))
    }

    fun recommend(compatibility: Compatibility, budgetBytes: Long?, peakBytes: Long?): Recommendation =
        Recommendation.entries[requireSuccess(nativeRecommend(compatibility.ordinal, budgetBytes ?: -1L, peakBytes ?: -1L))]

    private fun requireSuccess(result: Int): Int {
        if (result < 0) throw CoreException(result)
        return result
    }

    private external fun nativeAbiVersion(): Int
    private external fun nativeCreate(): Long
    private external fun nativeTransition(handle: Long, command: Int, atNs: Long): Int
    private external fun nativeCancel(handle: Long): Int
    private external fun nativeRelease(handle: Long): Int
    private external fun nativeRecommend(compatibility: Int, budgetBytes: Long, peakBytes: Long): Int
}
