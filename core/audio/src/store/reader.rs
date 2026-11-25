use rusqlite::OpenFlags;

use super::{
    Chunk, CommittedMedia, HistoryTrack, Response, TimelineRow, committed_media, playback,
};
use crate::{Error, Result};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use voiceledger_core::contracts::StableId;

#[derive(Serialize)]
pub struct FinishedSession {
    pub id: StableId,
    pub created_at_ms: i64,
}

#[derive(Serialize)]
pub struct SavedSession {
    pub id: StableId,
    pub created_at_ms: i64,
    pub elapsed_ns: i64,
    pub interrupted: bool,
    pub tracks: u32,
    pub committed: u32,
    pub missing: u32,
}

pub struct CommittedChunk {
    pub id: StableId,
    pub track_id: StableId,
    pub epoch: u32,
    pub start_ns: i64,
    pub end_ns: i64,
    pub channels: usize,
    pub samples: Option<Vec<f32>>,
}

#[derive(Debug, Serialize)]
pub struct TrackInput {
    pub track_id: StableId,
    pub first_chunk_id: StableId,
    pub last_chunk_id: StableId,
    pub start_ns: i64,
    pub end_ns: i64,
}

pub struct CommittedReader {
    root: PathBuf,
    db: Connection,
}

impl CommittedReader {
    pub fn open(root: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(
            root.join("manifest.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.busy_timeout(Duration::from_millis(150))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 1 {
            return Err(Error::UnsupportedSchema);
        }
        Ok(Self {
            root: root.join("audio"),
            db,
        })
    }

    pub fn latest(&self) -> Result<Option<FinishedSession>> {
        let row = self
            .db
            .query_row(
                "SELECT id,created_at_ms FROM sessions ses WHERE state IN (5,6)
             AND EXISTS(SELECT 1 FROM tracks t JOIN chunks c ON c.track_id=t.id
                        WHERE t.session_id=ses.id AND c.state='committed')
             ORDER BY created_at_ms DESC,id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(id, created_at_ms)| {
            Ok(FinishedSession {
                id: stable(id)?,
                created_at_ms,
            })
        })
        .transpose()
    }

    pub fn finished(&self, session: &StableId) -> Result<Option<FinishedSession>> {
        let id = String::from(session.clone());
        let created_at_ms = self
            .db
            .query_row(
                "SELECT created_at_ms FROM sessions WHERE id=?1 AND state IN (5,6)",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(created_at_ms.map(|created_at_ms| FinishedSession {
            id: session.clone(),
            created_at_ms,
        }))
    }

    pub fn saved_sessions(&self, before: Option<(i64, &StableId)>) -> Result<Vec<SavedSession>> {
        let before = before.map(|(at, id)| (at, String::from(id.clone())));
        let (at, id) = before.map_or((None, None), |(at, id)| (Some(at), Some(id)));
        let mut query = self.db.prepare(
            "WITH page AS (
                SELECT id,created_at_ms,elapsed_ns,state,interrupted_tail FROM sessions
                WHERE state IN (5,6) AND (?1 IS NULL OR (created_at_ms,id)<(?1,?2))
                ORDER BY created_at_ms DESC,id DESC LIMIT 64
            )
            SELECT p.id,p.created_at_ms,p.elapsed_ns,(p.state=6 OR p.interrupted_tail=1),
                COUNT(DISTINCT t.id),COUNT(CASE WHEN c.state='committed' THEN 1 END),
                COUNT(CASE WHEN c.state='missing' THEN 1 END)
            FROM page p LEFT JOIN tracks t ON t.session_id=p.id LEFT JOIN chunks c ON c.track_id=t.id
            GROUP BY p.id ORDER BY p.created_at_ms DESC,p.id DESC",
        )?;
        let rows = query.query_map(params![at, id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        rows.map(|row| {
            let (id, created_at_ms, elapsed_ns, interrupted, tracks, committed, missing) = row?;
            Ok(SavedSession {
                id: stable(id)?,
                created_at_ms,
                elapsed_ns,
                interrupted,
                tracks,
                committed,
                missing,
            })
        })
        .collect()
    }

    pub fn tracks(&self, session: &StableId) -> Result<Vec<HistoryTrack>> {
        history(&self.db, Some(String::from(session.clone()).as_str()))
    }

    pub fn timeline(&self, session: &StableId, track: &StableId) -> Result<Vec<TimelineRow>> {
        let track = String::from(track.clone());
        let available: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM tracks t JOIN sessions s ON s.id=t.session_id
             WHERE t.id=?1 AND s.id=?2 AND s.state IN (5,6))",
            params![track, String::from(session.clone())],
            |row| row.get(0),
        )?;
        if !available {
            return Err(Error::Unavailable);
        }
        timeline(&self.db, &track)
    }

    pub fn playback(&self, session: &StableId, track: &StableId, at: i64) -> Result<Response> {
        if at < 0 {
            return Err(Error::Invalid);
        }
        let chunk = self
            .db
            .query_row(
                &format!(
                    "{} JOIN sessions ses ON ses.id=t.session_id
             WHERE ses.id=?1 AND ses.state IN (5,6) AND c.track_id=?2 AND c.state='committed'
               AND c.start_ns<=?3 AND c.end_ns>?3",
                    Chunk::SELECT
                ),
                params![
                    String::from(session.clone()),
                    String::from(track.clone()),
                    at
                ],
                Chunk::read,
            )
            .optional()?
            .ok_or(Error::Unavailable)?;
        match committed_media(&self.root, &chunk)? {
            CommittedMedia::Decoded(media) => Ok(playback(&chunk, at, media)),
            CommittedMedia::Absent | CommittedMedia::Corrupt => Err(Error::Unavailable),
        }
    }

    pub fn recording(&self) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE state IN (1,2,3,4))",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn inputs(&self, session: &StableId) -> Result<Vec<TrackInput>> {
        let mut query = self.db.prepare(
            "WITH indexed AS (
                SELECT c.id,c.track_id,c.start_ns,c.end_ns FROM chunks c
                JOIN tracks t ON t.id=c.track_id JOIN sessions s ON s.id=t.session_id
                WHERE s.id=?1 AND s.state IN (5,6) AND c.state IN ('committed','missing')
                AND c.sha256 IS NOT NULL
            )
            SELECT track_id,
                (SELECT id FROM indexed WHERE track_id=i.track_id ORDER BY start_ns LIMIT 1),
                (SELECT id FROM indexed WHERE track_id=i.track_id ORDER BY start_ns DESC LIMIT 1),
                MIN(start_ns), MAX(end_ns)
            FROM indexed i GROUP BY track_id ORDER BY track_id",
        )?;
        let rows = query.query_map([String::from(session.clone())], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.map(|row| {
            let (track, first, last, start_ns, end_ns) = row?;
            Ok(TrackInput {
                track_id: stable(track)?,
                first_chunk_id: stable(first)?,
                last_chunk_id: stable(last)?,
                start_ns,
                end_ns,
            })
        })
        .collect()
    }

    pub fn input_chunk(
        &self,
        session: &StableId,
        input: &TrackInput,
        from_ns: i64,
    ) -> Result<Option<CommittedChunk>> {
        let row = self
            .db
            .query_row(
                &format!(
                    "{} JOIN sessions ses ON ses.id=t.session_id
                 WHERE ses.id=?1 AND ses.state IN (5,6) AND c.state='committed'
                 AND c.track_id=?2 AND c.start_ns>=?3 AND c.end_ns<=?4 AND c.end_ns>?5
                 ORDER BY c.start_ns LIMIT 1",
                    Chunk::SELECT,
                ),
                params![
                    String::from(session.clone()),
                    String::from(input.track_id.clone()),
                    input.start_ns,
                    input.end_ns,
                    from_ns
                ],
                Chunk::read,
            )
            .optional()?;
        row.map(|chunk| self.decode_chunk(chunk)).transpose()
    }

    pub fn next_chunk(
        &self,
        session: &StableId,
        after: Option<(&StableId, i64)>,
    ) -> Result<Option<CommittedChunk>> {
        let session = String::from(session.clone());
        let (track, start_ns) = after.map_or_else(
            || (String::new(), -1),
            |(track, start)| (track.clone().into(), start),
        );
        let row = self
            .db
            .query_row(
                &format!(
                    "{} JOIN sessions ses ON ses.id=t.session_id
                 WHERE ses.id=?1 AND ses.state IN (5,6) AND c.state='committed'
                 AND (c.track_id,c.start_ns)>(?2,?3) ORDER BY c.track_id,c.start_ns LIMIT 1",
                    Chunk::SELECT,
                ),
                params![session, track, start_ns],
                Chunk::read,
            )
            .optional()?;
        row.map(|chunk| self.decode_chunk(chunk)).transpose()
    }

    fn decode_chunk(&self, chunk: Chunk) -> Result<CommittedChunk> {
        let samples = match committed_media(&self.root, &chunk)? {
            CommittedMedia::Decoded(media) => Some(media.samples),
            CommittedMedia::Absent | CommittedMedia::Corrupt => None,
        };
        Ok(CommittedChunk {
            id: stable(chunk.id)?,
            track_id: stable(chunk.track_id)?,
            epoch: chunk.epoch,
            start_ns: chunk.start_ns,
            end_ns: chunk.end_ns,
            channels: chunk.channels,
            samples,
        })
    }
}

