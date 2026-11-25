package com.voiceledger.app

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.voiceledger.app.core.CoreException
import java.io.File
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.UUID
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

private const val RATE = 48_000
private const val SECOND_NS = 1_000_000_000.0

@Composable
internal fun SessionAudio(
    tracks: JSONArray?,
    selected: String?,
    onTrack: (String?) -> Unit,
    onStop: () -> Unit,
    onPlay: (String, Long, (Int?) -> Unit) -> Unit,
    timeline: suspend (String) -> JSONArray,
    archive: (String, String?) -> ByteArray,
) {
    if (tracks == null || tracks.length() == 0) return
    val choices = (0 until tracks.length()).map(tracks::getJSONObject)
    val track = choices.find { it.getString("source") == selected }
    val trackId = track?.getString("id")
    var menu by remember { mutableStateOf(false) }
    var at by remember(trackId) { mutableStateOf((track?.getLong("first_ns")?.div(SECOND_NS))?.toString().orEmpty()) }
    var rows by remember(trackId) { mutableStateOf<JSONArray?>(null) }
    var status by remember(trackId) { mutableStateOf<Int?>(null) }
    val stop by rememberUpdatedState(onStop)
    DisposableEffect(trackId) {
        onDispose { stop() }
    }
    LaunchedEffect(trackId) {
        if (trackId != null) {
            try {
                rows = timeline(trackId)
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                status = R.string.session_timeline_error
            } catch (_: LinkageError) {
                status = R.string.session_timeline_error
            }
        }
    }
    Box {
        Button(onClick = { menu = true }) {
            Text(stringResource(track?.let { trackLabel(it.getString("source")) } ?: R.string.session_all_tracks))
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            DropdownMenuItem(text = { Text(stringResource(R.string.session_all_tracks)) }, onClick = {
                onTrack(null)
                menu = false
            })
            for (choice in choices) {
                val source = choice.getString("source")
                DropdownMenuItem(text = { Text(stringResource(trackLabel(source))) }, onClick = {
                    onTrack(source)
                    menu = false
                })
            }
        }
    }
    if (trackId == null) return
    rows?.let { timelineRows ->
        LazyColumn(Modifier.heightIn(max = 180.dp)) {
            items(timelineRows.length()) { index ->
                val row = timelineRows.getJSONObject(index)
                val kind = when (row.getString("kind")) {
                    "silence" -> R.string.session_silence
                    "gap" -> R.string.session_gap
                    "pending" -> R.string.session_pending
                    else -> R.string.session_audio
                }
                Text(stringResource(R.string.session_timeline_row, stringResource(kind),
                    row.getLong("start_ns") / SECOND_NS, row.getLong("end_ns") / SECOND_NS))
            }
        }
    }
    if (track.getLong("committed_chunks") > 0) {
        val position = at.trim().replace(',', '.').toDoubleOrNull()?.times(SECOND_NS)
            ?.takeIf { it >= 0 && it < Long.MAX_VALUE.toDouble() }?.toLong()
        OutlinedTextField(
            value = at, onValueChange = { if (it.length <= 24) at = it },
            label = { Text(stringResource(R.string.session_start_time)) },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal), singleLine = true,
        )
        Button(enabled = position != null && status != R.string.session_playing, onClick = {
            val start = position ?: return@Button
            status = R.string.session_playing
            onPlay(trackId, start) { status = it }
        }) { Text(stringResource(R.string.session_play)) }
        Button(enabled = status == R.string.session_playing, onClick = onStop) { Text(stringResource(R.string.session_stop_playback)) }
        ExportTrack(trackId, track.getString("source"), archive)
    } else {
        Text(stringResource(R.string.no_saved_audio))
    }
    status?.let { Text(stringResource(it)) }
}

