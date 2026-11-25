use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use voiceledger_audio::TrackInput;
use voiceledger_core::contracts::{
    Language, LanguagePolicy, ModelRef, Sha256, StableId, UtteranceRevision,
};
use voiceledger_core::models::Model;
use voiceledger_core::scheduler::JobKind;

use crate::{Error, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS speech_jobs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('vad','asr','translation','speaker')),
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 2),
    specification TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('queued','running','deferred','completed','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    reason TEXT,
    cursor_track TEXT,
    cursor_ns INTEGER CHECK(cursor_ns >= 0),
    CHECK ((cursor_track IS NULL) = (cursor_ns IS NULL)),
    UNIQUE(session_id,kind,specification)
) STRICT;
CREATE INDEX IF NOT EXISTS speech_jobs_pending ON speech_jobs(state,priority);";

#[derive(Serialize)]
pub(crate) struct SourceJob<'a> {
    tracks: &'a [TrackInput],
    #[serde(skip_serializing_if = "Option::is_none")]
    live_track: Option<&'a StableId>,
    model_id: &'a str,
    model_version: &'a str,
    runtime_version: &'a str,
    weights: Vec<&'a Sha256>,
    language: LanguagePolicy,
}

impl<'a> SourceJob<'a> {
    pub fn new(tracks: &'a [TrackInput], model: &'a Model, language: LanguagePolicy) -> Self {
        Self {
            tracks,
            live_track: None,
            model_id: &model.id,
            model_version: &model.version,
            runtime_version: &model.runtime.version,
            weights: model
                .artifacts
                .iter()
                .map(|artifact| &artifact.sha256)
                .collect(),
            language,
        }
    }

    pub fn live(track: &'a StableId, model: &'a Model, language: LanguagePolicy) -> Self {
        Self {
            live_track: Some(track),
            ..Self::new(&[], model, language)
        }
    }
}

