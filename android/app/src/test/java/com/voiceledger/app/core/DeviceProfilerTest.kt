package com.voiceledger.app.core

import android.os.PowerManager
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class DeviceProfilerTest {
    @get:Rule val temp = TemporaryFolder()

    @Test
    fun savesKnownAndUnknownMetricsAndRefreshesLocalIdentityThroughJni() {
        val directory = temp.newFolder("设备").absolutePath
        val first = "06f96206-21a8-4ce2-a1cb-c1e96fb82125"
        val second = "f9a80d7a-b90b-43a3-b573-14216657d837"
        val firstDevice = "956eebb8-5719-450f-b302-5cb689f559d0"
        val secondDevice = "a43e11c4-90ad-4226-a56c-e6f0946d4390"
        val observation = """
            {"id":"$first","device_id":"$firstDevice","observed_at_unix_ms":1,"platform":"android","os_version":"36",
             "logical_cpu_count":4,"available_memory_bytes":0,"low_memory":true,"thermal_state":"severe"}
        """.trimIndent()
        NativeCore.saveDeviceProfile(directory, observation.encodeToByteArray())
        val path = File(directory, "device-profile-v1.json")
        val original = path.readText()
        assertTrue(original.contains("\"total_memory_bytes\":null"))
        assertTrue(original.contains("\"available_memory_bytes\":0"))
        assertTrue(original.contains("\"low_memory\":true"))
        assertTrue(original.contains("\"thermal_state\":\"severe\""))
        assertTrue(original.contains("\"abi\":"))

        val bad = observation.replace("\"low_memory\":true", "\"battery_percent\":101").encodeToByteArray()
        val error = assertThrows(CoreException::class.java) { NativeCore.saveDeviceProfile(directory, bad) }
        assertEquals(-18, error.code)
        assertEquals(original, path.readText())

        val next = observation.replace(first, second).replace(firstDevice, secondDevice)
        NativeCore.saveDeviceProfile(directory, next.encodeToByteArray())
        val updated = path.readText()
        assertTrue(updated.contains("\"id\":\"$second\""))
        assertTrue(updated.contains("\"device_id\":\"$firstDevice\""))
        assertFalse(updated.contains(secondDevice))
    }

    @Test
    fun distinguishesNormalPressureFromUnknownThermalStatus() {
        assertEquals("none", thermalState(PowerManager.THERMAL_STATUS_NONE))
        assertEquals("severe", thermalState(PowerManager.THERMAL_STATUS_SEVERE))
        assertEquals("shutdown", thermalState(PowerManager.THERMAL_STATUS_SHUTDOWN))
        assertNull(thermalState(-1))
    }
}
