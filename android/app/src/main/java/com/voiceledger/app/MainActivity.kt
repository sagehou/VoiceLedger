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
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTag
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.CaptureEnd
import com.voiceledger.app.recording.RecordingPhase
import com.voiceledger.app.recording.RecordingService
import com.voiceledger.app.recording.RecordingStatus
import com.voiceledger.app.recording.RecordingStatusStore
import com.voiceledger.app.recording.SECOND_NS
import com.voiceledger.app.speech.LiveAsrModel
import com.voiceledger.app.speech.LiveSpeech
import com.voiceledger.app.speech.LiveSpeechOptions
import com.voiceledger.app.speech.LiveSpeechStatus
import com.voiceledger.app.speech.speechRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

internal const val LIVE_PARTIAL_TAG = "voiceledger_live_partial"
internal const val LIVE_FINAL_TAG = "voiceledger_live_final"

class MainActivity : ComponentActivity() {
    private var startRequested = false
    private var pendingSpeech: LiveSpeechOptions? = null
    private var liveModels by mutableStateOf<Map<LiveAsrModel, String>>(emptyMap())
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
            val liveSpeech by LiveSpeech.state.collectAsState()
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    Box(Modifier.safeDrawingPadding().padding(24.dp), contentAlignment = Alignment.Center) {
                        RecordingScreen(
                            status, liveSpeech, liveModels, ::requestStart,
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

    override fun onResume() {
        super.onResume()
        lifecycleScope.launch {
            liveModels = withContext(Dispatchers.IO) {
                LiveAsrModel.entries.associateWith { model ->
                    runCatching {
                        val action = JSONObject().put("op", "live_model").put("model_id", model.id)
                        val request = speechRequest(applicationContext, action)
                        JSONObject(NativeCore.speechRequest(request).decodeToString()).getString("status")
                    }.getOrDefault("unavailable")
                }
            }
        }
    }

    private fun requestStart(speech: LiveSpeechOptions?) {
        if (startRequested || !lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) return
        startRequested = true
        pendingSpeech = speech
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            microphonePermission.launch(Manifest.permission.RECORD_AUDIO)
        } else {
            allowNotificationsAndStart()
        }
    }

    private fun allowNotificationsAndStart() {
        if (!lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) {
            startRequested = false
            pendingSpeech = null
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
        val speech = pendingSpeech
        pendingSpeech = null
        if (!lifecycle.currentState.isAtLeast(Lifecycle.State.STARTED)) return
        when {
            checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED ->
                rejectStart(CaptureEnd.Permission)
            !RecordingService.notificationsEnabled(this) -> rejectStart(CaptureEnd.Notification)
            else -> RecordingService.start(this, speech)
        }
    }

    private fun rejectStart(failure: CaptureEnd) {
        startRequested = false
        pendingSpeech = null
        RecordingStatusStore.update {
            if (it.phase == RecordingPhase.Idle) it.copy(failure = failure) else it
        }
    }
}

@Composable
private fun RecordingScreen(
    status: RecordingStatus,
    speech: LiveSpeechStatus,
    liveModels: Map<LiveAsrModel, String>,
    start: (LiveSpeechOptions?) -> Unit,
    stop: () -> Unit,
    openModels: () -> Unit,
    openSessions: () -> Unit,
    openSyncPrivacy: () -> Unit,
) {
    var transcribe by rememberSaveable { mutableStateOf(false) }
    var language by rememberSaveable { mutableStateOf("auto") }
    var translate by rememberSaveable { mutableStateOf(false) }
    var model by rememberSaveable { mutableStateOf(LiveAsrModel.Whisper) }
    val idle = status.phase == RecordingPhase.Idle
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
        if (idle) {
            RecordingOption(R.string.live_transcription, transcribe) { transcribe = it }
            if (transcribe) {
                LiveModelPicker(model) { model = it }
                LanguagePicker(language, enabled = true) { language = it }
                RecordingOption(R.string.live_translate, translate) { translate = it }
                val modelStatus = when (liveModels[model]) {
                    "installed", null -> null
                    "not_installed" -> R.string.live_install
                    else -> R.string.live_worker_unavailable
                }
                modelStatus?.let { Text(stringResource(it)) }
            }
        }
        Button(
            onClick = if (recording) stop else {
                { start(if (transcribe) LiveSpeechOptions(language, translate, model) else null) }
            },
            enabled = recording || idle,
        ) {
            Text(stringResource(if (recording) R.string.stop_recording else R.string.start_recording))
        }
        LiveTranscript(speech)
        Button(onClick = openSessions) { Text(stringResource(R.string.sessions)) }
        Button(onClick = openModels) { Text(stringResource(R.string.models)) }
        Button(onClick = openSyncPrivacy) { Text(stringResource(R.string.sync_privacy)) }
    }
}

@Composable
private fun LiveModelPicker(model: LiveAsrModel, select: (LiveAsrModel) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        Button(onClick = { expanded = true }) {
            Text(stringResource(R.string.live_model, stringResource(model.label)))
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            LiveAsrModel.entries.forEach { choice ->
                DropdownMenuItem(text = { Text(stringResource(choice.label)) }, onClick = {
                    select(choice)
                    expanded = false
                })
            }
        }
    }
}

@Composable
private fun RecordingOption(label: Int, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        modifier = Modifier.toggleable(checked, role = Role.Checkbox, onValueChange = onChange).padding(8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Checkbox(checked, onCheckedChange = null)
        Text(stringResource(label))
    }
}

@Composable
private fun LiveTranscript(status: LiveSpeechStatus) {
    val message = when {
        status.draining -> R.string.live_draining
        status.running -> when (status.stage) {
            "loading" -> R.string.live_loading
            "waiting_for_audio" -> R.string.live_waiting_audio
            "listening" -> if (status.lines.isEmpty() && status.partials.isEmpty()) R.string.live_listening else R.string.live_active
            else -> R.string.live_starting
        }
        status.result == null -> null
        status.result == 0 -> if (status.lines.isEmpty()) R.string.live_no_speech else R.string.live_complete
        status.result == -30 -> R.string.live_storage_error
        status.result == -31 -> R.string.live_worker_unavailable
        status.result == -32 -> R.string.speech_no_audio
        status.result == -33 -> R.string.live_install
        status.result == -34 -> R.string.live_blocked
        status.result == -36 -> R.string.speech_integrity
        status.result == -37 -> R.string.speech_busy
        status.result == -38 -> when (status.reason) {
            "memory_pressure" -> R.string.live_memory
            "thermal_pressure" -> R.string.live_thermal
            "battery_pressure" -> R.string.live_battery
            "storage_pressure" -> R.string.live_storage
            else -> R.string.live_resume
        }
        else -> R.string.live_resume
    }
    message?.let { Text(stringResource(it)) }
    if (status.translationPending) Text(stringResource(R.string.live_translation_pending))
    for (line in status.partials + status.lines.asReversed()) {
        Column {
            Text(
                if (line.language.isEmpty()) stringResource(R.string.live_untagged_partial)
                else stringResource(
                    if (line.partial) R.string.live_partial else R.string.live_line,
                    stringResource(detectedLanguage(line.language)),
                ),
                style = MaterialTheme.typography.labelMedium,
            )
            Text(
                line.text,
                modifier = Modifier.semantics {
                    testTag = if (line.partial) LIVE_PARTIAL_TAG else LIVE_FINAL_TAG
                    testTagsAsResourceId = true
                },
                fontStyle = if (line.partial) FontStyle.Italic else null,
            )
            line.translation?.let {
                Text(stringResource(if (line.stale) R.string.live_stale_zh else R.string.live_zh, it))
            }
        }
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
