package com.voiceledger.app

import android.app.Application
import android.util.Log
import com.voiceledger.app.core.DeviceProfiler
import kotlin.concurrent.thread

class VoiceLedgerApplication : Application() {
    override fun onCreate() {
        super.onCreate()
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
