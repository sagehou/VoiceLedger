package com.voiceledger.app.core

internal object NativeSync {
    fun checkProvider(configuration: ByteArray) {
        check(NativeCore.abiVersion == 1)
        val result = nativeCheckProvider(configuration)
        if (result < 0) throw CoreException(result)
    }

    private external fun nativeCheckProvider(configuration: ByteArray): Int
}
