use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use voiceledger_audio::{CommittedReader, SavedSession};
use voiceledger_core::contracts::{StableId, UtteranceKind};

use crate::transcript::{PATH, Transcripts, timestamp};
use crate::translations::{MAX_TEXT, selected_source};
use crate::{Error, Result};

#[derive(Serialize)]
pub(crate) struct SessionEntry {
    #[serde(flatten)]
    audio: SavedSession,
    has_transcript: bool,
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
    let transcripts = match root.join(PATH).metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
        Ok(_) => {
            let db =
                Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            db.busy_timeout(Duration::from_millis(150))?;
            Some(db)
        }
    };
    let mut match_text = transcripts.as_ref().map(|db| db.prepare(
        "SELECT EXISTS (
            SELECT 1 FROM utterances u JOIN revisions r ON r.id=u.selected_revision
            WHERE u.session_id=?1 AND (?2='' OR instr(lower(json_extract(r.payload,'$.text')), lower(?2)) > 0)
        )",
    )).transpose()?;
    let mut output = Vec::new();
    let mut cursor = None;
    while output.len() < 100 {
        let page = reader.saved_sessions(cursor.as_ref().map(|(at, id)| (*at, id)))?;
        let Some(last) = page.last() else { break };
        cursor = Some((last.created_at_ms, last.id.clone()));
        for audio in page {
            let has_transcript: bool = match_text
                .as_mut()
                .map(|search| {
                    search.query_row(params![String::from(audio.id.clone()), filter], |row| {
                        row.get(0)
                    })
                })
                .transpose()?
                .unwrap_or(false);
            if filter.is_empty() || has_transcript {
                output.push(SessionEntry {
                    audio,
                    has_transcript,
                });
                if output.len() == 100 {
                    break;
                }
            }
        }
    }
    Ok(output)
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
