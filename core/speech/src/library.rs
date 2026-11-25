use std::io::{ErrorKind, Write};
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use voiceledger_audio::{CommittedReader, FinishedSession, SavedSession};
use voiceledger_core::contracts::{Authorship, SpeakerAssignment, StableId, UtteranceKind};

use crate::transcript::{PATH, Transcripts, timestamp, visit_selected};
use crate::translations::{MAX_TEXT, selected_source};
use crate::{Error, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE session_titles (
    session_id TEXT PRIMARY KEY,
    title TEXT NOT NULL
) STRICT;
CREATE TABLE speaker_labels (
    utterance_id TEXT NOT NULL REFERENCES utterances(id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    payload TEXT NOT NULL,
    PRIMARY KEY (utterance_id, revision)
) STRICT;";

#[derive(Serialize)]
pub(crate) struct SessionEntry {
    #[serde(flatten)]
    audio: SavedSession,
    has_transcript: bool,
    title: Option<String>,
}

pub(crate) fn history(
    root: &Path,
    reader: Option<&CommittedReader>,
    filter: Option<&str>,
) -> Result<Vec<SessionEntry>> {
    let filter = filter.unwrap_or_default().trim();
    if filter.len() > 256 || filter.contains('\0') {
        return Err(Error::Invalid);
    }
    let Some(reader) = reader else {
        return Ok(Vec::new());
    };
    let transcripts = open_readonly(root)?;
    let has_metadata = transcripts
        .as_ref()
        .is_some_and(|(_, version)| *version >= 4);
    let mut match_text = transcripts
        .as_ref()
        .map(|(db, _)| {
            db.prepare(
                "SELECT EXISTS (
            SELECT 1 FROM utterances u JOIN revisions r ON r.id=u.selected_revision
            WHERE u.session_id=?1
        ), EXISTS (
            SELECT 1 FROM utterances u JOIN revisions r ON r.id=u.selected_revision
            WHERE u.session_id=?1 AND instr(lower(json_extract(r.payload,'$.text')), lower(?2)) > 0
        )",
            )
        })
        .transpose()?;
    let mut match_speaker = if has_metadata {
        let (db, _) = transcripts.as_ref().ok_or(Error::Storage)?;
        Some(db.prepare(
            "SELECT EXISTS (SELECT 1 FROM utterances u
             WHERE u.session_id=?1 AND instr(lower((
                 SELECT json_extract(s.payload, '$.confirmed_name') FROM speaker_labels s
                 WHERE s.utterance_id=u.id ORDER BY s.revision DESC LIMIT 1
             )), lower(?2)) > 0)",
        )?)
    } else {
        None
    };
    let folded_filter = filter.to_lowercase();
    let mut output = Vec::new();
    let mut cursor = None;
    while output.len() < 100 {
        let page = reader.saved_sessions(cursor.as_ref().map(|(at, id)| (*at, id)))?;
        let Some(last) = page.last() else { break };
        cursor = Some((last.created_at_ms, last.id.clone()));
        for audio in page {
            let id = String::from(audio.id.clone());
            let (has_transcript, text_match): (bool, bool) = match_text
                .as_mut()
                .map(|search| {
                    search.query_row(params![id, filter], |row| Ok((row.get(0)?, row.get(1)?)))
                })
                .transpose()?
                .unwrap_or_default();
            let title = transcripts
                .as_ref()
                .filter(|(_, version)| *version >= 4)
                .map(|(db, _)| read_title(db, &id))
                .transpose()?
                .flatten();
            let speaker_match: bool = match_speaker
                .as_mut()
                .map(|search| search.query_row(params![id, filter], |row| row.get(0)))
                .transpose()?
                .unwrap_or(false);
            let title_match = title
                .as_ref()
                .is_some_and(|title| title.to_lowercase().contains(&folded_filter));
            if filter.is_empty() || text_match || title_match || speaker_match {
                output.push(SessionEntry {
                    audio,
                    has_transcript,
                    title,
                });
                if output.len() == 100 {
                    break;
                }
            }
        }
    }
    Ok(output)
}

