package com.voiceledger.app.core

internal object NativeVault {
    fun createKey(): ByteArray = request(1)

    fun identity(recovery: ByteArray): ByteArray = request(4, recovery)

    fun rotate(recovery: ByteArray): ByteArray = request(5, recovery)

    fun seal(recovery: ByteArray, data: ByteArray): ByteArray = request(2, recovery, data = data)

    fun open(recovery: ByteArray, objectId: ByteArray, envelope: ByteArray): ByteArray =
        request(3, recovery, objectId, envelope)

    private fun request(
        operation: Int,
        recovery: ByteArray = byteArrayOf(),
        expected: ByteArray = byteArrayOf(),
        data: ByteArray = byteArrayOf(),
    ): ByteArray {
        check(NativeCore.abiVersion == 1)
        val output = arrayOfNulls<ByteArray>(1)
        val code = nativeRequest(operation, recovery, expected, data, output)
        if (code < 0) throw CoreException(code)
        return requireNotNull(output[0])
    }

    private external fun nativeRequest(
        operation: Int,
        recovery: ByteArray,
        expected: ByteArray,
        data: ByteArray,
        output: Array<ByteArray?>,
    ): Int
}
