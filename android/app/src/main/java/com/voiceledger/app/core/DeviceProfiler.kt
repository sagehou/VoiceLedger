package com.voiceledger.app.core

import android.app.ActivityManager
import android.content.Context
import android.os.BatteryManager
import android.os.Build
import android.os.Debug
import android.os.PowerManager
import android.os.StatFs
import java.util.UUID
import org.json.JSONObject

internal object DeviceProfiler {
    fun capture(context: Context) {
        val activity = context.getSystemService(ActivityManager::class.java)
        val memory = activity?.let { manager ->
            ActivityManager.MemoryInfo().also { manager.getMemoryInfo(it) }.takeIf { it.totalMem > 0 }
        }
        val power = context.getSystemService(PowerManager::class.java)
        val thermal = if (Build.VERSION.SDK_INT >= 29) power?.currentThermalStatus?.let(::thermalState) else null
        val battery = context.getSystemService(BatteryManager::class.java)
            ?.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
            ?.takeIf { it in 0..100 }
        val storage = try {
            StatFs(context.filesDir.absolutePath).availableBytes
        } catch (_: IllegalArgumentException) {
            null
        }
        val measurement = JSONObject(
            mapOf(
                "id" to UUID.randomUUID().toString(),
                "device_id" to UUID.randomUUID().toString(),
                "observed_at_unix_ms" to System.currentTimeMillis(),
                "platform" to "android",
                "os_version" to Build.VERSION.SDK_INT.toString(),
                "logical_cpu_count" to Runtime.getRuntime().availableProcessors(),
                "total_memory_bytes" to memory?.totalMem,
                "available_memory_bytes" to memory?.availMem,
                "low_memory_threshold_bytes" to memory?.threshold?.takeIf { it > 0 },
                "process_memory_bytes" to Debug.getPss().takeIf { it > 0 }?.times(1024L),
                "low_memory" to memory?.lowMemory,
                "low_ram_device" to activity?.isLowRamDevice,
                "storage_available_bytes" to storage,
                "thermal_state" to thermal,
                "power_saver" to power?.isPowerSaveMode,
                "battery_percent" to battery,
            ),
        )
        NativeCore.saveDeviceProfile(context.noBackupFilesDir.absolutePath, measurement.toString().encodeToByteArray())
    }
}

internal fun thermalState(status: Int): String? = when (status) {
    PowerManager.THERMAL_STATUS_NONE -> "none"
    PowerManager.THERMAL_STATUS_LIGHT -> "light"
    PowerManager.THERMAL_STATUS_MODERATE -> "moderate"
    PowerManager.THERMAL_STATUS_SEVERE -> "severe"
    PowerManager.THERMAL_STATUS_CRITICAL -> "critical"
    PowerManager.THERMAL_STATUS_EMERGENCY -> "emergency"
    PowerManager.THERMAL_STATUS_SHUTDOWN -> "shutdown"
    else -> null
}