pub(super) fn history(db: &Connection, session: Option<&str>) -> Result<Vec<HistoryTrack>> {
    let mut statement = db.prepare(
        "SELECT t.id,s.id,t.source,s.created_at_ms,s.interrupted_tail,
          COALESCE(MIN(c.start_ns),0),COALESCE(MAX(c.end_ns),0),COUNT(c.id),
          (SELECT COUNT(*) FROM audio_timeline a WHERE a.track_id=t.id AND a.kind='gap')
         FROM tracks t JOIN sessions s ON s.id=t.session_id
         LEFT JOIN chunks c ON c.track_id=t.id AND c.state='committed'
         WHERE s.state IN (5,6) AND (?1 IS NULL OR s.id=?1)
         GROUP BY t.id ORDER BY s.created_at_ms DESC,t.source",
    )?;
    let rows = statement.query_map([session], |row| {
        Ok(HistoryTrack {
            id: row.get(0)?,
            session_id: row.get(1)?,
            source: row.get(2)?,
            created_at_ms: row.get(3)?,
            interrupted_tail: row.get(4)?,
            first_ns: row.get(5)?,
            last_ns: row.get(6)?,
            committed_chunks: row.get(7)?,
            loss_intervals: row.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(super) fn timeline(db: &Connection, track: &str) -> Result<Vec<TimelineRow>> {
    let mut statement = db.prepare(
        "SELECT source_epoch,start_ns,end_ns,kind,cause FROM audio_timeline
         WHERE track_id=?1 ORDER BY start_ns,end_ns,kind",
    )?;
    let rows = statement.query_map([track], |row| {
        Ok(TimelineRow {
            source_epoch: row.get(0)?,
            start_ns: row.get(1)?,
            end_ns: row.get(2)?,
            kind: row.get(3)?,
            cause: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn stable(id: String) -> Result<StableId> {
    id.try_into().map_err(|_| Error::Invalid)
}
