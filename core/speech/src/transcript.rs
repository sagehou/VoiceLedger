use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use voiceledger_core::contracts::{
    Count, Language, LanguagePolicy, ModelRef, StableId, UtteranceKind, UtteranceRevision,
};

use crate::jobs::{self, Queue};
use crate::pipeline::{Final, Output, Partial};
use crate::translations::{self, TranslationLine};
use crate::{Error, Result};

pub(crate) const PATH: &str = "transcript-v1.sqlite3";
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS utterances (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    start_ns INTEGER NOT NULL,
    end_ns INTEGER NOT NULL CHECK (end_ns > start_ns),
    selected_revision TEXT,
    UNIQUE (session_id,track_id,start_ns,end_ns)
) STRICT;
CREATE TABLE IF NOT EXISTS revisions (
    id TEXT PRIMARY KEY,
    utterance_id TEXT NOT NULL REFERENCES utterances(id),
    revision INTEGER NOT NULL CHECK (revision > 0),
    machine_key TEXT,
    payload TEXT NOT NULL,
    UNIQUE (utterance_id, revision),
    UNIQUE (utterance_id, machine_key)
) STRICT;";

pub(crate) struct Transcripts {
    pub(crate) db: Connection,
}

impl Transcripts {
    pub fn open(root: &Path) -> Result<Self> {
        let mut db = Connection::open(root.join(PATH))?;
        db.busy_timeout(Duration::from_millis(500))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if !(0..=5).contains(&version) {
            return Err(Error::Storage);
        }
        if version == 0 {
            tx.execute_batch(SCHEMA)?;
        }
        if version < 2 {
            tx.execute_batch(jobs::SCHEMA)?;
        }
        if version < 3 {
            tx.execute_batch(translations::SCHEMA)?;
        }
        if version < 4 {
            tx.execute_batch(crate::library::SCHEMA)?;
        }
        if version < 5 {
            tx.execute_batch(crate::sync_data::SCHEMA)?;
            tx.execute_batch("PRAGMA user_version=5;")?;
        }
        tx.commit()?;
        Ok(Self { db })
    }

