package com.voiceledger.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.RecordingPhase
import com.voiceledger.app.recording.RecordingStatusStore
import com.voiceledger.app.speech.SpeechConnection
import java.io.File
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

class ModelsActivity : ComponentActivity() {
    private var connection: SpeechConnection? = null
    private var preparing: Job? = null
    private var working by mutableStateOf(false)
    private var partial by mutableStateOf<String?>(null)
    private var result by mutableStateOf<Int?>(null)
    private var reload by mutableStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            var report by remember { mutableStateOf<JSONObject?>(null) }
            var inspection by remember { mutableStateOf<JSONObject?>(null) }
            var unavailable by remember { mutableStateOf(false) }
            var language by remember { mutableStateOf("auto") }
            var edit by remember { mutableStateOf<JSONObject?>(null) }
            val recording by RecordingStatusStore.state.collectAsState()
            LaunchedEffect(reload) {
                val loaded = withContext(Dispatchers.IO) {
                    try {
                        runCatching { DeviceProfiler.capture(applicationContext) }
                        val catalog = JSONObject(NativeCore.modelPreview(noBackupFilesDir.absolutePath).decodeToString())
                        val details = JSONObject(NativeCore.speechRequest(query(JSONObject().put("op", "inspect"))).decodeToString())
                        catalog to details
                    } catch (_: Exception) {
                        null
                    } catch (_: LinkageError) {
                        null
                    }
                }
                report = loaded?.first
                inspection = loaded?.second
                unavailable = loaded == null
            }
            LaunchedEffect(working) {
                while (working) {
                    delay(700)
                    connection?.poll()
                }
            }
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    Column(
                        Modifier.safeDrawingPadding().verticalScroll(rememberScrollState()).padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Text(stringResource(R.string.models), style = MaterialTheme.typography.headlineLarge)
                        Text(stringResource(R.string.recording_only_during_capture))
                        val status = result?.let(::resultText) ?: inspection?.optJSONObject("job")?.let(::jobStatus)
                        status?.let { Text(stringResource(it)) }
                        if (working) {
                            Text(stringResource(R.string.speech_working))
                            partial?.let { Text(it) }
                            Button(onClick = { finishRequest(-4) }) { Text(stringResource(R.string.cancel_speech)) }
                        }
                        val candidates = report?.getJSONArray("candidates")
                        if (candidates == null) {
                            Text(stringResource(if (unavailable) R.string.models_unavailable else R.string.models_loading))
                        } else {
                            val session = inspection?.optJSONObject("session")
                            val sessionId = session?.getString("id")
                            if (session != null) {
                                val date = Date(session.getLong("created_at_ms"))
                                Text(stringResource(R.string.latest_saved_session, DateFormat.getDateTimeInstance().format(date)))
                                LanguagePicker(language, !working) { language = it }
                            }
                            val choices = inspection?.getJSONArray("models")
                            val idle = recording.phase == RecordingPhase.Idle
                            val hasEnglish = inspection?.optBoolean("has_translation_source") == true
                            for (index in 0 until candidates.length()) {
                                val details = candidates.getJSONObject(index)
                                val model = details.getJSONObject("model")
                                val translation = model.has("translation")
                                val choice = choices?.optJSONObject(index)
                                val installed = choice?.optBoolean("installed") == true
                                val allowed = choice?.isNull("blocked_by") == true && idle && !working
                                val canProcess = sessionId != null && (!translation || hasEnglish)
                                ModelRow(details, choice, allowed && (!installed || canProcess)) {
                                    val action = JSONObject()
                                        .put("model_id", model.getString("id"))
                                        .put("approved", true)
                                    if (installed) {
                                        action.put("session_id", sessionId)
                                        if (translation) action.put("op", "translate")
                                        else action.put("op", "transcribe").put("language", language)
                                    } else {
                                        action.put("op", "install")
                                    }
                                    startRequest(action)
                                }
                            }
                            val lines = inspection?.optJSONArray("lines")
                            for (index in 0 until (lines?.length() ?: 0)) {
                                val line = lines?.getJSONObject(index) ?: continue
                                val source = if (line.getString("source") == "mic") R.string.model_mic else R.string.model_loopback
                                Text(stringResource(R.string.model_line, stringResource(source), line.getLong("start_ns") / 1_000_000_000.0))
                                Text(line.getString("text"))
                                val revision = line.optJSONObject("translation")
                                if (revision != null) {
                                    val author = stringResource(if (revision.getString("authorship") == "manual")
                                        R.string.manual_translation else R.string.machine_translation)
                                    Text(stringResource(if (revision.getBoolean("stale"))
                                        R.string.translation_stale else R.string.translation_label, author))
                                    Text(revision.getString("text"))
                                }
                                if (sessionId != null && line.getString("language") == "en") {
                                    Button(enabled = idle && !working, onClick = { edit = line }) {
                                        Text(stringResource(if (revision == null) R.string.add_translation else R.string.edit_translation))
                                    }
                                }
                            }
                            edit?.let { line ->
                                if (sessionId != null) {
                                    TranslationEditor(line, idle && !working, onDismiss = { edit = null }) { text ->
                                        val action = JSONObject()
                                            .put("op", "save_translation")
                                            .put("session_id", sessionId)
                                            .put("utterance_id", line.getString("utterance_id"))
                                            .put("source_revision_id", line.getString("revision_id"))
                                            .put("expected_translation_id", line.optJSONObject("translation")?.getString("revision_id") ?: JSONObject.NULL)
                                            .put("text", text)
                                        saveTranslation(action).also { code ->
                                            if (code == 0) {
                                                edit = null
                                                result = code
                                                reload++
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    override fun onStop() {
        if (working) finishRequest(-4)
        super.onStop()
    }

    private fun startRequest(action: JSONObject) {
        if (working || RecordingStatusStore.state.value.phase != RecordingPhase.Idle) return
        working = true
        result = null
        partial = null
        preparing = lifecycleScope.launch {
            val refreshed = withContext(Dispatchers.IO) { runCatching { DeviceProfiler.capture(applicationContext) }.isSuccess }
            if (!working) return@launch
            if (!refreshed || RecordingStatusStore.state.value.phase != RecordingPhase.Idle) {
                finishRequest(-34)
                return@launch
            }
            val request = SpeechConnection(this@ModelsActivity, query(action), ::finishRequest) { bytes ->
                partial = bytes?.takeIf { it.isNotEmpty() }?.let {
                    runCatching { JSONObject(it.decodeToString()).getString("text") }.getOrNull()
                }
            }
            connection = request
            try {
                request.bind()
            } catch (_: RuntimeException) {
                finishRequest(-31)
            }
        }
    }

    private fun finishRequest(code: Int) {
        preparing?.cancel()
        preparing = null
        connection?.close()
        connection = null
        working = false
        partial = null
        result = code
        reload++
    }

    private suspend fun saveTranslation(action: JSONObject): Int = withContext(Dispatchers.IO) {
        try {
            NativeCore.speechRequest(query(action))
            0
        } catch (failure: CoreException) {
            failure.code
        } catch (_: LinkageError) {
            -31
        }
    }

    private fun query(action: JSONObject): String = JSONObject()
        .put("recordings", File(filesDir, "recordings").absolutePath)
        .put("profiles", noBackupFilesDir.absolutePath)
        .put("adapters", applicationInfo.nativeLibraryDir)
        .put("action", action)
        .toString()
}

@Composable
private fun TranslationEditor(
    line: JSONObject,
    enabled: Boolean,
    onDismiss: () -> Unit,
    save: suspend (String) -> Int,
) {
    val scope = rememberCoroutineScope()
    var text by remember(line) { mutableStateOf(line.optJSONObject("translation")?.getString("text").orEmpty()) }
    var saving by remember(line) { mutableStateOf(false) }
    var error by remember(line) { mutableStateOf<Int?>(null) }
    AlertDialog(
        onDismissRequest = { if (!saving) onDismiss() },
        title = { Text(stringResource(R.string.edit_translation)) },
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { if (it.encodeToByteArray().size <= 8192) text = it },
                    label = { Text(stringResource(R.string.chinese_translation)) },
                    readOnly = saving,
                )
                error?.let { Text(stringResource(resultText(it))) }
            }
        },
        confirmButton = {
            TextButton(
                enabled = enabled && !saving && text.isNotBlank(),
                onClick = {
                    saving = true
                    scope.launch {
                        try {
                            val code = save(text)
                            error = code.takeUnless { it == 0 }
                        } finally { saving = false }
                    }
                },
            ) {
                Text(stringResource(R.string.save_translation))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !saving) { Text(stringResource(R.string.cancel_speech)) }
        },
    )
}

@Composable
private fun ModelRow(candidate: JSONObject, choice: JSONObject?, enabled: Boolean, action: () -> Unit) {
    val model = candidate.getJSONObject("model")
    val runtime = model.getJSONObject("runtime")
    val languageCodes = model.getJSONArray("languages")
    val languages = (0 until languageCodes.length()).joinToString(", ") { languageCodes.getString(it) }
    val estimate = model.optJSONObject("peak_ram_estimate")
    Column(Modifier.padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Text(
            stringResource(R.string.model_title, model.getString("name"), model.getString("quantization")),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(stringResource(R.string.model_runtime, runtime.getString("id"), runtime.getString("version"),
            model.getJSONObject("license").getString("id")))
        Text(stringResource(R.string.model_download, languages, candidate.getLong("download_bytes") / 1048576.0))
        Text(
            if (estimate == null) stringResource(R.string.model_estimate_unknown)
            else stringResource(R.string.model_estimate, estimate.getLong("bytes") / 1048576.0),
        )
        Text(stringResource(R.string.model_status, stringResource(reason(choice?.optString("blocked_by") ?: "unknown"))))
        val actionLabel = when {
            choice?.optBoolean("installed") != true -> R.string.download_model
            model.has("translation") -> R.string.translate_saved
            else -> R.string.transcribe_saved
        }
        Button(onClick = action, enabled = enabled) { Text(stringResource(actionLabel)) }
    }
}

@Composable
private fun LanguagePicker(policy: String, enabled: Boolean, selected: (String) -> Unit) {
    val labels = mapOf(
        "auto" to R.string.model_auto,
        "zh" to R.string.model_chinese,
        "en" to R.string.model_english,
        "ja" to R.string.model_japanese,
    )
    var expanded by remember { mutableStateOf(false) }
    Box {
        Button(onClick = { expanded = true }, enabled = enabled) {
            Text(stringResource(R.string.transcription_language, stringResource(labels.getValue(policy))))
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            labels.forEach { (code, label) ->
                DropdownMenuItem(text = { Text(stringResource(label)) }, onClick = {
                    selected(code)
                    expanded = false
                })
            }
        }
    }
}

private fun resultText(code: Int): Int = when (code) {
    0 -> R.string.speech_complete
    -4 -> R.string.speech_cancelled
    -32 -> R.string.speech_no_audio
    -33 -> R.string.speech_not_installed
    -34 -> R.string.speech_blocked
    -35 -> R.string.speech_transfer
    -36 -> R.string.speech_integrity
    -37 -> R.string.speech_busy
    -38 -> R.string.speech_deferred
    -39 -> R.string.translation_conflict
    -40 -> R.string.translation_limit
    else -> R.string.speech_failed
}

private fun jobStatus(job: JSONObject): Int? = when (job.optString("state")) {
    "deferred" -> if (job.optString("reason") == "worker_interrupted") R.string.speech_interrupted else R.string.speech_deferred
    "failed" -> R.string.speech_retry
    else -> null
}

private fun reason(key: String): Int = when (key) {
    "", "null" -> R.string.model_compatible
    "device_profile" -> R.string.model_device_unknown
    "abi", "os_version", "cpu_features" -> R.string.model_incompatible
    "storage_space" -> R.string.model_storage_low
    "storage_unknown" -> R.string.model_storage_unknown
    "memory_budget" -> R.string.model_memory_low
    "memory_unknown" -> R.string.model_memory_unknown
    "runtime_unavailable" -> R.string.model_runtime_unknown
    "memory_pressure" -> R.string.memory_pressure
    "thermal_pressure" -> R.string.thermal_pressure
    "battery_pressure" -> R.string.speech_battery_pressure
    else -> R.string.model_compatibility_unknown
}