fn open_readonly(root: &Path) -> Result<Option<(Connection, i64)>> {
    match root.join(PATH).metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(150))?;
    let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(Some((db, version)))
}

fn read_title(db: &Connection, session: &str) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT title FROM session_titles WHERE session_id=?1",
            [session],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn title(root: &Path, session: &StableId) -> Result<Option<String>> {
    match open_readonly(root)? {
        Some((db, version)) if version >= 4 => read_title(&db, &String::from(session.clone())),
        _ => Ok(None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TitleEdit {
    pub session_id: StableId,
    pub text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpeakerEdit {
    pub session_id: StableId,
    pub utterance_id: StableId,
    pub expected_revision_id: StableId,
    pub expected_speaker_id: Option<StableId>,
    pub text: String,
}

#[derive(Serialize)]
pub struct SpeakerLine {
    revision_id: StableId,
    pub(crate) name: Option<String>,
}

fn saved_speaker(db: &Connection, utterance: &str) -> Result<Option<SpeakerAssignment>> {
    let payload: Option<String> = db.query_row(
        "SELECT payload FROM speaker_labels WHERE utterance_id=?1 ORDER BY revision DESC LIMIT 1",
        [utterance],
        |row| row.get(0),
    ).optional()?;
    Ok(payload
        .map(|json| serde_json::from_str(&json))
        .transpose()?)
}

pub(crate) fn speaker(db: &Connection, utterance: &StableId) -> Result<Option<SpeakerLine>> {
    Ok(
        saved_speaker(db, &String::from(utterance.clone()))?.map(|saved| SpeakerLine {
            revision_id: saved.id,
            name: saved.confirmed_name,
        }),
    )
}

fn short_text(value: &str) -> Result<Option<&str>> {
    let value = value.trim();
    if value.chars().count() > 64 || value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok((!value.is_empty()).then_some(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UtteranceEdit {
    pub session_id: StableId,
    pub utterance_id: StableId,
    pub expected_revision_id: StableId,
    pub text: String,
}

impl Transcripts {
    pub(crate) fn rename(&self, edit: TitleEdit) -> Result<()> {
        let session = String::from(edit.session_id);
        match short_text(&edit.text)? {
            Some(title) => self.db.execute(
                "INSERT INTO session_titles(session_id,title) VALUES (?1,?2)
                 ON CONFLICT(session_id) DO UPDATE SET title=excluded.title",
                params![session, title],
            )?,
            None => self
                .db
                .execute("DELETE FROM session_titles WHERE session_id=?1", [&session])?,
        };
        Ok(())
    }

    pub(crate) fn save_speaker(&self, edit: SpeakerEdit) -> Result<()> {
        let name = short_text(&edit.text)?.map(str::to_owned);
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let source = selected_source(&tx, &edit.session_id, &edit.utterance_id)?
            .filter(|value| value.id == edit.expected_revision_id && value.is_durable())
            .ok_or(Error::RevisionConflict)?;
        let utterance = String::from(edit.utterance_id);
        let prior = saved_speaker(&tx, &utterance)?;
        if prior.as_ref().map(|value| &value.id) != edit.expected_speaker_id.as_ref() {
            return Err(Error::RevisionConflict);
        }
        let revision = prior
            .as_ref()
            .map_or(0, |value| value.revision.get())
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .ok_or(Error::Storage)?;
        let speaker_id = match name {
            Some(_) => Some(match prior.and_then(|value| value.speaker_id) {
                Some(id) => id,
                None => self.id()?,
            }),
            None => None,
        };
        let assignment = SpeakerAssignment {
            id: self.id()?,
            session_id: edit.session_id,
            track_id: source.track_id,
            speaker_id,
            range: source.range,
            revision,
            authorship: Authorship::Manual,
            confidence: None,
            model: None,
            confirmed_name: name,
            confirmed_profile_id: None,
        };
        tx.execute(
            "INSERT INTO speaker_labels(utterance_id,revision,payload) VALUES (?1,?2,?3)",
            params![
                utterance,
                revision.get(),
                serde_json::to_string(&assignment)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn save_utterance(&self, edit: UtteranceEdit) -> Result<()> {
        let text = edit.text.trim();
        if text.is_empty() || text.len() > MAX_TEXT || text.contains('\0') {
            return Err(Error::Invalid);
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let mut revision = selected_source(&tx, &edit.session_id, &edit.utterance_id)?
            .filter(|source| source.id == edit.expected_revision_id && source.is_durable())
            .ok_or(Error::RevisionConflict)?;
        let utterance = String::from(edit.utterance_id);
        let ordinal: u32 = tx.query_row(
            "SELECT max(revision) FROM revisions WHERE utterance_id=?1",
            [&utterance],
            |row| row.get(0),
        )?;
        revision.parent_revision_ids = vec![revision.id];
        revision.id = self.id()?;
        revision.revision =
            NonZeroU32::new(ordinal.checked_add(1).ok_or(Error::Storage)?).ok_or(Error::Storage)?;
        revision.kind = UtteranceKind::Manual;
        revision.text = text.to_owned();
        revision.model = None;
        revision.confidence = None;
        revision.created_at_unix_ms = timestamp()?;
        let id = String::from(revision.id.clone());
        tx.execute(
            "INSERT INTO revisions(id,utterance_id,revision,payload) VALUES (?1,?2,?3,?4)",
            params![
                id,
                utterance,
                revision.revision.get(),
                serde_json::to_string(&revision)?
            ],
        )?;
        tx.execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2",
            params![id, utterance],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExportFormat {
    Text,
    Markdown,
    Json,
}

pub(crate) fn export(
    root: &Path,
    session: &FinishedSession,
    format: ExportFormat,
) -> Result<Vec<u8>> {
    const LIMIT: usize = 32 * 1024 * 1024;
    let mut output = Vec::new();
    let name = title(root, &session.id)?;
    match format {
        ExportFormat::Text => writeln!(output, "VoiceLedger transcript")?,
        ExportFormat::Markdown => writeln!(output, "# VoiceLedger transcript")?,
        ExportFormat::Json => {
            write!(output, "{{\"schema_version\":1,\"session\":")?;
            serde_json::to_writer(&mut output, session)?;
            write!(output, ",\"title\":")?;
            serde_json::to_writer(&mut output, &name)?;
            write!(output, ",\"lines\":[")?;
        }
    }
    if let Some(name) = name
        .as_deref()
        .filter(|_| !matches!(format, ExportFormat::Json))
    {
        write_text(&mut output, name, format)?;
    }
    let mut first = true;
    visit_selected(root, &session.id, -1, |line| {
        if matches!(format, ExportFormat::Json) {
            if !first {
                output.push(b',');
            }
            serde_json::to_writer(&mut output, &line)?;
        } else {
            let seconds = line.start_ns / 1_000_000_000;
            let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
            let source = if line.source == "mic" {
                "Microphone"
            } else {
                "System loopback"
            };
            let speaker = line
                .speaker
                .as_ref()
                .and_then(|speaker| speaker.name.as_deref());
            writeln!(output, "\n{hours:02}:{minutes:02}:{seconds:02} · {source}")?;
            if let Some(speaker) = speaker {
                write_text(&mut output, speaker, format)?;
            }
            write_text(&mut output, &line.text, format)?;
            if let Some(translation) = line.translation {
                let status = if translation.stale {
                    " · Outdated source"
                } else {
                    ""
                };
                writeln!(output, "Simplified Chinese{status}")?;
                write_text(&mut output, &translation.text, format)?;
            }
        }
        if output.len() > LIMIT {
            return Err(Error::Storage);
        }
        first = false;
        Ok(())
    })?;
    if matches!(format, ExportFormat::Json) {
        output.extend_from_slice(b"]}\n");
    }
    Ok(output)
}

fn write_text(output: &mut Vec<u8>, text: &str, format: ExportFormat) -> Result<()> {
    if matches!(format, ExportFormat::Markdown) {
        output.push(b'\n');
        for line in text.lines() {
            // Indented code keeps arbitrary transcript text literal in Markdown renderers.
            writeln!(output, "    {line}")?;
        }
    } else {
        writeln!(output, "{text}")?;
    }
    Ok(())
}