pub(crate) enum Outcome {
    Deferred(&'static str),
    Completed,
    Failed(&'static str),
}

impl Outcome {
    pub(crate) fn for_result<T>(result: &Result<T>) -> Self {
        match result {
            Ok(_) => Self::Completed,
            Err(Error::Cancelled) => Self::Deferred("user_cancelled"),
            Err(Error::Deferred(reason)) => Self::Deferred(reason.code()),
            Err(Error::Blocked(_)) => Self::Deferred("resource_pressure"),
            Err(Error::NotInstalled) => Self::Failed("not_installed"),
            Err(Error::Integrity) => Self::Failed("model_integrity"),
            Err(Error::Runtime) => Self::Failed("runtime_unavailable"),
            Err(Error::TranslationTextLimit) => Self::Failed("text_limit"),
            Err(_) => Self::Failed("job_failed"),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct Status {
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub track: StableId,
    pub next_ns: i64,
}

pub(crate) struct Queue<'a>(pub &'a Connection);

impl Queue<'_> {
    pub fn recover(&self) -> Result<()> {
        self.0.execute(
            "UPDATE speech_jobs SET state='deferred',reason='worker_interrupted' WHERE state='running'",
            [],
        )?;
        Ok(())
    }

    pub fn enqueue(
        &self,
        id: StableId,
        session: &StableId,
        kind: JobKind,
        spec: &impl Serialize,
    ) -> Result<String> {
        let session = String::from(session.clone());
        let id = String::from(id);
        let spec = serde_json::to_string(spec)?;
        self.0.execute(
            "INSERT INTO speech_jobs(id,session_id,kind,priority,specification,state)
             VALUES (?1,?2,?3,?4,?5,'queued') ON CONFLICT(session_id,kind,specification) DO NOTHING",
            params![id, session, kind.name(), kind.priority(), spec],
        )?;
        Ok(self.0.query_row(
            "SELECT id FROM speech_jobs WHERE session_id=?1 AND kind=?2 AND specification=?3",
            params![session, kind.name(), spec],
            |row| row.get(0),
        )?)
    }

    pub fn live_exists(&self, session: &StableId, spec: &SourceJob<'_>) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM speech_jobs WHERE session_id=?1 AND kind='asr' AND specification=?2)",
            params![String::from(session.clone()), serde_json::to_string(spec)?],
            |row| row.get(0),
        )?)
    }

    pub fn next_translation(
        &self,
        session: &StableId,
        model: &ModelRef,
    ) -> Result<Option<(String, UtteranceRevision)>> {
        let row = self
            .0
            .query_row(
                "SELECT j.id,r.payload FROM speech_jobs j
             JOIN revisions r ON r.id=json_extract(j.specification,'$[0]')
             JOIN utterances u ON u.selected_revision=r.id AND u.session_id=j.session_id
             WHERE j.session_id=?1 AND j.kind='translation' AND j.state IN ('queued','deferred')
               AND json_extract(j.specification,'$[1]')=?2
               AND json_extract(j.specification,'$[2]')=?3
             ORDER BY j.rowid LIMIT 1",
                params![
                    String::from(session.clone()),
                    Language::Zh.code(),
                    serde_json::to_string(model)?
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(id, payload)| Ok((id, serde_json::from_str(&payload)?)))
            .transpose()
    }

    pub fn translation_pending(&self, session: &StableId) -> Result<bool> {
        Ok(self.0.query_row(
            "SELECT EXISTS(SELECT 1 FROM speech_jobs j
                JOIN utterances u ON u.session_id=j.session_id AND u.selected_revision=json_extract(j.specification,'$[0]')
                WHERE j.session_id=?1 AND j.kind='translation' AND j.state IN ('queued','running','deferred','failed'))",
            [String::from(session.clone())], |row| row.get(0),
        )?)
    }

    pub fn begin(&self, ids: &[&str]) -> Result<bool> {
        let tx = self.0.unchecked_transaction()?;
        let mut claimed = false;
        for id in ids {
            claimed |= tx.execute(
                "UPDATE speech_jobs SET state='running',attempts=attempts+1,reason=NULL
                 WHERE id=?1 AND state IN ('queued','deferred','failed')",
                [id],
            )? != 0;
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn finish(&self, ids: &[&str], outcome: Outcome) -> Result<()> {
        let (state, reason) = match outcome {
            Outcome::Completed => ("completed", None),
            Outcome::Deferred(reason) => ("deferred", Some(reason)),
            Outcome::Failed(reason) => ("failed", Some(reason)),
        };
        let tx = self.0.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE speech_jobs SET state=?2,reason=?3 WHERE id=?1 AND state='running'",
                params![id, state, reason],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn backlog(&self) -> Result<u32> {
        Ok(self.0.query_row(
            "SELECT count(*) FROM speech_jobs WHERE state IN ('queued','deferred')",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn higher_priority_backlog(&self, kind: JobKind) -> Result<u32> {
        Ok(self.0.query_row(
            "SELECT count(*) FROM speech_jobs WHERE state IN ('queued','deferred') AND priority<?1",
            [kind.priority()],
            |row| row.get(0),
        )?)
    }

    pub fn cursor(&self, id: &str) -> Result<Option<Cursor>> {
        let pair: (Option<String>, Option<i64>) = self.0.query_row(
            "SELECT cursor_track,cursor_ns FROM speech_jobs WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        pair.0
            .zip(pair.1)
            .map(|(track, next_ns)| {
                Ok(Cursor {
                    track: track.try_into().map_err(|_| Error::Storage)?,
                    next_ns,
                })
            })
            .transpose()
    }

    pub fn checkpoint(&self, id: &str, track: &StableId, next_ns: i64) -> Result<()> {
        self.0.execute(
            "UPDATE speech_jobs SET cursor_track=?2,cursor_ns=?3 WHERE id=?1 AND state='running'
             AND (cursor_track IS NULL OR (cursor_track,cursor_ns)<(?2,?3))",
            params![id, String::from(track.clone()), next_ns],
        )?;
        Ok(())
    }

    pub fn status(&self, session: &StableId, worker_active: bool) -> Result<Option<Status>> {
        let mut status: Option<Status> = self.0.query_row(
            "SELECT state,reason FROM speech_jobs WHERE session_id=?1 AND kind='asr' ORDER BY rowid DESC LIMIT 1",
            [String::from(session.clone())],
            |row| Ok(Status { state: row.get(0)?, reason: row.get(1)? }),
        ).optional()?;
        if let Some(job) = &mut status
            && job.state == "running"
            && !worker_active
        {
            job.state = "deferred".into();
            job.reason = Some("worker_interrupted".into());
        }
        Ok(status)
    }
}
