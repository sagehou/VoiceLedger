package com.voiceledger.app.core

internal object NativeSync {
    fun checkProvider(configuration: ByteArray) {
        check(NativeCore.abiVersion == 1)
        val result = nativeCheckProvider(configuration)
        if (result < 0) throw CoreException(result)
    }

    fun transfer(command: ByteArray, configuration: ByteArray, recovery: ByteArray, successor: ByteArray = byteArrayOf()): ByteArray {
        check(NativeCore.abiVersion == 1)
        val output = arrayOfNulls<ByteArray>(1)
        val result = nativeTransfer(command, configuration, recovery, successor, output)
        if (result < 0) throw CoreException(result)
        return requireNotNull(output[0])
    }

    private external fun nativeTransfer(
        command: ByteArray, configuration: ByteArray, recovery: ByteArray, successor: ByteArray, output: Array<ByteArray?>,
    ): Int

    private external fun nativeCheckProvider(configuration: ByteArray): Int
}
