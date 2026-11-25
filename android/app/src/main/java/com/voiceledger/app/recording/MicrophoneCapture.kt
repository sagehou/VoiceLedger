package com.voiceledger.app.recording

import android.Manifest
import android.annotation.SuppressLint
import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioRecordingConfiguration
import android.media.AudioRouting
import android.media.AudioTimestamp
import android.media.MediaRecorder
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread

internal class MicrophoneCapture(
    private val context: Context,
    private val control: RecordingControl,
) {
    private val audioManager = requireNotNull(context.getSystemService(AudioManager::class.java))

    fun run(manifest: RecordingManifest, onStarted: () -> Unit) {
        checkEnvironment()
        if (control.stop != null) return
        val recorder = try {
            createRecorder()
        } catch (_: SecurityException) {
            control.request(CaptureEnd.Permission)
            return
        } catch (_: RuntimeException) {
            control.request(CaptureEnd.Input)
            return
        }
        try {
            if (control.stop != null) return
            val startNs = control.elapsedNs()
            try {
                recorder.startRecording()
            } catch (_: SecurityException) {
                control.request(CaptureEnd.Permission)
                return
            } catch (_: IllegalStateException) {
                control.request(CaptureEnd.Input)
                return
            }
            if (recorder.recordingState != AudioRecord.RECORDSTATE_RECORDING) {
                control.request(CaptureEnd.Input)
                return
            }
            val route = recorder.routedDevice
            manifest.beginStream(startNs, route?.type)
            RecordingStatusStore.update {
                if (control.stop == null) it.copy(phase = RecordingPhase.Recording, saved = null) else it
            }
            onStarted()
            watchInput(recorder, route?.id) { captureAndDrain(recorder, startNs, manifest) }
        } finally {
            try {
                stopRecorder(recorder)
            } finally {
                recorder.release()
            }
        }
    }

    @SuppressLint("MissingPermission") // Checked before creating the native input and again during capture.
    private fun createRecorder(): AudioRecord {
        val channels = AudioFormat.CHANNEL_IN_MONO
        val encoding = AudioFormat.ENCODING_PCM_16BIT
        val minimum = AudioRecord.getMinBufferSize(SAMPLE_RATE, channels, encoding)
        check(minimum > 0)
        val recorder = AudioRecord.Builder()
            .setAudioSource(MediaRecorder.AudioSource.MIC)
            .setAudioFormat(
                AudioFormat.Builder().setSampleRate(SAMPLE_RATE).setChannelMask(channels)
                    .setEncoding(encoding).build(),
            )
            .setBufferSizeInBytes(maxOf(minimum, 10 * PACKET_FRAMES * Short.SIZE_BYTES))
            .build()
        if (recorder.state != AudioRecord.STATE_INITIALIZED || recorder.sampleRate != SAMPLE_RATE ||
            recorder.channelCount != 1 || recorder.audioFormat != encoding
        ) {
            recorder.release()
            error("Unsupported microphone format.")
        }
        return recorder
    }

    private fun watchInput(recorder: AudioRecord, initialRoute: Int?, capture: () -> Unit) {
        val routeId = AtomicReference(initialRoute)
        fun checkRoute(routing: AudioRouting) {
            if (control.stop == null) {
                val current = routing.routedDevice?.id
                val previous = routeId.get()
                if (previous != null && current != previous) {
                    control.request(CaptureEnd.Route)
                } else if (current != null) {
                    routeId.compareAndSet(null, current)
                }
            }
        }
        val routeListener = AudioRouting.OnRoutingChangedListener(::checkRoute)
        val recordingCallback = object : AudioManager.AudioRecordingCallback() {
            override fun onRecordingConfigChanged(configs: List<AudioRecordingConfiguration>) {
                if (Build.VERSION.SDK_INT >= 29 && control.stop == null &&
                    configs.any { it.clientAudioSessionId == recorder.audioSessionId && it.isClientSilenced }
                ) {
                    control.request(CaptureEnd.Contention)
                }
            }
        }
        recorder.addOnRoutingChangedListener(routeListener, Handler(Looper.getMainLooper()))
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                recorder.registerAudioRecordingCallback(context.mainExecutor, recordingCallback)
            }
            try {
                checkRoute(recorder)
                checkEnvironment(recorder)
                if (control.stop == null) capture()
            } finally {
                if (Build.VERSION.SDK_INT >= 29) recorder.unregisterAudioRecordingCallback(recordingCallback)
            }
        } finally {
            recorder.removeOnRoutingChangedListener(routeListener)
        }
    }

    private data class Packet(val samples: ShortArray, val time: PacketTime)

    private fun captureAndDrain(recorder: AudioRecord, startNs: Long, manifest: RecordingManifest) {
        val pool = ArrayBlockingQueue<ShortArray>(24)
        repeat(24) { pool.add(ShortArray(PACKET_FRAMES)) }
        val ready = ArrayBlockingQueue<Packet>(24)
        val lastReadNs = AtomicLong(control.elapsedNs())
        val stallNs = maxOf(2 * SECOND_NS, recorder.bufferSizeInFrames.toLong() * SECOND_NS / SAMPLE_RATE)
        val reader = thread(name = "VoiceLedger microphone") {
            val timeline = CaptureTimeline(startNs)
            val timestamp = AudioTimestamp()
            var first = true
            try {
                while (control.stop == null) {
                    val samples = pool.poll()
                    if (samples == null) {
                        control.request(CaptureEnd.Backpressure)
                        break
                    }
                    val frames = recorder.read(samples, 0, samples.size, AudioRecord.READ_BLOCKING)
                    val stop = control.stop
                    if (stop != null && stop.reason != CaptureEnd.User) break
                    val observed = control.elapsedNs()
                    if (frames <= 0) {
                        control.request(CaptureEnd.Input)
                        break
                    }
                    if (observed - lastReadNs.getAndSet(observed) > stallNs) {
                        control.request(CaptureEnd.Input)
                        break
                    }
                    val hardware = if (first &&
                        recorder.getTimestamp(timestamp, AudioTimestamp.TIMEBASE_BOOTTIME) == AudioRecord.SUCCESS
                    ) {
                        CaptureTimestamp(timestamp.framePosition, timestamp.nanoTime - control.origin)
                    } else {
                        null
                    }
                    first = false
                    val time = timeline.next(frames, observed, hardware)
                    if (stop != null && time.endNs > stop.atNs) break
                    control.captured(time)
                    ready.add(Packet(samples, time))
                }
            } catch (_: SecurityException) {
                control.request(CaptureEnd.Permission)
            } catch (_: RuntimeException) {
                control.request(CaptureEnd.Input)
            }
        }
        try {
            var nextCheckNs = control.elapsedNs()
            while (reader.isAlive || ready.isNotEmpty()) {
                if (control.stop != null) stopRecorder(recorder)
                ready.poll(50, TimeUnit.MILLISECONDS)?.let {
                    manifest.append(it.samples, it.time)
                    pool.add(it.samples)
                }
                val now = control.elapsedNs()
                if (control.stop == null && now >= nextCheckNs) {
                    checkEnvironment(recorder)
                    if (now - lastReadNs.get() > stallNs) control.request(CaptureEnd.Input)
                    nextCheckNs = now + SECOND_NS / 2
                }
            }
        } finally {
            if (reader.isAlive) {
                control.request(CaptureEnd.Storage)
                try {
                    stopRecorder(recorder)
                } finally {
                    // AudioRecord.stop interrupts the blocking native read.
                    reader.join()
                }
            }
        }
    }

    private fun stopRecorder(recorder: AudioRecord) {
        if (recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING) recorder.stop()
    }

    private fun checkEnvironment(recorder: AudioRecord? = null) {
        val mode = audioManager.mode
        when {
            context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED ->
                control.request(CaptureEnd.Permission)
            !RecordingService.notificationsEnabled(context) -> control.request(CaptureEnd.Notification)
            mode == AudioManager.MODE_IN_CALL || mode == AudioManager.MODE_IN_COMMUNICATION ||
                (Build.VERSION.SDK_INT >= 29 && recorder?.activeRecordingConfiguration?.isClientSilenced == true) ->
                control.request(CaptureEnd.Contention)
            context.filesDir.usableSpace < STORAGE_RESERVE_BYTES -> control.request(CaptureEnd.LowStorage)
        }
        val power = context.getSystemService(PowerManager::class.java)
        val thermal = Build.VERSION.SDK_INT >= 29 &&
            (power?.currentThermalStatus ?: PowerManager.THERMAL_STATUS_NONE) >= PowerManager.THERMAL_STATUS_SEVERE
        val memory = ActivityManager.MemoryInfo()
        context.getSystemService(ActivityManager::class.java)?.getMemoryInfo(memory)
        RecordingStatusStore.update { it.copy(thermal = thermal, lowMemory = memory.lowMemory) }
    }
}
