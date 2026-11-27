package com.voiceledger.app

import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.NativeSync
import com.voiceledger.app.core.NativeVault
import com.voiceledger.app.core.VaultKeyProtection
import java.io.File
import java.nio.ByteBuffer
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject

class SyncPrivacyActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        setContent {
            MaterialTheme {
                Surface(Modifier.fillMaxSize()) {
                    Column(
                        Modifier.safeDrawingPadding().verticalScroll(rememberScrollState()).padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Text(stringResource(R.string.sync_privacy), style = MaterialTheme.typography.headlineLarge)
                        val settings = remember { ProviderSettings() }
                        ProviderCheck(settings)
                        LocalVault(File(noBackupFilesDir, "sync"), File(filesDir, "recordings"), settings)
                    }
                }
            }
        }
    }
}

@Composable
private fun LocalVault(directory: File, recordings: File, settings: ProviderSettings) {
    val scope = rememberCoroutineScope()
    var settingUp by remember { mutableStateOf(false) }
    var updatingKey by remember { mutableStateOf(false) }
    var pendingTarget by remember { mutableStateOf<String?>(null) }
    var devices by remember { mutableStateOf(emptyList<String>()) }
    var deviceMenu by remember { mutableStateOf(false) }
    var confirmation by remember { mutableStateOf<String?>(null) }
    var localDevice by remember { mutableStateOf<String?>(null) }
    var imported by remember { mutableStateOf<Int?>(null) }
    var removed by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<Int?>(null) }
    var generation by remember { mutableStateOf<Long?>(null) }
    var keyVisible by remember { mutableStateOf<String?>(null) }
    var input by remember { mutableStateOf("") }
    var working by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<Int?>(null) }

    suspend fun refresh() {
        val (keyGeneration, pending) = withContext(Dispatchers.IO) {
            useLocalKey(directory) { recovery ->
                Integer.toUnsignedLong(ByteBuffer.wrap(NativeVault.identity(recovery), 16, 4).int)
            } to VaultKeyProtection.pendingTarget(directory)
        }
        generation = keyGeneration
        pendingTarget = pending
    }
    LaunchedEffect(directory) {
        try {
            refresh()
        } catch (failure: CancellationException) {
            throw failure
        } catch (_: Exception) {
            error = R.string.vault_unavailable
        } catch (_: LinkageError) {
            error = R.string.vault_unavailable
        } finally {
            working = false
        }
    }
    fun install(create: Boolean) {
        val supplied = input
        input = ""
        error = null
        working = true
        scope.launch {
            try {
                withContext(Dispatchers.IO) {
                    val recovery = if (create) NativeVault.createKey() else supplied.trim().encodeToByteArray()
                    try { VaultKeyProtection.installLocal(directory, recovery) } finally { recovery.fill(0) }
                }
                refresh()
            } catch (failure: CancellationException) {
                throw failure
            } catch (_: Exception) {
                error = R.string.vault_install_failed
            } catch (_: LinkageError) {
                error = R.string.vault_unavailable
            } finally {
                working = false
            }
        }
    }

    fun transfer(operation: String, target: String? = null) {
        val provider = settings.encode()
        val submitted = if (operation == "verify_key") input.trim().encodeToByteArray() else byteArrayOf()
        input = ""
        imported = null
        removed = false
        status = R.string.sync_working
        working = true
        scope.launch {
            try {
                val report = withContext(Dispatchers.IO) {
                    val action = JSONObject().put("op", operation)
                    if (target != null) action.put("target", target)
                    val command = JSONObject().put("action", action).put("directory", directory.absolutePath)
                        .put("recordings", recordings.absolutePath).toString().encodeToByteArray()
                    checkNotNull(useLocalKey(directory) { current ->
                        val next = when (operation) {
                            "remove_device" -> VaultKeyProtection.prepareRotation(directory, current, requireNotNull(target))
                            "verify_key" -> submitted
                            else -> byteArrayOf()
                        }
                        try {
                            val bytes = if (operation == "verify_key") NativeSync.transfer(command, provider, next)
                            else NativeSync.transfer(command, provider, current, next)
                            if (next.isNotEmpty()) VaultKeyProtection.commitRotation(directory, current, next)
                            JSONObject(bytes.decodeToString())
                        } finally { next.fill(0) }
                    })
                }
                status = null
                if (operation == "synchronize") {
                    val own = report.getString("device")
                    localDevice = own
                    val known = report.getJSONArray("devices")
                    devices = (0 until known.length()).map(known::getString).filter { it != own }
                    imported = report.getInt("imported")
                } else {
                    devices = emptyList()
                    updatingKey = false
                    keyVisible = null
                    removed = operation == "remove_device"
                    refresh()
                }
            } catch (failure: CancellationException) {
                throw failure
            } catch (failure: CoreException) {
                status = syncError(failure.code)
                pendingTarget = withContext(Dispatchers.IO) { VaultKeyProtection.pendingTarget(directory) }
            } catch (_: Exception) {
                status = R.string.vault_install_failed
            } catch (_: LinkageError) {
                status = R.string.sync_check_unavailable
            } finally {
                provider.fill(0)
                submitted.fill(0)
                working = false
            }
        }
    }

    val current = generation
    when {
        current != null -> {
            Text(stringResource(R.string.vault_generation, current))
            Button(enabled = !working, onClick = {
                if (keyVisible != null) {
                    keyVisible = null
                } else {
                    working = true
                    scope.launch {
                        try {
                            keyVisible = withContext(Dispatchers.IO) { useLocalKey(directory) { it.decodeToString() } }
                        } catch (failure: CancellationException) {
                            throw failure
                        } catch (_: Exception) {
                            error = R.string.vault_unavailable
                        } catch (_: LinkageError) {
                            error = R.string.vault_unavailable
                        } finally {
                            working = false
                        }
                    }
                }
            }) { Text(stringResource(if (keyVisible == null) R.string.vault_show_key else R.string.vault_hide_key)) }
            keyVisible?.let { value -> SelectionContainer { Text(value) } }
            if (updatingKey) {
                SyncField(input, { input = it }, R.string.vault_updated_key, 128, secret = true, enabled = !working)
                Button(enabled = !working && input.isNotBlank() && settings.ready, onClick = { transfer("verify_key") }) {
                    Text(stringResource(R.string.vault_join_updated))
                }
            } else {
                Button(enabled = !working, onClick = { updatingKey = true }) {
                    Text(stringResource(R.string.vault_enter_updated))
                }
            }
            Button(enabled = !working && settings.ready, onClick = { transfer("synchronize") }) {
                Text(stringResource(R.string.sync_text))
            }
            pendingTarget?.let { target ->
                Button(enabled = !working && settings.ready, onClick = { transfer("remove_device", target) }) {
                    Text(stringResource(R.string.sync_resume_removal))
                }
            }
            if (pendingTarget == null && devices.isNotEmpty()) {
                Box {
                    Button(enabled = !working && settings.ready, onClick = { deviceMenu = true }) {
                        Text(stringResource(R.string.sync_remove_device))
                    }
                    DropdownMenu(expanded = deviceMenu, onDismissRequest = { deviceMenu = false }) {
                        for (id in devices) DropdownMenuItem(text = { Text(id) }, onClick = {
                            deviceMenu = false
                            confirmation = id
                        })
                    }
                }
            }
            confirmation?.let { target ->
                AlertDialog(
                    onDismissRequest = { confirmation = null },
                    title = { Text(stringResource(R.string.sync_remove_device)) },
                    text = { Text(stringResource(R.string.sync_remove_prompt)) },
                    confirmButton = {
                        TextButton(onClick = { confirmation = null; transfer("remove_device", target) }) {
                            Text(stringResource(R.string.sync_rotate_remove))
                        }
                    },
                    dismissButton = { TextButton(onClick = { confirmation = null }) { Text(stringResource(R.string.cancel_speech)) } },
                )
            }
            if (removed) Text(stringResource(R.string.sync_removed, current))
            imported?.let { Text(stringResource(R.string.sync_imported, it)) }
            localDevice?.let { Text(stringResource(R.string.sync_this_device, it)) }
            status?.let { Text(stringResource(it)) }

        }
        working -> Text(stringResource(R.string.vault_loading))
        error == R.string.vault_unavailable -> Unit
        else -> {
            Text(stringResource(R.string.vault_empty))
            if (settingUp) {
                Button(onClick = { install(true) }) { Text(stringResource(R.string.vault_create_key)) }
                SyncField(input, { input = it }, R.string.vault_code, 128, secret = true)
                Button(onClick = { install(false) }, enabled = input.isNotBlank()) {
                    Text(stringResource(R.string.vault_import_key))
                }
            } else {
                Button(onClick = { settingUp = true }) { Text(stringResource(R.string.vault_setup)) }
            }
        }
    }
    error?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
}