    pub fn queue(&self) -> Queue<'_> {
        Queue(&self.db)
    }

    pub fn id(&self) -> Result<StableId> {
        let mut bytes: Vec<u8> = self
            .db
            .query_row("SELECT randomblob(16)", [], |row| row.get(0))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut uuid = String::with_capacity(36);
        for (index, byte) in bytes.into_iter().enumerate() {
            use std::fmt::Write;
            if matches!(index, 4 | 6 | 8 | 10) {
                uuid.push('-');
            }
            write!(uuid, "{byte:02x}").map_err(|_| Error::Invalid)?;
        }
        uuid.try_into().map_err(|_| Error::Invalid)
    }

    fn save(
        &self,
        session_id: &StableId,
        model: &ModelRef,
        language: LanguagePolicy,
        job: Option<&str>,
        translate: Option<&ModelRef>,
        value: Final,
    ) -> Result<bool> {
        let session = String::from(session_id.clone());
        let track = String::from(value.track_id.clone());
        let start = value.range.start_ns.get() as i64;
        let end = value.range.end_ns.get() as i64;
        let provisional = String::from(value.id);
        let machine_key = serde_json::to_string(&(
            &model.model_id,
            &model.model_version,
            &model.runtime_version,
            &model.sha256,
            language,
        ))?;
        let tx = self.db.unchecked_transaction()?;
        if let Some(job) = job {
            Queue(&tx).checkpoint(job, &value.track_id, end)?;
        }
        tx.execute(
            "INSERT INTO utterances(id,session_id,track_id,start_ns,end_ns) VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(session_id,track_id,start_ns,end_ns) DO NOTHING",
            params![provisional, session, track, start, end],
        )?;
        let utterance: String = tx.query_row(
            "SELECT id FROM utterances WHERE session_id=?1 AND track_id=?2 AND start_ns=?3 AND end_ns=?4",
            params![session, track, start, end], |row| row.get(0),
        )?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE utterance_id=?1 AND machine_key=?2)",
            params![utterance, machine_key],
            |row| row.get(0),
        )?;
        if exists {
            tx.commit()?;
            return Ok(false);
        }
        let prior: Option<(String, u32)> = tx.query_row(
            "SELECT id,revision FROM revisions WHERE utterance_id=?1 ORDER BY revision DESC LIMIT 1",
            [&utterance], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?;
        let (ordinal, parents) = match prior {
            Some((id, _)) => (
                crate::sync_data::next_ordinal(&tx, &utterance)?,
                vec![id.try_into().map_err(|_| Error::Invalid)?],
            ),
            None => (1, Vec::new()),
        };
        let id = self.id()?;
        let revision = UtteranceRevision {
            id: id.clone(),
            utterance_id: utterance.clone().try_into().map_err(|_| Error::Invalid)?,
            revision: NonZeroU32::new(ordinal).ok_or(Error::Invalid)?,
            parent_revision_ids: parents,
            track_id: value.track_id,
            source: value.spans,
            range: value.range,
            kind: UtteranceKind::Final,
            language: value.recognition.language,
            text: value.recognition.text,
            confidence: None,
            model: Some(model.clone()),
            created_at_unix_ms: timestamp()?,
        };
        let id = String::from(id);
        tx.execute(
            "INSERT INTO revisions(id,utterance_id,revision,machine_key,payload) VALUES (?1,?2,?3,?4,?5)",
            params![id, utterance, ordinal, machine_key, serde_json::to_string(&revision)?],
        )?;
        let selected = tx.execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2 AND selected_revision IS NULL",
            params![id, utterance],
        )?;
        if selected != 0
            && revision.language == Language::En
            && let Some(model) = translate
        {
            Queue(&tx).enqueue(
                self.id()?,
                session_id,
                voiceledger_core::scheduler::JobKind::Translation,
                &(&revision.id, Language::Zh, model),
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
}

pub(crate) struct Writer<'a> {
    pub transcripts: &'a Transcripts,
    pub session: &'a StableId,
    pub model: &'a ModelRef,
    pub language: LanguagePolicy,
    pub job: Option<&'a str>,
    pub translate: Option<&'a ModelRef>,
    pub publish_partial: &'a mut dyn FnMut(Option<Partial>),
    pub saved: u64,
}

impl Output for Writer<'_> {
    fn id(&mut self) -> Result<StableId> {
        self.transcripts.id()
    }

    fn partial(&mut self, value: Option<Partial>) -> Result<()> {
        (self.publish_partial)(value);
        Ok(())
    }

    fn finish(&mut self, value: Final) -> Result<()> {
        self.saved += u64::from(self.transcripts.save(
            self.session,
            self.model,
            self.language,
            self.job,
            self.translate,
            value,
        )?);
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Line {
    pub source: String,
    pub start_ns: u64,
    pub utterance_id: StableId,
    pub revision_id: StableId,
    pub language: Language,
    pub text: String,
    pub translation: Option<TranslationLine>,
    pub speaker: Option<crate::library::SpeakerLine>,
    pub(crate) conflicts: Vec<crate::sync_data::ConflictRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) external_draft: Option<String>,
}

pub fn selected(root: &Path, session: &StableId) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    visit_selected(root, session, 40, false, |line| {
        lines.push(line);
        Ok(())
    })?;
    Ok(lines)
}

