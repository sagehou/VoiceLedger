use rusqlite::OpenFlags;

use super::{Chunk, CommittedMedia, committed_media};
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

fn stable(id: String) -> Result<StableId> {
    id.try_into().map_err(|_| Error::Invalid)
}
