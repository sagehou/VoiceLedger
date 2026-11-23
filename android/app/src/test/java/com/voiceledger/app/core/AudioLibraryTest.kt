package com.voiceledger.app.core

import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class AudioLibraryTest {
    @get:Rule val temp = TemporaryFolder()

    @Test
    fun persistsOpusAndReconcilesPendingTailThroughJni() {
        val root = temp.newFolder("recording库")
        val session = "c69eda02-38ea-4d03-a14c-ec0d92e51016"
        val track = "65459b38-ec60-4d0f-8708-b5f1b4ea8136"
        val committed = "f7ea8da4-374c-433f-b872-c704f4678c85"
        val pending = "c9fe7d2d-0682-437f-882b-873b3a46b09e"
        AudioLibrary(root).use { library ->
            library.request(
                """{"op":"create_session","id":"$session","created_at_ms":1,"clock":"android_boottime","clock_origin":1,
                    "tracks":[{"id":"$track","source":"mic","endpoint_id":"android_default_mic"}]}""",
            )
            library.request(
                """{"op":"begin_stream","track_id":"$track","epoch":0,"start_ns":0,"rate":48000,"channels":1,
                    "sample_format":"pcm_s16","native_format":"android PCM"}""",
            )
            library.request("""{"op":"session_state","id":"$session","state":2,"elapsed_ns":0,"snapshot":{}}""")
            library.request(
                """{"op":"begin_chunk","id":"$committed","track_id":"$track","epoch":0,"start_ns":0,"end_ns":20000000}""",
            )
            val pcm = FloatArray(1920) { ((it % 64) - 32) / 128f }
            val seal = """{"op":"seal_chunk","id":"$committed","end_ns":20000000,"packets":[
                {"start_ns":0,"end_ns":20000000,"frames":960,"device_position_frames":0,"qpc100ns":0,
                "flags":0,"estimated_timestamp":false}]}"""
            val invalidCount = runCatching { library.request(seal, pcm, pcm.size + 1) }.exceptionOrNull()
            assertEquals(-1, (invalidCount as CoreException).code)
            assertNull(library.request(seal, pcm, 960))
            library.request(
                """{"op":"begin_chunk","id":"$pending","track_id":"$track","epoch":0,"start_ns":20000000,
                    "end_ns":40000000}""",
            )
        }
        assertTrue(File(root, "audio/$session/$track/$committed.opus").isFile)

        AudioLibrary(root).use { library ->
            val recovery = library.read("""{"op":"recovery"}""")
            assertTrue(recovery.contains("\"interrupted_sessions\":1"))
            assertTrue(recovery.contains("\"known_tail_loss_ns\":20000000"))
            assertTrue(library.read("""{"op":"history"}""").contains("\"interrupted_tail\":true"))
            val timeline = library.read("""{"op":"timeline","track_id":"$track"}""")
            assertTrue(timeline.contains("\"kind\":\"audio\""))
            assertTrue(timeline.contains("\"kind\":\"gap\",\"cause\":\"uncommitted_tail\""))
            val playback = ByteBuffer.wrap(
                requireNotNull(library.request("""{"op":"seek","track_id":"$track","position_ns":0}""")),
            ).order(ByteOrder.LITTLE_ENDIAN)
            assertEquals(1, playback.int)
            assertEquals(20_000_000L, playback.long)
            assertEquals(960 * Float.SIZE_BYTES, playback.remaining())
        }
        AudioLibrary(root).use { library ->
            assertTrue(library.read("""{"op":"recovery"}""").contains("\"interrupted_sessions\":0"))
        }
    }

    private fun AudioLibrary.read(command: String): String =
        requireNotNull(request(command)).decodeToString()
}