pub(crate) fn visit_selected(
    root: &Path,
    session: &StableId,
    limit: i64,
    latest: bool,
    mut visit: impl FnMut(Line) -> Result<()>,
) -> Result<()> {
    match root.join(PATH).metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(150))?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let audio = crate::sync_data::open_audio(root)?;
    let session = String::from(session.clone());
    if version >= 5 && crate::sync_data::hidden(&db, "session", &session)? {
        return Ok(());
    }
    let suffix = if latest && version >= 5 {
        " AND NOT EXISTS(SELECT 1 FROM synced_tombstones t WHERE t.scope='utterance' AND t.entity_id=u.id)
          ORDER BY u.start_ns DESC,u.track_id DESC LIMIT ?2"
    } else if latest {
        " ORDER BY u.start_ns DESC,u.track_id DESC LIMIT ?2"
    } else if version >= 5 {
        " AND NOT EXISTS(SELECT 1 FROM synced_tombstones t WHERE t.scope='utterance' AND t.entity_id=u.id)
          ORDER BY (CASE WHEN ?2<0 THEN 0 ELSE EXISTS(SELECT 1 FROM sync_conflicts c WHERE c.utterance_id=u.id) END) DESC,
            u.start_ns,u.track_id LIMIT ?2"
    } else {
        " ORDER BY u.start_ns,u.track_id LIMIT ?2"
    };
    let mut query = db.prepare(&format!(
        "SELECT u.track_id, u.start_ns, r.payload FROM utterances u JOIN revisions r ON r.id=u.selected_revision
         WHERE u.session_id=?1{suffix}",
    ))?;
    let rows = query.query_map(params![session, limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (track, start_ns, payload) = row?;
        let source = crate::sync_data::source(&db, audio.as_ref(), &session, &track, version)?;
        let value: UtteranceRevision = serde_json::from_str(&payload)?;
        let translation = if version >= 3 {
            translations::display(&db, &value)?
        } else {
            None
        };
        let speaker = if version >= 4 && !latest {
            crate::library::speaker(&db, &value.utterance_id)?
        } else {
            None
        };
        let conflicts = if version >= 5 && limit >= 0 && !latest {
            crate::sync_data::conflict(&db, &value.utterance_id)?
        } else {
            Vec::new()
        };
        let external_draft = if limit >= 0 && !latest && conflicts.is_empty() {
            db.query_row(
                "SELECT json_extract(payload,'$.text') FROM revisions
                 WHERE utterance_id=?1 AND revision>?2 AND json_extract(payload,'$.model.runtime_version')=?3
                 ORDER BY revision DESC LIMIT 1",
                params![String::from(value.utterance_id.clone()), value.revision.get(), crate::remote::PROTOCOL],
                |row| row.get(0),
            ).optional()?
        } else {
            None
        };
        visit(Line {
            source,
            start_ns: u64::try_from(start_ns).map_err(|_| Error::Storage)?,
            utterance_id: value.utterance_id,
            revision_id: value.id,
            language: value.language,
            text: value.text,
            translation,
            speaker,
            conflicts,
            external_draft,
        })?;
    }
    Ok(())
}

pub(crate) fn job_status(
    root: &Path,
    session: &StableId,
    worker_active: bool,
) -> Result<Option<jobs::Status>> {
    if !root.join(PATH).try_exists()? {
        return Ok(None);
    }
    let db = Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(150))?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 2 {
        return Ok(None);
    }
    Queue(&db).status(session, worker_active)
}

pub(crate) fn has_translation_source(root: &Path, session: &StableId) -> Result<bool> {
    if !root.join(PATH).try_exists()? {
        return Ok(false);
    }
    let db = Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(150))?;
    Ok(db.query_row(
        "SELECT EXISTS (
            SELECT 1 FROM utterances u JOIN revisions r ON r.id=u.selected_revision
            WHERE u.session_id=?1 AND json_extract(r.payload,'$.language')='en'
                AND json_extract(r.payload,'$.kind') IN ('final','manual')
         )",
        [String::from(session.clone())],
        |row| row.get(0),
    )?)
}

pub(crate) fn timestamp() -> Result<Count> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Invalid)?
        .as_millis();
    i64::try_from(ms)
        .ok()
        .and_then(|ms| Count::try_from(ms).ok())
        .ok_or(Error::Invalid)
}
