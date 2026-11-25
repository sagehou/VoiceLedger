package com.voiceledger.app

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.voiceledger.app.core.CoreException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import org.json.JSONArray

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
    if (trackId == null || track == null) return
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
    } else {
        Text(stringResource(R.string.no_saved_audio))
    }
    status?.let { Text(stringResource(it)) }
}

@StringRes
private fun trackLabel(source: String): Int = if (source == "mic") R.string.model_mic else R.string.model_loopback

@StringRes
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
