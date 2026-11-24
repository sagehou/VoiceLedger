package com.voiceledger.app

import android.app.Application
import android.app.ActivityManager
import android.os.Build
import android.os.Process
import android.util.Log
import com.voiceledger.app.core.DeviceProfiler
import kotlin.concurrent.thread

class VoiceLedgerApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        val process = if (Build.VERSION.SDK_INT >= 28) Application.getProcessName() else {
            getSystemService(ActivityManager::class.java)?.runningAppProcesses
                ?.firstOrNull { it.pid == Process.myPid() }?.processName
        }
        if (process?.endsWith(":speech") == true) return
        thread(name = "VoiceLedger device profile") {
            try {
                DeviceProfiler.capture(this)
            } catch (_: Exception) {
                Log.w("VoiceLedger", "Device profile unavailable.")
            } catch (_: LinkageError) {
                Log.w("VoiceLedger", "Device profile unavailable.")
            }
        }
    }
}
