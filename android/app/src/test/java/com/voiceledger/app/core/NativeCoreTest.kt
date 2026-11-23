package com.voiceledger.app.core

import java.io.File
import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NativeCoreTest {
    @Test
    fun consumesSharedGoldenFixtureThroughJni() {
        val fixture = File(requireNotNull(System.getProperty("voiceledger.ffi.fixture")))
        val sessions = mutableMapOf<String, CoreSession>()
        var cases = 0
        try {
            fixture.forEachLine { line ->
                if (line.isNotBlank() && !line.startsWith('#')) {
                    val fields = line.split('\t')
                    val actual = outcome {
                        when (fields[0]) {
                            "session" -> sessions.getOrPut(fields[1]) { CoreSession() }.apply(
                                SessionCommand.entries.single { it.code == fields[2].toInt() },
                                fields[3].toLong(),
                            ).ordinal
                            "recommend" -> NativeCore.recommend(
                                Compatibility.entries[fields[1].toInt()],
                                memory(fields[2]),
                                memory(fields[3]),
                            ).ordinal
                            else -> error("Unknown golden fixture operation ${fields[0]}.")
                        }
                    }
                    assertEquals(line, fields[4].toInt(), actual)
                    cases++
                }
            }
        } finally {
            sessions.values.forEach(CoreSession::close)
        }
        assertTrue("The golden fixture contains no test cases", cases > 0)
        assertEquals(1, NativeCore.abiVersion)
        val fixtureHash = MessageDigest.getInstance("SHA-256").digest(fixture.readBytes())
            .joinToString("") { "%02x".format(it) }
        println("JNI ABI ${NativeCore.abiVersion} passed $cases fixture cases.")
        println("Fixture SHA-256 $fixtureHash")
    }

    @Test
    fun repeatedlyCreatesCancelsAndReleasesOwnedSessions() {
        val cycles = 128
        repeat(cycles) {
            CoreSession().use { session ->
                assertEquals(SessionState.Preparing, session.apply(SessionCommand.Prepare, 10))
                session.cancel()
                session.cancel()
                assertEquals(-4, outcome { session.apply(SessionCommand.Start, 20).ordinal })
            }
        }
        println("JNI passed $cycles ownership cycles.")
    }

    private fun memory(value: String): Long? = if (value == "-1") null else value.toLong()

    private fun outcome(operation: () -> Int): Int = try {
        operation()
    } catch (error: CoreException) {
        error.code
    }
}
