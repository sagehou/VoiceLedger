package com.voiceledger.app

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

class ModelsActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            var report by remember { mutableStateOf<JSONObject?>(null) }
            var unavailable by remember { mutableStateOf(false) }
            LaunchedEffect(Unit) {
                val result = withContext(Dispatchers.IO) {
                    try {
                        runCatching { DeviceProfiler.capture(applicationContext) }
                        JSONObject(NativeCore.modelPreview(noBackupFilesDir.absolutePath).decodeToString())
                    } catch (_: Exception) {
                        null
                    } catch (_: LinkageError) {
                        null
                    }
                }
                report = result
                unavailable = result == null
            }
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    Column(
                        Modifier.safeDrawingPadding().verticalScroll(rememberScrollState()).padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        Text(stringResource(R.string.models), style = MaterialTheme.typography.headlineLarge)
                        Text(stringResource(R.string.recording_only))
                        val candidates = report?.getJSONArray("candidates")
                        if (candidates == null) {
                            Text(stringResource(if (unavailable) R.string.models_unavailable else R.string.models_loading))
                        } else {
                            for (index in 0 until candidates.length()) ModelRow(candidates.getJSONObject(index))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ModelRow(candidate: JSONObject) {
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
        Text(stringResource(R.string.model_status, stringResource(reason(candidate.optString("blocked_by")))))
    }
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
    else -> R.string.model_compatibility_unknown
}
