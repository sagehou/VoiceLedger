package com.voiceledger.app.core

import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class ModelPreviewTest {
    @get:Rule val temp = TemporaryFolder()

    @Test
    fun exposesBothBundledCandidatesAndRecordingOnlyFallbackThroughJni() {
        val preview = NativeCore.modelPreview(temp.newFolder("models").absolutePath).decodeToString()
        assertTrue(preview.contains("\"mode\":\"recording_only\""))
        assertTrue(preview.contains("\"id\":\"whisper-tiny-multilingual-q5-1\""))
        assertTrue(preview.contains("\"id\":\"sensevoice-small-int8\""))
        assertTrue(preview.contains("\"blocked_by\":\"device_profile\""))
        assertTrue(preview.contains("\"download_bytes\":32152673"))
        assertTrue(preview.contains("\"peak_ram_estimate\":{\"bytes\":1100000000"))
    }
}
