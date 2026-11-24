package com.voiceledger.app.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

class SpeechBridgeTest {
    @get:Rule val folder = TemporaryFolder()

    @Test
    fun inspectsEmptyLibraryAndRequiresApprovalBeforeDownloading() {
        val root = folder.newFolder("转写")
        val path = root.absolutePath.replace("\\", "\\\\")
        val request = """{"recordings":"$path","profiles":"$path","adapters":"$path","action":%s}"""
        val report = NativeCore.speechRequest(request.format("""{"op":"inspect"}""")).decodeToString()
        assertTrue(report.contains("\"session\":null"))
        assertTrue(report.contains("\"lines\":[]"))
        assertTrue(NativeCore.speechPartial().isEmpty())
        val error = runCatching {
            NativeCore.speechRequest(request.format("""{"op":"install","model_id":"whisper-tiny-multilingual-q5-1","approved":false}"""))
        }.exceptionOrNull()
        assertEquals(-34, (error as CoreException).code)
        assertFalse(File(root, "models").exists())
    }
}
