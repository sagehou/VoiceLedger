package com.voiceledger.app.core

import java.io.File
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class VaultBridgeTest {
    @Test
    fun sharesTheVaultVectorAndRejectsTampering() {
        val fixture = File(requireNotNull(System.getProperty("voiceledger.vault.fixture"))).readText()
        val fields = Regex(""""([a-z_]+)": "([^"]*)"""").findAll(fixture)
            .associate { it.groupValues[1] to it.groupValues[2] }
        val recovery = fields.getValue("recovery_code").encodeToByteArray()
        val generated = NativeVault.createKey()
        val plaintext = fields.getValue("plaintext").encodeToByteArray()
        try {
            val expected = hex(fields.getValue("object_id"))
            val envelope = hex(fields.getValue("envelope"))
            assertArrayEquals(plaintext, NativeVault.open(recovery, expected, envelope))
            val fresh = NativeVault.seal(generated, plaintext)
            val id = fresh.copyOfRange(8, 24)
            assertArrayEquals(plaintext, NativeVault.open(generated, id, fresh))
            fresh[fresh.lastIndex] = (fresh.last().toInt() xor 1).toByte()
            val failure = assertThrows(CoreException::class.java) { NativeVault.open(generated, id, fresh) }
            assertEquals(-50, failure.code)
        } finally {
            recovery.fill(0)
            generated.fill(0)
        }
    }

    private fun hex(hex: String): ByteArray =
        ByteArray(hex.length / 2) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
}
