package com.voiceledger.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.format.DateUtils
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import com.voiceledger.app.recording.CaptureEnd
import com.voiceledger.app.recording.RecordingPhase
import com.voiceledger.app.recording.RecordingService
import com.voiceledger.app.recording.RecordingStatus
import com.voiceledger.app.recording.RecordingStatusStore
import com.voiceledger.app.recording.SECOND_NS

class MainActivity : ComponentActivity() {
    private var startRequested = false
    private val microphonePermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (startRequested) {
                if (granted) allowNotificationsAndStart() else rejectStart(CaptureEnd.Permission)
            }
        }
    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (startRequested) {
                if (granted) startIfVisible() else rejectStart(CaptureEnd.Notification)
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        RecordingService.createChannel(this)
        RecordingService.recoverOnLaunch(this)
        setContent {
            val status by RecordingStatusStore.state.collectAsState()
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    Box(Modifier.safeDrawingPadding().padding(24.dp), contentAlignment = Alignment.Center) {
                        RecordingScreen(
                            status, ::requestStart,
                            { RecordingService.stop(this@MainActivity) },
                            { startActivity(Intent(this@MainActivity, ModelsActivity::class.java)) },
                            { startActivity(Intent(this@MainActivity, ModelsActivity::class.java).putExtra("sessions", true)) },
                            { startActivity(Intent(this@MainActivity, SyncPrivacyActivity::class.java)) },
                        )
                    }
                }
            }
        }
    }

    private fun requestStart() {
        if (startRequested || !lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) return
        startRequested = true
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            microphonePermission.launch(Manifest.permission.RECORD_AUDIO)
        } else {
            allowNotificationsAndStart()
        }
    }

    private fun allowNotificationsAndStart() {
        if (!lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
            startRequested = false
            return
        }
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            startIfVisible()
        }
    }

    private fun startIfVisible() {
        startRequested = false
        if (!lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) return
        when {
            checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED ->
                rejectStart(CaptureEnd.Permission)
            !RecordingService.notificationsEnabled(this) -> rejectStart(CaptureEnd.Notification)
            else -> RecordingService.start(this)
        }
    }

    private fun rejectStart(failure: CaptureEnd) {
        startRequested = false
        RecordingStatusStore.update {
            if (it.phase == RecordingPhase.Idle) it.copy(failure = failure) else it
        }
    }
}

@Composable
private fun RecordingScreen(
    status: RecordingStatus,
    start: () -> Unit,
    stop: () -> Unit,
    openModels: () -> Unit,
    openSessions: () -> Unit,
    openSyncPrivacy: () -> Unit,
) {
    val recording = status.phase == RecordingPhase.Preparing || status.phase == RecordingPhase.Recording
    Column(
        modifier = Modifier.verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineLarge)
        val phase = when (status.phase) {
            RecordingPhase.Recovering -> R.string.recovering_recordings
            RecordingPhase.Preparing -> R.string.preparing_recording
            RecordingPhase.Recording -> R.string.recording_in_progress
            RecordingPhase.Stopping -> R.string.saving_recording
            RecordingPhase.Idle -> null
        }
        phase?.let { Text(stringResource(it)) }
        if (status.phase == RecordingPhase.Recording) {
            Text(DateUtils.formatElapsedTime(status.elapsedNs / SECOND_NS))
        }
        status.saved?.let {
            if (it.chunks > 0) {
                Text(stringResource(R.string.saved_audio, it.chunks, DateUtils.formatElapsedTime(it.throughNs / SECOND_NS)))
            } else if (status.phase == RecordingPhase.Idle) {
                Text(stringResource(R.string.no_saved_audio))
            }
            if (it.gaps > 0) Text(stringResource(R.string.saved_gaps, it.gaps))
            if (it.unknownTail) Text(stringResource(R.string.unknown_tail))
        }
        status.recovery?.takeIf { status.phase == RecordingPhase.Idle }?.let {
            Text(
                stringResource(
                    R.string.recovery_report, it.interrupted, it.recoveredChunks,
                    it.knownTailLossNs / 1_000_000, it.elapsedMs,
                ),
            )
        }
        if (status.thermal) Text(stringResource(R.string.thermal_pressure))
        if (status.lowMemory) Text(stringResource(R.string.memory_pressure))
        status.failure?.let { Text(stringResource(it.message()), color = MaterialTheme.colorScheme.error) }
        Button(
            onClick = if (recording) stop else start,
            enabled = recording || status.phase == RecordingPhase.Idle,
        ) {
            Text(stringResource(if (recording) R.string.stop_recording else R.string.start_recording))
        }
        Button(onClick = openSessions) { Text(stringResource(R.string.sessions)) }
        Button(onClick = openModels) { Text(stringResource(R.string.models)) }
        Button(onClick = openSyncPrivacy) { Text(stringResource(R.string.sync_privacy)) }
    }
}

private fun CaptureEnd.message(): Int = when (this) {
    CaptureEnd.User, CaptureEnd.Service -> R.string.service_interrupted
    CaptureEnd.Permission -> R.string.microphone_permission_required
    CaptureEnd.Notification -> R.string.recording_notifications_required
    CaptureEnd.Route -> R.string.microphone_route_changed
    CaptureEnd.Contention -> R.string.microphone_contended
    CaptureEnd.Input -> R.string.microphone_unavailable
    CaptureEnd.Backpressure -> R.string.recording_backpressure
    CaptureEnd.LowStorage -> R.string.recording_low_storage
    CaptureEnd.Storage -> R.string.recording_storage_error
}
