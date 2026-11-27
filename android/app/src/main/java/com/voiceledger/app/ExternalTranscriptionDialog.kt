package com.voiceledger.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import org.json.JSONArray
import org.json.JSONObject

@Composable
internal fun ExternalTranscriptionDialog(
    sessionId: String,
    tracks: List<JSONObject>,
    presets: JSONArray,
    sourceLanguage: String,
    enabled: Boolean,
    onDismiss: () -> Unit,
    onSend: (JSONObject) -> Unit,
) {
    val providers = remember(presets) {
        List(presets.length()) { presets.getJSONObject(it) }.associateBy { it.getString("id") }
    }
    var providerId by remember(presets) { mutableStateOf(providers.keys.first()) }
    val provider = providers.getValue(providerId)
    val custom = providerId == "custom"
    var baseUrl by remember(providerId) { mutableStateOf(provider.getString("base_url")) }
    var model by remember(providerId) { mutableStateOf(provider.getString("model_id")) }
    var key by remember(providerId) { mutableStateOf("") }
    var language by remember { mutableStateOf(sourceLanguage.takeUnless { it == "auto" }) }
    var trackId by remember { mutableStateOf(tracks.first().getString("id")) }
    val trackLabels = tracks.associate { track ->
        track.getString("id") to stringResource(
            if (track.getString("source") == "mic") R.string.model_mic else R.string.model_loopback,
        )
    }
    val providerLabels = providers.mapValues { (id, preset) ->
        if (id == "custom") stringResource(R.string.external_custom) else preset.getString("name")
    }
    val languages = mapOf(
        "zh" to stringResource(R.string.model_chinese),
        "en" to stringResource(R.string.model_english),
        "ja" to stringResource(R.string.model_japanese),
    )
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.external_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Column(
                    Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    ExternalChoice(stringResource(R.string.external_track), trackId, trackLabels) { trackId = it }
                    ExternalChoice(stringResource(R.string.external_provider), providerId, providerLabels) { providerId = it }
                    OutlinedTextField(
                        value = baseUrl,
                        onValueChange = {
                            if (custom && it.length <= 2048) {
                                key = ""
                                baseUrl = it
                            }
                        },
                        modifier = Modifier.fillMaxWidth(),
                        label = { Text(stringResource(R.string.external_base_url)) },
                        singleLine = true,
                        readOnly = !custom,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    )
                    OutlinedTextField(
                        value = model,
                        onValueChange = { if (it.length <= 192) model = it },
                        modifier = Modifier.fillMaxWidth(),
                        label = { Text(stringResource(R.string.external_model)) },
                        singleLine = true,
                    )
                    ExternalChoice(stringResource(R.string.external_language), language, languages) { language = it }
                    OutlinedTextField(
                        value = key,
                        onValueChange = { if (it.length <= 4096) key = it },
                        modifier = Modifier.fillMaxWidth(),
                        label = { Text(stringResource(if (custom) R.string.external_key_optional else R.string.external_key)) },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                    )
                }
                Text(stringResource(R.string.external_disclosure), style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = {
            TextButton(
                enabled = enabled && language != null && baseUrl.isNotBlank() && model.isNotBlank()
                    && (custom || key.isNotBlank()),
                onClick = {
                    val settings = JSONObject()
                        .put("provider", providerId)
                        .put("base_url", if (custom) baseUrl.trim() else JSONObject.NULL)
                        .put("model_id", model.trim())
                        .put("api_key", key)
                    key = ""
                    onSend(JSONObject().put("op", "transcribe_external").put("session_id", sessionId)
                        .put("track_id", trackId).put("language", language).put("approved", true).put("settings", settings))
                },
            ) { Text(stringResource(R.string.external_send)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel_speech)) } },
    )
}

@Composable
private fun ExternalChoice(label: String, value: String?, choices: Map<String, String>, select: (String) -> Unit) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        TextButton(onClick = { expanded = true }) {
            Text(value?.let(choices::get)?.let { stringResource(R.string.external_choice, label, it) } ?: label)
        }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            choices.forEach { (id, name) ->
                DropdownMenuItem(text = { Text(name) }, onClick = {
                    expanded = false
                    select(id)
                })
            }
        }
    }
}
