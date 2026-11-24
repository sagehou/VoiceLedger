use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use voiceledger_core::contracts::{
    Count, LanguagePolicy, ModelRef, StableId, UtteranceKind, UtteranceRevision,
};
use voiceledger_core::models::Model;

use crate::jobs::{self, Queue};
use crate::pipeline::{Final, Output, Partial};
use crate::{Error, Result};

const PATH: &str = "transcript-v1.sqlite3";
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
    db: Connection,
}

impl Transcripts {
    pub fn open(root: &Path) -> Result<Self> {
        let db = Connection::open(root.join(PATH))?;
        db.busy_timeout(Duration::from_millis(500))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if matches!(version, 0 | 1) {
            let tx = db.unchecked_transaction()?;
            if version == 0 {
                tx.execute_batch(SCHEMA)?;
            }
            tx.execute_batch(jobs::SCHEMA)?;
            tx.execute_batch("PRAGMA user_version=2;")?;
            tx.commit()?;
        } else if version != 2 {
            return Err(Error::Storage);
        }
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
        session: &StableId,
        model: &Model,
        language: LanguagePolicy,
        job: Option<&str>,
        value: Final,
    ) -> Result<bool> {
        let session = String::from(session.clone());
        let track = String::from(value.track_id.clone());
        let start = value.range.start_ns.get() as i64;
        let end = value.range.end_ns.get() as i64;
        let provisional = String::from(value.id);
        let artifact = model.artifacts.first().ok_or(Error::Invalid)?;
        let machine_key = serde_json::to_string(&(
            &model.id,
            &model.version,
            &model.runtime.version,
            &artifact.sha256,
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
            Some((id, prior)) => (
                prior.checked_add(1).ok_or(Error::Invalid)?,
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
            model: Some(ModelRef {
                model_id: model.id.clone(),
                model_version: model.version.clone(),
                runtime_version: model.runtime.version.clone(),
                sha256: artifact.sha256.clone(),
            }),
            created_at_unix_ms: timestamp()?,
        };
        let id = String::from(id);
        tx.execute(
            "INSERT INTO revisions(id,utterance_id,revision,machine_key,payload) VALUES (?1,?2,?3,?4,?5)",
            params![id, utterance, ordinal, machine_key, serde_json::to_string(&revision)?],
        )?;
        tx.execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2 AND selected_revision IS NULL",
            params![id, utterance],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

pub(crate) struct Writer<'a> {
    pub transcripts: &'a Transcripts,
    pub session: &'a StableId,
    pub model: &'a Model,
    pub language: LanguagePolicy,
    pub job: Option<&'a str>,
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
            value,
        )?);
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Line {
    pub source: String,
    pub start_ns: u64,
    pub text: String,
}

pub fn selected(root: &Path, session: &StableId) -> Result<Vec<Line>> {
    match root.join(PATH).metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(root.join(PATH), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(150))?;
    // Track labels come from the recording manifest, which stays read-only here.
    let manifest = root.join("manifest.sqlite3");
    let audio = Connection::open_with_flags(manifest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    audio.busy_timeout(Duration::from_millis(150))?;
    let session = String::from(session.clone());
    let mut query = db.prepare(
        "SELECT u.track_id, u.start_ns, r.payload FROM utterances u JOIN revisions r ON r.id=u.selected_revision
         WHERE u.session_id=?1 ORDER BY u.start_ns,u.track_id LIMIT 40",
    )?;
    let rows = query.query_map([&session], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut labels = audio.prepare("SELECT source FROM tracks WHERE id=?1 AND session_id=?2")?;
    let mut lines = Vec::new();
    for row in rows {
        let (track, start_ns, payload) = row?;
        let source = labels.query_row(params![track, session], |row| row.get(0))?;
        let value: UtteranceRevision = serde_json::from_str(&payload)?;
        lines.push(Line {
            source,
            start_ns: u64::try_from(start_ns).map_err(|_| Error::Storage)?,
            text: value.text,
        });
    }
    Ok(lines)
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

fn timestamp() -> Result<Count> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Invalid)?
        .as_millis();
    i64::try_from(ms)
        .ok()
        .and_then(|ms| Count::try_from(ms).ok())
        .ok_or(Error::Invalid)
}
