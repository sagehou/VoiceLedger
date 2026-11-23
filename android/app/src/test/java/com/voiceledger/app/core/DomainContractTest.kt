package com.voiceledger.app.core

import java.io.File
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class DomainContractTest {
    @Test
    fun validatesSharedDomainFixturesThroughJni() {
        val directory = File(requireNotNull(System.getProperty("voiceledger.domain.fixtures")))
        val index = File(directory, "cases.tsv")
        var count = 0
        index.forEachLine { row ->
            val (name, expected, digest) = row.split('\t')
            val data = File(directory, "$name.json").readBytes()
            assertEquals("Fixture digest for $name", digest, sha256(data))
            val actual = try {
                NativeCore.validateDomain(data)
            } catch (error: CoreException) {
                error.code
            }
            assertEquals(name, expected.toInt(), actual)
            count++
        }
        assertTrue("The domain fixture suite contains no cases", count > 0)
        println("Domain schema v1 passed $count cases. Suite SHA-256 ${sha256(index.readBytes())}")
    }

    private fun sha256(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256")
        .digest(bytes).joinToString("") { "%02x".format(it) }
}
