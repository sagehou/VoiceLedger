package com.voiceledger.app.recording

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import com.voiceledger.app.MainActivity
import com.voiceledger.app.R
import com.voiceledger.app.core.AudioLibrary
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

class RecordingService : Service() {
    private val main = Handler(Looper.getMainLooper())
    private var active: RecordingControl? = null
    private var destroyed = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            START -> if (active == null) startRecording()
            STOP -> {
                val recording = active
                if (recording == null) {
                    stopSelf(startId)
                } else if (!intent.hasExtra(RECORDING_ORIGIN) ||
                    intent.getLongExtra(RECORDING_ORIGIN, -1) == recording.origin
                ) {
                    recording.request(CaptureEnd.User)
                    RecordingStatusStore.update { it.copy(phase = RecordingPhase.Stopping) }
                }
            }
            else -> if (active == null) stopSelf(startId)
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        destroyed = true
        active?.request(CaptureEnd.Service)
        super.onDestroy()
    }

    private fun startRecording() {
        if (RecordingStatusStore.state.value.phase != RecordingPhase.Preparing) {
            stopSelf()
            return
        }
        val control = RecordingControl(SystemClock::elapsedRealtimeNanos)
        val failure = try {
            when {
                checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED ->
                    CaptureEnd.Permission
                !notificationsEnabled(this) -> CaptureEnd.Notification
                else -> {
                    val notification = buildNotification(control.origin)
                    if (Build.VERSION.SDK_INT >= 30) {
                        startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
                    } else {
                        startForeground(NOTIFICATION_ID, notification)
                    }
                    null
                }
            }
        } catch (_: SecurityException) {
            CaptureEnd.Permission
        } catch (_: IllegalStateException) {
            CaptureEnd.Service
        }
        if (failure != null) {
            stopSelf()
            RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle, failure = failure) }
            return
        }
        active = control
        thread(name = "VoiceLedger recording writer") { record(control) }
    }

    private fun record(control: RecordingControl) {
        val directory = libraryDirectory(applicationContext)
        var saved = RecordingStatusStore.state.value.saved
        var recovery = RecordingStatusStore.state.value.recovery
        var sessionCreated = false
        var failure: CaptureEnd? = null
        try {
            if (filesDir.usableSpace < STORAGE_RESERVE_BYTES) control.request(CaptureEnd.LowStorage)
            if (control.stop == null) {
                AudioLibrary(directory).use { library ->
                    RecordingManifest(library) { committed, elapsed ->
                        saved = committed
                        RecordingStatusStore.update { it.copy(saved = committed, elapsedNs = elapsed) }
                    }.use { manifest ->
                        sessionCreated = true
                        saved = SavedAudio()
                        try {
                            manifest.prepare(control.origin)
                            MicrophoneCapture(applicationContext, control).run(manifest)
                            RecordingStatusStore.update { it.copy(phase = RecordingPhase.Stopping) }
                            saved = manifest.finish(requireNotNull(control.stop))
                        } catch (failure: Exception) {
                            try {
                                manifest.checkpointUnwrittenTail(control.capturedThroughNs)
                            } catch (_: Exception) {
                                // Reconciliation will use the last durable checkpoint.
                            }
                            throw failure
                        }
                    }
                }
            }
            failure = control.stop?.reason?.takeUnless { it == CaptureEnd.User }
        } catch (_: LinkageError) {
            failure = CaptureEnd.Storage
        } catch (_: Exception) {
            failure = CaptureEnd.Storage
            if (sessionCreated) {
                saved = (saved ?: SavedAudio()).copy(unknownTail = true)
                try {
                    val (reconciled, report) = recoverLibrary(directory)
                    saved = reconciled
                    recovery = report ?: recovery
                } catch (_: Exception) {
                    // The next open reconciles pending files when the store is writable again.
                }
            }
        } finally {
            val result = saved
            val report = recovery
            val reason = failure
            main.post {
                active = null
                if (!destroyed) {
                    stopForeground(STOP_FOREGROUND_REMOVE)
                    stopSelf()
                }
                RecordingStatusStore.update {
                    it.copy(
                        phase = RecordingPhase.Idle, saved = result, elapsedNs = 0, failure = reason,
                        thermal = false, lowMemory = false, recovery = report,
                    )
                }
            }
        }
    }

    private fun buildNotification(origin: Long): Notification {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 0, Intent(this, RecordingService::class.java).setAction(STOP)
                .setData(Uri.parse("voiceledger:recording/$origin")).putExtra(RECORDING_ORIGIN, origin),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle(getString(R.string.recording_notification))
            .setContentIntent(open)
            .setOngoing(true)
            .setDeleteIntent(stop)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                    getString(R.string.stop_recording), stop,
                ).build(),
            )
            .apply {
                if (Build.VERSION.SDK_INT >= 31) {
                    setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
                }
            }
            .build()
    }

    companion object {
        private const val START = "com.voiceledger.app.recording.START"
        private const val STOP = "com.voiceledger.app.recording.STOP"
        private const val RECORDING_ORIGIN = "recording_origin"
        private const val CHANNEL = "recording"
        private const val NOTIFICATION_ID = 1
        private val recoveryStarted = AtomicBoolean()

        private fun libraryDirectory(context: Context): File = File(context.filesDir, "recordings")

        internal fun recoverOnLaunch(context: Context) {
            if (!recoveryStarted.compareAndSet(false, true)) return
            val app = context.applicationContext
            thread(name = "VoiceLedger recording recovery") {
                try {
                    val (saved, report) = recoverLibrary(libraryDirectory(app))
                    RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle, saved = saved, recovery = report) }
                } catch (_: LinkageError) {
                    recoveryFailed()
                } catch (_: Exception) {
                    recoveryFailed()
                }
            }
        }

        private fun recoveryFailed() {
            RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle, failure = CaptureEnd.Storage) }
        }

        internal fun createChannel(context: Context) {
            context.getSystemService(NotificationManager::class.java)?.createNotificationChannel(
                NotificationChannel(CHANNEL, context.getString(R.string.recording_channel), NotificationManager.IMPORTANCE_LOW),
            )
        }

        internal fun notificationsEnabled(context: Context): Boolean {
            val manager = context.getSystemService(NotificationManager::class.java) ?: return false
            return manager.areNotificationsEnabled() &&
                (manager.getNotificationChannel(CHANNEL)?.importance ?: NotificationManager.IMPORTANCE_NONE) !=
                NotificationManager.IMPORTANCE_NONE
        }

        internal fun start(context: Context) {
            if (RecordingStatusStore.state.value.phase != RecordingPhase.Idle) return
            RecordingStatusStore.update { it.copy(phase = RecordingPhase.Preparing, elapsedNs = 0, failure = null) }
            try {
                context.startForegroundService(Intent(context, RecordingService::class.java).setAction(START))
            } catch (_: SecurityException) {
                RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle, failure = CaptureEnd.Permission) }
            } catch (_: IllegalStateException) {
                RecordingStatusStore.update { it.copy(phase = RecordingPhase.Idle, failure = CaptureEnd.Service) }
            }
        }

        internal fun stop(context: Context) {
            context.startService(Intent(context, RecordingService::class.java).setAction(STOP))
        }
    }
}