@Composable
private fun ExportTrack(trackId: String, source: String, read: (String, String?) -> ByteArray) {
    val app = LocalContext.current
    val scope = rememberCoroutineScope()
    val request by rememberUpdatedState(read)
    var preparing by remember { mutableStateOf(false) }
    var confirmation by remember { mutableStateOf(false) }
    var pendingTrack by remember { mutableStateOf<String?>(null) }
    var status by remember { mutableStateOf<Int?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/zip")) { uri ->
        val track = pendingTrack
        pendingTrack = null
        if (uri != null && track != null) scope.launch {
            preparing = true
            status = try {
                withContext(Dispatchers.IO) {
                    val temporary = File.createTempFile("voiceledger-track-", ".partial", app.cacheDir)
                    try {
                        temporary.outputStream().use { writeTrackZip(it) { chunk -> request(track, chunk) } }
                        currentCoroutineContext().ensureActive()
                        checkNotNull(app.contentResolver.openOutputStream(uri, "wt")).use { output ->
                            temporary.inputStream().use { it.copyTo(output) }
                        }
                    } finally {
                        temporary.delete()
                    }
                }
                R.string.export_complete
            } catch (error: CancellationException) {
                throw error
            } catch (_: Exception) {
                R.string.export_failed
            } catch (_: LinkageError) {
                R.string.export_failed
            } finally {
                preparing = false
            }
        }
    }
    Button(enabled = !preparing && pendingTrack == null, onClick = { confirmation = true }) {
        Text(stringResource(R.string.session_export_audio))
    }
    if (confirmation) AlertDialog(
        onDismissRequest = { confirmation = false },
        text = { Text(stringResource(R.string.export_plaintext)) },
        confirmButton = {
            TextButton(onClick = {
                confirmation = false
                pendingTrack = trackId
                status = null
                val name = if (source == "mic") "microphone" else "system"
                picker.launch("VoiceLedger-$name.zip")
            }) { Text(stringResource(R.string.session_export_audio)) }
        },
        dismissButton = { TextButton(onClick = { confirmation = false }) { Text(stringResource(R.string.cancel_speech)) } },
    )
    status?.let { Text(stringResource(it)) }
}

private suspend fun writeTrackZip(output: OutputStream, read: (String?) -> ByteArray) {
    val manifest = read(null)
    val chunks = JSONObject(manifest.decodeToString()).getJSONArray("chunks")
    ZipOutputStream(output).use { zip ->
        fun entry(name: String, bytes: ByteArray) {
            zip.putNextEntry(ZipEntry(name))
            zip.write(bytes)
            zip.closeEntry()
        }
        entry("manifest.json", manifest)
        for (index in 0 until chunks.length()) {
            currentCoroutineContext().ensureActive()
            val id = chunks.getJSONObject(index).getString("id")
            check(UUID.fromString(id).toString() == id)
            val audio = read(id)
            try { entry("chunks/$id.opus", audio) } finally { audio.fill(0) }
        }
    }
}

@StringRes
private fun trackLabel(source: String): Int = if (source == "mic") R.string.model_mic else R.string.model_loopback

internal suspend fun playSavedTrack(start: Long, read: (Long) -> ByteArray): Int = withContext(Dispatchers.IO) {
    var player: AudioTrack? = null
    try {
        var position = start
        var channels = 0
        var writtenFrames = 0L
        while (true) {
            currentCoroutineContext().ensureActive()
            val bytes = try {
                read(position)
            } catch (error: CoreException) {
                if (error.code != -32) throw error
                val output = player ?: return@withContext R.string.session_audio_unavailable
                repeat(100) {
                    if (Integer.toUnsignedLong(output.playbackHeadPosition) >= writtenFrames) {
                        return@withContext R.string.session_playback_ended
                    }
                    delay(20)
                }
                return@withContext R.string.session_playback_ended
            }
            val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
            check(buffer.remaining() >= 12)
            val count = buffer.int
            val next = buffer.long
            check(count in 1..2 && next > position && buffer.remaining() % (count * 4) == 0)
            if (player == null) {
                channels = count
                val channelMask = if (channels == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO
                val format = AudioFormat.Builder().setSampleRate(RATE).setChannelMask(channelMask)
                    .setEncoding(AudioFormat.ENCODING_PCM_FLOAT).build()
                val minimum = AudioTrack.getMinBufferSize(RATE, channelMask, AudioFormat.ENCODING_PCM_FLOAT)
                check(minimum > 0)
                val output = AudioTrack.Builder()
                    .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH).build())
                    .setAudioFormat(format).setTransferMode(AudioTrack.MODE_STREAM).setBufferSizeInBytes(minimum).build()
                player = output
                check(output.state == AudioTrack.STATE_INITIALIZED)
                output.play()
            }
            if (count != channels) return@withContext R.string.session_playback_ended
            val output = requireNotNull(player)
            val floats = buffer.asFloatBuffer()
            val samples = FloatArray(minOf(floats.remaining(), 4_800 * channels))
            while (floats.hasRemaining()) {
                val length = minOf(samples.size, floats.remaining())
                floats.get(samples, 0, length)
                var written = 0
                while (written < length) {
                    currentCoroutineContext().ensureActive()
                    val size = output.write(samples, written, length - written, AudioTrack.WRITE_BLOCKING)
                    check(size > 0)
                    written += size
                }
                writtenFrames += length / channels
            }
            position = next
        }
        @Suppress("UNREACHABLE_CODE")
        R.string.session_playback_ended
    } catch (error: CancellationException) {
        throw error
    } catch (_: Exception) {
        R.string.session_playback_error
    } catch (_: LinkageError) {
        R.string.session_playback_error
    } finally {
        player?.release()
    }
}