private fun <T> useLocalKey(directory: File, action: (ByteArray) -> T): T? {
    val key = VaultKeyProtection.readLocal(directory) ?: return null
    try { return action(key) } finally { key.fill(0) }
}

private class ProviderSettings {
    var s3 by mutableStateOf(false)
    var endpoint by mutableStateOf("")
    var login by mutableStateOf("")
    var credential by mutableStateOf("")
    var bucket by mutableStateOf("")
    var region by mutableStateOf("")
    var token by mutableStateOf("")
    val ready: Boolean
        get() = endpoint.isNotBlank() && login.isNotBlank() && (!s3 || bucket.isNotBlank() && region.isNotBlank())

    fun encode(): ByteArray {
        val request = JSONObject().put("endpoint", endpoint.trim())
        if (s3) {
            request.put("provider", "s3").put("bucket", bucket.trim()).put("region", region.trim())
                .put("access_key", login.trim()).put("secret_key", credential)
                .put("session_token", if (token.isEmpty()) JSONObject.NULL else token)
        } else {
            request.put("provider", "webdav").put("name", login.trim()).put("password", credential)
        }
        return request.toString().encodeToByteArray()
    }
}

@Composable
private fun ProviderCheck(settings: ProviderSettings) = with(settings) {
    val scope = rememberCoroutineScope()
    var working by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<Int?>(null) }
    FilterChip(selected = !s3, enabled = !working, onClick = {
        if (s3) { s3 = false; credential = ""; token = ""; status = null }
    }, label = { Text(stringResource(R.string.sync_webdav)) })
    FilterChip(selected = s3, enabled = !working, onClick = {
        if (!s3) { s3 = true; credential = ""; status = null }
    }, label = { Text(stringResource(R.string.sync_s3)) })
    SyncField(endpoint, { endpoint = it }, R.string.sync_endpoint, 2048, enabled = !working)
    SyncField(login, { login = it }, if (s3) R.string.sync_access_key else R.string.sync_account, 128, enabled = !working)
    SyncField(credential, { credential = it }, if (s3) R.string.sync_secret_key else R.string.sync_password,
        4096, secret = true, enabled = !working)
    if (s3) {
        SyncField(bucket, { bucket = it }, R.string.sync_bucket, 63, enabled = !working)
        SyncField(region, { region = it }, R.string.sync_region, 64, enabled = !working)
        SyncField(token, { token = it }, R.string.sync_token, 16384, secret = true, enabled = !working)
    }
    Button(enabled = !working && ready, onClick = {
        val bytes = encode()
        working = true
        status = R.string.sync_checking
        scope.launch {
            try {
                withContext(Dispatchers.IO) { NativeSync.checkProvider(bytes) }
                status = R.string.sync_check_pass
            } catch (failure: CancellationException) {
                throw failure
            } catch (failure: CoreException) {
                status = syncError(failure.code)
            } catch (_: LinkageError) {
                status = R.string.sync_check_unavailable
            } finally {
                bytes.fill(0)
                working = false
            }
        }
    }) { Text(stringResource(R.string.sync_check_provider)) }
    status?.let { Text(stringResource(it)) }
}

private fun syncError(code: Int): Int = when (code) {
    -60 -> R.string.sync_configuration_invalid
    -61 -> R.string.sync_authentication_error
    -62 -> R.string.sync_quota_error
    -63 -> R.string.sync_provider_unsupported
    -65 -> R.string.sync_integrity_error
    -66 -> R.string.sync_generation_retired
    -67 -> R.string.sync_device_removed
    else -> R.string.sync_connection_error
}

@Composable
private fun SyncField(
    value: String,
    set: (String) -> Unit,
    @StringRes label: Int,
    limit: Int,
    secret: Boolean = false,
    enabled: Boolean = true,
) {
    OutlinedTextField(
        value = value,
        onValueChange = { if (it.length <= limit) set(it) },
        label = { Text(stringResource(label)) },
        singleLine = true,
        visualTransformation = if (secret) PasswordVisualTransformation() else VisualTransformation.None,
        enabled = enabled,
    )
}
