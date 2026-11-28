package com.voiceledger.app

import android.os.Bundle
import android.text.format.DateUtils
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.lifecycleScope
import com.voiceledger.app.core.CoreException
import com.voiceledger.app.core.DeviceProfiler
import com.voiceledger.app.core.NativeCore
import com.voiceledger.app.recording.RecordingPhase
import com.voiceledger.app.recording.RecordingStatusStore
import com.voiceledger.app.speech.SpeechConnection
import com.voiceledger.app.speech.speechRequest
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

class ModelsActivity : ComponentActivity() {
    private var connection: SpeechConnection? = null
    private var preparing: Job? = null
    private var playback: Job? = null
    private var working by mutableStateOf(false)
    private var requestOperation by mutableStateOf<String?>(null)
    private var downloadModelId by mutableStateOf<String?>(null)
    private var downloadedBytes by mutableStateOf<Long?>(null)
    private var partial by mutableStateOf<String?>(null)
    private var result by mutableStateOf<Int?>(null)
    private var reload by mutableStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val sessions = intent.getBooleanExtra("sessions", false)
        setContent {
            var selectedSession by rememberSaveable { mutableStateOf(intent.getStringExtra("session_id")) }
            var report by remember(selectedSession) { mutableStateOf<JSONObject?>(null) }
            var inspection by remember(selectedSession) { mutableStateOf<JSONObject?>(null) }
            var unavailable by remember(selectedSession) { mutableStateOf(false) }
            var loadedRevision by remember(selectedSession) { mutableStateOf(-1) }
            var language by remember { mutableStateOf("auto") }
            var edit by remember { mutableStateOf<JSONObject?>(null) }
            var editKind by remember { mutableStateOf(RevisionEditor.Transcript) }
            var rename by remember(selectedSession) { mutableStateOf(false) }
            var externalDialog by remember(selectedSession) { mutableStateOf(false) }
            var trackFilter by remember(selectedSession) { mutableStateOf<String?>(null) }
            var after by remember(selectedSession) { mutableStateOf<String?>(null) }
            val priorPages = remember(selectedSession) { mutableStateListOf<String?>() }
            var notePage by remember(selectedSession) { mutableStateOf<JSONObject?>(null) }
            var pageLoading by remember(selectedSession) { mutableStateOf(true) }
            var showModels by remember(selectedSession) { mutableStateOf(false) }
            var showAudio by remember(selectedSession) { mutableStateOf(false) }
            val scroll = rememberScrollState()
            val scope = rememberCoroutineScope()
            var search by remember { mutableStateOf("") }
            var requestedSearch by remember { mutableStateOf("") }
            var history by remember { mutableStateOf<JSONArray?>(null) }
            var historyError by remember { mutableStateOf(false) }
            val recording by RecordingStatusStore.state.collectAsState()
            LaunchedEffect(selectedSession, requestedSearch, reload) {
                if (sessions && selectedSession == null) {
                    history = null
                    historyError = false
                    history = withContext(Dispatchers.IO) {
                        val request = JSONObject().put("op", "history").put("query", requestedSearch)
                        runCatching { JSONArray(NativeCore.speechRequest(query(request)).decodeToString()) }.getOrNull()
                    }
                    historyError = history == null
                    return@LaunchedEffect
                }
                val selected = selectedSession
                val loaded = withContext(Dispatchers.IO) {
                    try {
                        runCatching { DeviceProfiler.capture(applicationContext) }
                        val catalog = JSONObject(NativeCore.modelPreview(noBackupFilesDir.absolutePath).decodeToString())
                        val action = JSONObject().put("op", "inspect")
                        if (selected != null) action.put("session_id", selected)
                        val details = JSONObject(NativeCore.speechRequest(query(action)).decodeToString())
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
                loadedRevision = reload
            }
            LaunchedEffect(selectedSession, trackFilter, after, reload) {
                val sessionId = selectedSession
                if (!sessions || sessionId == null) return@LaunchedEffect
                pageLoading = true
                notePage = withContext(Dispatchers.IO) {
                    val action = JSONObject().put("op", "transcript_page").put("session_id", sessionId)
                        .put("after", after?.let(::JSONObject) ?: JSONObject.NULL)
                        .put("track_id", trackFilter ?: JSONObject.NULL)
                    runCatching { JSONObject(NativeCore.speechRequest(query(action)).decodeToString()) }.getOrNull()
                }
                pageLoading = false
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
                        Modifier.safeDrawingPadding().verticalScroll(scroll).padding(24.dp),
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        if (!sessions || selectedSession == null) {
                            Text(stringResource(if (sessions) R.string.sessions else R.string.models),
                                style = MaterialTheme.typography.headlineLarge)
                        }
                        if (sessions && selectedSession == null) {
                            OutlinedTextField(
                                value = search, onValueChange = { if (it.length <= 64) search = it },
                                label = { Text(stringResource(R.string.search_transcripts)) }, singleLine = true,
                            )
                            Button(onClick = { requestedSearch = search; reload++ }) {
                                Text(stringResource(R.string.search_transcripts))
                            }
                            val results = history
                            when {
                                historyError -> Text(stringResource(R.string.sessions_unavailable))
                                results == null -> Text(stringResource(R.string.sessions_loading))
                                results.length() == 0 -> {
                                    Text(stringResource(if (requestedSearch.isBlank()) R.string.sessions_empty else R.string.sessions_no_match))
                                    Button(onClick = ::finish) { Text(stringResource(R.string.return_to_recording)) }
                                }
                                else -> for (index in 0 until results.length()) {
                                    val item = results.getJSONObject(index)
                                    val time = DateFormat.getDateTimeInstance().format(Date(item.getLong("created_at_ms")))
                                    val duration = DateUtils.formatElapsedTime(item.getLong("elapsed_ns") / 1_000_000_000)
                                    val state = stringResource(if (item.getBoolean("interrupted")) R.string.session_interrupted else R.string.session_completed)
                                    Button(onClick = {
                                        selectedSession = item.getString("id")
                                        scope.launch { scroll.scrollTo(0) }
                                    }) {
                                        val label = if (item.optBoolean("text_only"))
                                            stringResource(R.string.session_synced_row, time, duration)
                                        else stringResource(R.string.session_row, time, duration, state,
                                            item.getLong("committed"), item.getLong("missing"))
                                        val title = if (item.isNull("title")) "" else item.getString("title")
                                        Text(if (title.isEmpty()) label else "$title\n$label")
                                    }
                                }
                            }
                            return@Column
                        }
                        if (sessions) {
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                TextButton(enabled = !working, onClick = {
                                    selectedSession = null
                                    edit = null
                                    downloadModelId = null
                                    result = null
                                    scope.launch { scroll.scrollTo(0) }
                                }) { Text(stringResource(R.string.back_to_sessions)) }
                                TextButton(enabled = !working, onClick = { reload++ }) {
                                    Text(stringResource(R.string.note_refresh))
                                }
                            }
                        }
                        val status = if (downloadModelId == null) {
                            result?.let { resultText(it, requestOperation) } ?: inspection?.optJSONObject("job")?.let(::jobStatus)
                        } else null
                        status?.let { Text(stringResource(it)) }
                        if (working && downloadModelId == null) {
                            Text(stringResource(when (requestOperation) {
                                "summarize" -> R.string.summary_working
                                "transcribe_external" -> R.string.external_working
                                else -> R.string.speech_working
                            }))
                            partial?.let { Text(it) }
                            Button(onClick = { finishRequest(-4) }) { Text(stringResource(R.string.cancel_speech)) }
                        }
                        val candidates = report?.getJSONArray("candidates")
                        if (candidates == null) {
                            Text(stringResource(when {
                                sessions && unavailable -> R.string.note_unavailable
                                sessions -> R.string.note_loading
                                unavailable -> R.string.models_unavailable
                                else -> R.string.models_loading
                            }))
                        } else {
                            val session = inspection?.optJSONObject("session")
                            val sessionId = session?.getString("id")
                            val textOnly = inspection?.optBoolean("text_only") == true
                            val idle = recording.phase == RecordingPhase.Idle
                            if (sessions && sessionId == null) {
                                Text(stringResource(R.string.note_unavailable))
                                return@Column
                            }
                            if (session != null) {
                                val title = inspection?.takeUnless { it.isNull("title") }?.getString("title").orEmpty()
                                if (sessions) Text(title.ifEmpty { stringResource(R.string.session_note) },
                                    style = MaterialTheme.typography.headlineLarge)
                                val date = Date(session.getLong("created_at_ms"))
                                Text(DateFormat.getDateTimeInstance().format(date))
                                if (textOnly) Text(stringResource(R.string.session_text_only))
                                if (sessions && sessionId != null) {
                                    Button(enabled = !working, onClick = { rename = true }) {
                                        Text(stringResource(R.string.session_rename))
                                    }
                                    if (rename) {
                                        TextRevisionEditor(title, R.string.session_rename, !working,
                                            field = R.string.session_title, maxLength = 64, allowBlank = true,
                                            onDismiss = { rename = false }) { value ->
                                            saveRevision(JSONObject().put("op", "rename_session")
                                                .put("session_id", sessionId).put("text", value)).also { code ->
                                                if (code == 0) { rename = false; reload++ }
                                            }
                                        }
                                    }
                                    val summary = inspection?.optJSONObject("summary")
                                    val hasText = (inspection?.optJSONArray("lines")?.length() ?: 0) > 0
                                    if (hasText || summary?.optJSONObject("current") != null) {
                                        ExportTranscript(!working) { format ->
                                            withContext(Dispatchers.IO) {
                                                NativeCore.speechRequest(query(JSONObject().put("op", "export")
                                                    .put("session_id", sessionId).put("format", format)))
                                            }
                                        }
                                    }
                                    SummarySection(
                                        sessionId, summary, inspection?.optJSONArray("summary_providers"),
                                        hasText, idle && !working && loadedRevision == reload,
                                        startRequest = ::startRequest, save = ::saveRevision,
                                        onSaved = { result = null; reload++ },
                                    )
                                    val tracks = inspection?.optJSONArray("tracks")
                                    if (tracks != null && tracks.length() > 0) {
                                        TextButton(onClick = {
                                            showAudio = !showAudio
                                            if (!showAudio && trackFilter != null) {
                                                trackFilter = null
                                                after = null
                                                priorPages.clear()
                                            }
                                        }) {
                                            Text(stringResource(if (showAudio) R.string.note_hide_audio else R.string.note_show_audio))
                                        }
                                    }
                                    if (showAudio) SessionAudio(tracks, trackFilter,
                                        onTrack = {
                                            trackFilter = it
                                            after = null
                                            priorPages.clear()
                                        },
                                        onStop = { playback?.cancel() },
                                        onPlay = { track, at, finished ->
                                            playback?.cancel()
                                            playback = lifecycleScope.launch {
                                                try {
                                                    finished(playSavedTrack(at) { position ->
                                                        NativeCore.speechRequest(query(JSONObject().put("op", "playback")
                                                            .put("session_id", sessionId).put("track_id", track)
                                                            .put("position_ns", position)))
                                                    })
                                                } catch (error: CancellationException) {
                                                    finished(null)
                                                    throw error
                                                }
                                            }
                                        },
                                        archive = { track, chunk ->
                                            val op = if (chunk == null) "track_archive" else "track_archive_chunk"
                                            val action = JSONObject().put("op", op).put("session_id", sessionId).put("track_id", track)
                                            if (chunk != null) action.put("chunk_id", chunk)
                                            NativeCore.speechRequest(query(action))
                                        },
                                        timeline = { track ->
                                            withContext(Dispatchers.IO) {
                                                JSONArray(NativeCore.speechRequest(query(JSONObject().put("op", "track_timeline")
                                                    .put("session_id", sessionId).put("track_id", track))).decodeToString())
                                            }
                                        })
                                    val savedTracks = (0 until (tracks?.length() ?: 0)).mapNotNull { index ->
                                        tracks?.optJSONObject(index)?.takeIf { it.optLong("committed_chunks") > 0 }
                                    }
                                    val providers = inspection?.optJSONArray("external_transcription_providers")
                                    if (showModels && savedTracks.isNotEmpty() && providers != null && providers.length() > 0) {
                                        val enabled = loadedRevision == reload && idle && !working
                                        Button(enabled = enabled, onClick = { externalDialog = true }) {
                                            Text(stringResource(R.string.external_open))
                                        }
                                        if (externalDialog) {
                                            ExternalModelDialog(
                                                sessionId, providers, enabled,
                                                onDismiss = { externalDialog = false },
                                                onSend = {
                                                    externalDialog = false
                                                    startRequest(it)
                                                },
                                                tracks = savedTracks, sourceLanguage = language,
                                            )
                                        }
                                    }
                                }
                                if (sessions && !textOnly) {
                                    TextButton(enabled = !working, onClick = { showModels = !showModels }) {
                                        Text(stringResource(if (showModels) R.string.note_hide_models else R.string.note_show_models))
                                    }
                                }
                                if (!textOnly && (!sessions || showModels)) LanguagePicker(language, !working) { language = it }
                            }
                            val choices = inspection?.getJSONArray("models")
                            val hasEnglish = inspection?.optBoolean("has_translation_source") == true
                            if (!sessions || showModels) for (index in 0 until candidates.length()) {
                                val details = candidates.getJSONObject(index)
                                val model = details.getJSONObject("model")
                                val translation = model.has("translation")
                                val choice = choices?.optJSONObject(index)
                                val installed = choice?.optBoolean("installed") == true
                                val allowed = loadedRevision == reload && choice?.isNull("blocked_by") == true && idle && !working
                                val canProcess = sessionId != null && !textOnly && (!translation || hasEnglish)
                                val currentDownload = model.getString("id") == downloadModelId
                                ModelRow(
                                    details, choice, allowed && (!installed || canProcess),
                                    downloading = currentDownload && working,
                                    downloadedBytes = downloadedBytes.takeIf { currentDownload },
                                    downloadResult = result.takeIf { currentDownload },
                                    cancelDownload = { finishRequest(-4) },
                                ) {
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
                            val lines = if (sessions) notePage?.optJSONArray("lines") else inspection?.optJSONArray("lines")
                            if (sessions) {
                                Text(stringResource(R.string.transcript), style = MaterialTheme.typography.titleLarge)
                                when {
                                    pageLoading -> Text(stringResource(R.string.note_loading_transcript))
                                    notePage == null -> {
                                        Text(stringResource(R.string.note_load_failed))
                                        Button(onClick = { reload++ }) { Text(stringResource(R.string.note_reload_transcript)) }
                                    }
                                    lines?.length() == 0 -> {
                                        Text(stringResource(if (trackFilter == null) R.string.note_empty_transcript else R.string.note_empty_track))
                                        if (!textOnly && !showModels) Button(onClick = {
                                            showModels = true
                                            scope.launch { scroll.scrollTo(0) }
                                        }) { Text(stringResource(R.string.note_choose_model)) }
                                    }
                                }
                            }
                            for (index in 0 until (lines?.length() ?: 0)) {
                                val line = lines?.getJSONObject(index) ?: continue
                                val source = if (line.getString("source") == "mic") R.string.model_mic else R.string.model_loopback
                                val detected = stringResource(detectedLanguage(line.getString("language")))
                                val time = DateUtils.formatElapsedTime(line.getLong("start_ns") / 1_000_000_000)
                                val speakerName = line.optJSONObject("speaker")?.takeUnless { it.isNull("name") }?.getString("name")
                                val caption = stringResource(R.string.note_line, time, stringResource(source), detected)
                                var showEdits by remember(line.getString("utterance_id")) { mutableStateOf(false) }
                                val conflicts = line.getJSONArray("conflicts")
                                val needsReview = conflicts.length() > 1
                                val actionsVisible = !sessions || showEdits
                                Column(Modifier.fillMaxWidth().padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                    Text(if (speakerName.isNullOrBlank()) caption else "$caption · $speakerName",
                                        style = MaterialTheme.typography.labelMedium)
                                    SelectionContainer {
                                        Text(line.getString("text"), style = MaterialTheme.typography.bodyLarge)
                                    }
                                    val revision = line.optJSONObject("translation")
                                    if (revision != null) {
                                        val author = stringResource(if (revision.getString("authorship") == "manual")
                                            R.string.manual_translation else R.string.machine_translation)
                                        Text(stringResource(if (revision.getBoolean("stale"))
                                            R.string.translation_stale else R.string.translation_label, author),
                                            style = MaterialTheme.typography.labelMedium)
                                        SelectionContainer { Text(revision.getString("text")) }
                                    }
                                    if (sessions) TextButton(onClick = { showEdits = !showEdits }) {
                                        Text(stringResource(when {
                                            showEdits -> R.string.note_hide_edits
                                            needsReview -> R.string.sync_resolve_conflict
                                            else -> R.string.note_show_edits
                                        }))
                                    }
                                    if (actionsVisible && sessionId != null) {
                                        if (needsReview) {
                                            for (version in 0 until conflicts.length()) {
                                                val conflict = conflicts.getJSONObject(version)
                                                val author = stringResource(if (conflict.getBoolean("manual")) R.string.sync_manual else R.string.sync_machine)
                                                val lang = stringResource(detectedLanguage(conflict.getString("language")))
                                                Text(stringResource(R.string.sync_revision_author, author, lang))
                                                Text(conflict.getString("text"))
                                            }
                                        }
                                        TextButton(enabled = idle && !working, onClick = {
                                            editKind = if (needsReview) RevisionEditor.Conflict else RevisionEditor.Transcript
                                            edit = line
                                        }) {
                                            Text(stringResource(if (needsReview) R.string.sync_resolve_conflict else R.string.edit_transcript))
                                        }
                                        if (sessions && !needsReview && !line.isNull("external_draft")) {
                                            TextButton(enabled = idle && !working, onClick = {
                                                editKind = RevisionEditor.ExternalDraft
                                                edit = line
                                            }) { Text(stringResource(R.string.external_review_draft)) }
                                        }
                                        if (line.getString("language") == "en") {
                                            TextButton(enabled = idle && !working, onClick = {
                                                editKind = RevisionEditor.Translation
                                                edit = line
                                            }) { Text(stringResource(if (revision == null) R.string.add_translation else R.string.edit_translation)) }
                                        }
                                        if (sessions) {
                                            TextButton(enabled = idle && !working, onClick = {
                                                editKind = RevisionEditor.Speaker
                                                edit = line
                                            }) { Text(stringResource(R.string.session_edit_speaker)) }
                                        }
                                    }
                                }
                            }
                            if (sessions && !pageLoading && notePage != null) {
                                val next = notePage?.optJSONObject("next")?.toString()
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    if (priorPages.isNotEmpty()) TextButton(onClick = {
                                        after = priorPages.removeAt(priorPages.lastIndex)
                                        notePage = null
                                        scope.launch { scroll.scrollTo(0) }
                                    }) { Text(stringResource(R.string.note_previous_page)) }
                                    if (next != null) TextButton(onClick = {
                                        priorPages.add(after)
                                        after = next
                                        notePage = null
                                        scope.launch { scroll.scrollTo(0) }
                                    }) { Text(stringResource(R.string.note_next_page)) }
                                }
                            }
                            edit?.let { line ->
                                if (sessionId != null) {
                                    val speaker = line.optJSONObject("speaker")
                                    val initial = when (editKind) {
                                        RevisionEditor.Transcript, RevisionEditor.Conflict -> line.getString("text")
                                        RevisionEditor.ExternalDraft -> line.getString("external_draft")
                                        RevisionEditor.Translation -> line.optJSONObject("translation")?.getString("text").orEmpty()
                                        RevisionEditor.Speaker -> speaker?.takeUnless { it.isNull("name") }?.getString("name").orEmpty()
                                    }
                                    val isSpeaker = editKind == RevisionEditor.Speaker
                                    TextRevisionEditor(initial, editKind.label, idle && !working, field = editKind.field,
                                        maxLength = if (isSpeaker) 64 else 8192, allowBlank = isSpeaker,
                                        onDismiss = { edit = null }) { text ->
                                        val action = JSONObject().put("session_id", sessionId)
                                            .put("utterance_id", line.getString("utterance_id"))
                                            .put("text", text)
                                        val revision = line.getString("revision_id")
                                        when (editKind) {
                                            RevisionEditor.Transcript, RevisionEditor.ExternalDraft ->
                                                action.put("op", "save_utterance").put("expected_revision_id", revision)
                                            RevisionEditor.Conflict -> {
                                                val conflicts = line.getJSONArray("conflicts")
                                                val heads = JSONArray()
                                                for (index in 0 until conflicts.length())
                                                    heads.put(conflicts.getJSONObject(index).getString("revision_id"))
                                                action.put("op", "resolve_conflict").put("expected_heads", heads)
                                            }
                                            RevisionEditor.Translation -> action.put("op", "save_translation").put("source_revision_id", revision)
                                                .put("expected_translation_id", line.optJSONObject("translation")?.getString("revision_id") ?: JSONObject.NULL)
                                            RevisionEditor.Speaker -> action.put("op", "save_speaker").put("expected_revision_id", revision)
                                                .put("expected_speaker_id", speaker?.getString("revision_id") ?: JSONObject.NULL)
                                        }
                                        saveRevision(action).also { code ->
                                            if (code == 0) {
                                                edit = null
                                                downloadModelId = null
                                                result = code
                                                requestOperation = null
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
        playback?.cancel()
        if (working) finishRequest(-4)
        super.onStop()
    }

    private fun startRequest(action: JSONObject) {
        if (working || RecordingStatusStore.state.value.phase != RecordingPhase.Idle) return
        val operation = action.getString("op")
        val external = operation == "transcribe_external" || operation == "summarize"
        requestOperation = operation
        downloadModelId = if (operation == "install") action.getString("model_id") else null
        downloadedBytes = null
        working = true
        result = null
        partial = null
        preparing = lifecycleScope.launch {
            val refreshed = external || withContext(Dispatchers.IO) {
                runCatching { DeviceProfiler.capture(applicationContext) }.isSuccess
            }
            if (!working) return@launch
            if (!refreshed || RecordingStatusStore.state.value.phase != RecordingPhase.Idle) {
                finishRequest(-34)
                return@launch
            }
            val request = SpeechConnection(this@ModelsActivity, query(action), ::finishRequest) { bytes, _ ->
                val progress = bytes?.takeIf { it.isNotEmpty() }?.let {
                    runCatching { JSONObject(it.decodeToString()) }.getOrNull()
                }
                if (downloadModelId != null) {
                    progress?.optLong("downloaded_bytes", -1L)?.takeIf { it >= 0 }?.let { downloadedBytes = it }
                } else {
                    partial = progress?.optString("text")?.takeIf { it.isNotEmpty() }
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

    private suspend fun saveRevision(action: JSONObject): Int = withContext(Dispatchers.IO) {
        try {
            NativeCore.speechRequest(query(action))
            0
        } catch (failure: CoreException) {
            failure.code
        } catch (_: LinkageError) {
            -31
        }
    }

    private fun query(action: JSONObject): String = speechRequest(this, action)
}

@Composable
private fun SummarySection(
    sessionId: String,
    summary: JSONObject?,
    presets: JSONArray?,
    hasText: Boolean,
    enabled: Boolean,
    startRequest: (JSONObject) -> Unit,
    save: suspend (JSONObject) -> Int,
    onSaved: () -> Unit,
) {
    val current = summary?.optJSONObject("current")
    val draft = summary?.optJSONObject("draft")
    var editor by remember(sessionId) { mutableStateOf<Int?>(null) }
    var generate by remember(sessionId) { mutableStateOf(false) }
    val canGenerate = hasText && presets != null && presets.length() > 0
    Text(stringResource(R.string.note_summary), style = MaterialTheme.typography.titleLarge)
    if (current == null) {
        Text(stringResource(R.string.summary_empty))
        Button(enabled = enabled, onClick = { editor = R.string.summary_create }) {
            Text(stringResource(R.string.summary_create))
        }
    } else {
        if (current.optBoolean("stale")) Text(stringResource(R.string.summary_stale))
        SelectionContainer {
            Text(current.getString("text"), style = MaterialTheme.typography.bodyLarge)
        }
        TextButton(enabled = enabled, onClick = { editor = R.string.summary_edit }) {
            Text(stringResource(R.string.summary_edit))
        }
        if (canGenerate) TextButton(enabled = enabled, onClick = { generate = true }) {
            Text(stringResource(R.string.summary_generate))
        }
    }
    if (draft != null) {
        if (draft.optBoolean("stale")) Text(stringResource(R.string.summary_stale_draft))
        else TextButton(enabled = enabled, onClick = { editor = R.string.summary_review }) {
            Text(stringResource(R.string.summary_review))
        }
    }
    editor?.let { label ->
        val reviewed = draft.takeIf { label == R.string.summary_review }
        TextRevisionEditor(
            (reviewed ?: current)?.getString("text").orEmpty(), label, enabled,
            field = R.string.note_summary, maxLength = 8192, allowBlank = false,
            onDismiss = { editor = null },
            alternate = if (current == null && canGenerate) {
                { editor = null; generate = true }
            } else null,
            operation = "save_summary",
        ) { text ->
            val action = JSONObject().put("op", "save_summary").put("session_id", sessionId)
                .put("expected_revision_id", current?.getString("id") ?: JSONObject.NULL).put("text", text)
            if (reviewed != null) action.put("draft_id", reviewed.getString("id"))
            save(action).also { code ->
                if (code == 0) {
                    editor = null
                    onSaved()
                }
            }
        }
    }
    if (generate && presets != null) {
        ExternalModelDialog(
            sessionId, presets, enabled,
            onDismiss = { generate = false },
            onSend = {
                generate = false
                startRequest(it)
            },
        )
    }
}

@Composable
private fun ExportTranscript(enabled: Boolean, prepare: suspend (String) -> ByteArray) {
    val app = LocalContext.current
    val scope = rememberCoroutineScope()
    var expanded by remember { mutableStateOf(false) }
    var selected by remember { mutableStateOf<Pair<String, String>?>(null) }
    var data by remember { mutableStateOf<ByteArray?>(null) }
    var exporting by remember { mutableStateOf(false) }
    var outcome by remember { mutableStateOf<Int?>(null) }
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val bytes = data
        data = null
        if (uri == null || bytes == null) {
            bytes?.fill(0)
            exporting = false
        } else {
            scope.launch {
                val written = withContext(Dispatchers.IO) {
                    try {
                        val stream = app.contentResolver.openOutputStream(uri)
                        if (stream == null) false
                        else {
                            stream.use { it.write(bytes) }
                            true
                        }
                    } catch (_: Exception) {
                        false
                    } finally { bytes.fill(0) }
                }
                outcome = if (written) R.string.export_complete else R.string.export_failed
                exporting = false
            }
        }
    }
    Button(onClick = { expanded = true }, enabled = enabled && !exporting) {
        Text(stringResource(R.string.export_note))
    }
    DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
        for ((label, format, extension) in listOf(
            Triple(R.string.export_text, "text", "txt"),
            Triple(R.string.export_markdown, "markdown", "md"),
            Triple(R.string.export_json, "json", "json"),
        )) {
            DropdownMenuItem(text = { Text(stringResource(label)) }, onClick = {
                expanded = false
                selected = format to extension
            })
        }
    }
    selected?.let { (format, extension) ->
        AlertDialog(
            onDismissRequest = { selected = null },
            text = { Text(stringResource(R.string.export_plaintext)) },
            confirmButton = {
                TextButton(onClick = {
                    selected = null
                    exporting = true
                    outcome = null
                    scope.launch {
                        try {
                            data = prepare(format)
                            picker.launch("VoiceLedger-note.$extension")
                        } catch (_: Exception) {
                            data?.fill(0)
                            data = null
                            outcome = R.string.export_failed
                            exporting = false
                        }
                    }
                }) { Text(stringResource(R.string.export_note)) }
            },
            dismissButton = { TextButton(onClick = { selected = null }) { Text(stringResource(R.string.cancel_speech)) } },
        )
    }
    outcome?.let { Text(stringResource(it)) }
}

internal fun detectedLanguage(code: String): Int = when (code) {
    "zh" -> R.string.model_chinese
    "ja" -> R.string.model_japanese
    else -> R.string.model_english
}

private enum class RevisionEditor(val label: Int, val field: Int) {
    Transcript(R.string.edit_transcript, R.string.transcript),
    ExternalDraft(R.string.external_review_draft, R.string.transcript),
    Conflict(R.string.sync_resolve_conflict, R.string.transcript),
    Translation(R.string.edit_translation, R.string.chinese_translation),
    Speaker(R.string.session_edit_speaker, R.string.session_speaker_name),
}

@Composable
private fun TextRevisionEditor(
    initial: String,
    label: Int,
    enabled: Boolean,
    field: Int,
    maxLength: Int,
    allowBlank: Boolean,
    onDismiss: () -> Unit,
    alternate: (() -> Unit)? = null,
    operation: String? = null,
    save: suspend (String) -> Int,
) {
    val scope = rememberCoroutineScope()
    var text by remember(initial) { mutableStateOf(initial) }
    var saving by remember(initial) { mutableStateOf(false) }
    var error by remember(initial) { mutableStateOf<Int?>(null) }
    AlertDialog(
        onDismissRequest = { if (!saving) onDismiss() },
        title = { Text(stringResource(label)) },
        text = {
            Column {
                OutlinedTextField(
                    value = text,
                    onValueChange = { if (it.length <= maxLength && it.encodeToByteArray().size <= 8192) text = it },
                    label = { Text(stringResource(field)) }, singleLine = allowBlank,
                    maxLines = if (allowBlank) 1 else 12,
                    readOnly = saving,
                )
                if (alternate != null) {
                    TextButton(enabled = enabled && !saving && text.isBlank(), onClick = alternate) {
                        Text(stringResource(R.string.summary_generate))
                    }
                }
                error?.let { Text(stringResource(resultText(it, operation))) }
            }
        },
        confirmButton = {
            TextButton(
                enabled = enabled && !saving && (allowBlank || text.isNotBlank()),
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
                Text(stringResource(R.string.save_revision))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !saving) { Text(stringResource(R.string.cancel_speech)) }
        },
    )
}

@Composable
private fun ModelRow(
    candidate: JSONObject,
    choice: JSONObject?,
    enabled: Boolean,
    downloading: Boolean,
    downloadedBytes: Long?,
    downloadResult: Int?,
    cancelDownload: () -> Unit,
    action: () -> Unit,
) {
    val model = candidate.getJSONObject("model")
    val runtime = model.getJSONObject("runtime")
    val languageCodes = model.getJSONArray("languages")
    val languages = (0 until languageCodes.length()).joinToString(", ") { languageCodes.getString(it) }
    val estimate = model.optJSONObject("peak_ram_estimate")
    val total = candidate.getLong("download_bytes")
    Column(Modifier.padding(top = 8.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
        Text(
            stringResource(R.string.model_title, model.getString("name"), model.getString("quantization")),
            style = MaterialTheme.typography.titleMedium,
        )
        Text(stringResource(R.string.model_runtime, runtime.getString("id"), runtime.getString("version"),
            model.getJSONObject("license").getString("id")))
        Text(stringResource(R.string.model_download, languages, total / 1048576.0))
        Text(
            if (estimate == null) stringResource(R.string.model_estimate_unknown)
            else stringResource(R.string.model_estimate, estimate.getLong("bytes") / 1048576.0),
        )
        if (downloading) {
            Text(
                when {
                    downloadedBytes == null -> stringResource(R.string.model_download_starting)
                    downloadedBytes >= total -> stringResource(R.string.model_download_verifying)
                    else -> stringResource(R.string.model_download_progress, downloadedBytes * 100.0 / total)
                },
            )
            Button(onClick = cancelDownload) { Text(stringResource(R.string.cancel_speech)) }
            return@Column
        }
        val blockedBy = choice?.optString("blocked_by") ?: "unknown"
        val blocked = choice?.isNull("blocked_by") == false
        val status = when (downloadResult) {
            0 -> if (blocked) reason(blockedBy) else R.string.model_download_complete
            -30 -> R.string.model_download_storage_failed
            -31 -> R.string.model_download_runtime_failed
            -34 -> if (blocked) reason(blockedBy) else R.string.speech_blocked
            null -> reason(blockedBy)
            else -> resultText(downloadResult)
        }
        Text(stringResource(status))
        val actionLabel = when {
            choice?.optBoolean("installed") != true && downloadResult == -35 -> R.string.model_download_retry
            choice?.optBoolean("installed") != true -> R.string.download_model
            model.has("translation") -> R.string.translate_saved
            else -> R.string.transcribe_saved
        }
        Button(onClick = action, enabled = enabled) { Text(stringResource(actionLabel)) }
    }
}

@Composable
internal fun LanguagePicker(policy: String, enabled: Boolean, selected: (String) -> Unit) {
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

private fun resultText(code: Int, operation: String? = null): Int = when (code) {
    0 -> when (operation) {
        "summarize" -> R.string.summary_generated
        "transcribe_external" -> R.string.external_complete
        else -> R.string.speech_complete
    }
    -4 -> R.string.speech_cancelled
    -32 -> R.string.speech_no_audio
    -33 -> R.string.speech_not_installed
    -34 -> R.string.speech_blocked
    -35 -> R.string.speech_transfer
    -36 -> R.string.speech_integrity
    -37 -> R.string.speech_busy
    -38 -> when (operation) {
        "summarize" -> R.string.summary_deferred
        "transcribe_external" -> R.string.external_deferred
        else -> R.string.speech_deferred
    }
    -39 -> if (operation == "summarize" || operation == "save_summary") R.string.summary_conflict else R.string.translation_conflict
    -40 -> R.string.translation_limit
    -41 -> R.string.external_configuration
    -42 -> R.string.external_authentication
    -43 -> R.string.external_rate_limit
    -44 -> R.string.external_connection
    -45 -> if (operation == "summarize") R.string.summary_response else R.string.external_response
    -46 -> R.string.note_empty_transcript
    -47 -> R.string.summary_input_limit
    else -> when (operation) {
        "summarize", "save_summary" -> R.string.summary_failed
        "transcribe_external" -> R.string.external_failed
        else -> R.string.speech_failed
    }
}

private fun jobStatus(job: JSONObject): Int? = when (job.optString("state")) {
    "running" -> R.string.note_transcription_finishing
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
