use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use voiceledger_core::contracts::{Source, StableId};

use crate::{Error, MAX_CHANNELS, MAX_CHUNK_BYTES, RATE, Result, opus};

#[cfg(test)]
mod tests;

const MAX_DURATION_NS: i64 = 10_000_000_000;
const NS: u64 = 1_000_000_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTrack {
    pub id: StableId,
    pub source: Source,
    pub endpoint_id: String,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    CreateSession {
        id: StableId,
        created_at_ms: i64,
        clock: String,
        clock_origin: i64,
        tracks: Vec<NewTrack>,
    },
    SessionState {
        id: StableId,
        state: i32,
        elapsed_ns: i64,
        snapshot: serde_json::Value,
    },
    BeginStream {
        track_id: StableId,
        epoch: u32,
        start_ns: i64,
        rate: u32,
        channels: u32,
        sample_format: String,
        native_format: String,
    },
    EndStream {
        track_id: StableId,
        epoch: u32,
        end_ns: i64,
        reason: String,
    },
    BeginChunk {
        id: StableId,
        track_id: StableId,
        epoch: u32,
        start_ns: i64,
        end_ns: i64,
    },
    Checkpoint {
        id: StableId,
        end_ns: i64,
    },
    SealChunk {
        id: StableId,
        end_ns: i64,
        packets: Vec<PacketMetadata>,
    },
    Interval {
        track_id: StableId,
        epoch: u32,
        start_ns: i64,
        end_ns: i64,
        kind: IntervalKind,
        cause: Option<String>,
    },
    History,
    Recovery,
    Timeline {
        track_id: StableId,
    },
    Seek {
        track_id: StableId,
        position_ns: i64,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PacketMetadata {
    pub start_ns: i64,
    pub end_ns: i64,
    pub frames: u32,
    pub device_position_frames: i64,
    pub qpc100ns: i64,
    pub flags: i32,
    pub estimated_timestamp: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntervalKind {
    Gap,
    Silence,
}

pub enum Response {
    Empty,
    Json(Vec<u8>),
    Playback {
        channels: u32,
        next_ns: i64,
        samples: Vec<f32>,
    },
}

impl Response {
    fn json(value: impl Serialize) -> Result<Self> {
        Ok(Self::Json(serde_json::to_vec(&value)?))
    }
}

#[derive(Default, Serialize)]
struct Recovery {
    restored_chunks: u32,
    missing_chunks: u32,
    corrupt_chunks: u32,
    uncommitted_chunks: u32,
    quarantined_files: u32,
    interrupted_sessions: u32,
    known_tail_loss_ns: u64,
    elapsed_ms: u64,
}

#[derive(Serialize)]
struct HistoryTrack {
    id: String,
    session_id: String,
    source: String,
    created_at_ms: i64,
    interrupted_tail: bool,
    first_ns: i64,
    last_ns: i64,
    committed_chunks: i64,
    loss_intervals: i64,
}

#[derive(Serialize)]
struct TimelineRow {
    source_epoch: u32,
    start_ns: i64,
    end_ns: i64,
    kind: String,
    cause: Option<String>,
}

struct Chunk {
    id: String,
    track_id: String,
    session_id: String,
    start_ns: i64,
    end_ns: i64,
    channels: usize,
    rate: u32,
    frames: Option<usize>,
    state: String,
    sha256: Option<String>,
    byte_size: Option<u64>,
}

impl Chunk {
    const SELECT: &str =
        "SELECT c.id,c.track_id,t.session_id,c.start_ns,c.end_ns,s.channels,s.rate,
        c.frames,c.state,c.sha256,c.byte_size FROM chunks c JOIN tracks t ON t.id=c.track_id
        JOIN streams s ON s.id=c.stream_id";

    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            track_id: row.get(1)?,
            session_id: row.get(2)?,
            start_ns: row.get(3)?,
            end_ns: row.get(4)?,
            channels: row.get::<_, u32>(5)? as usize,
            rate: row.get(6)?,
            frames: row.get::<_, Option<u32>>(7)?.map(|frames| frames as usize),
            state: row.get(8)?,
            sha256: row.get(9)?,
            byte_size: row.get::<_, Option<i64>>(10)?.map(|bytes| bytes as u64),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Boundary {
    TempCreated,
    Encoded,
    Flushed,
    Prepared,
    Renamed,
    BeforeCommit,
    Committed,
}

enum Payload {
    Absent,
    Corrupt,
    Valid(Vec<u8>),
}

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
    recovery: Recovery,
    _lock: File,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        let started = Instant::now();
        fs::create_dir_all(root)?;
        let lock = OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .truncate(false)
            .open(root.join("manifest.lock"))?;
        lock.try_lock().map_err(|_| Error::Locked)?;
        let db = Connection::open(root.join("manifest.sqlite3"))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        match version {
            0 => {
                let tx = db.unchecked_transaction()?;
                tx.execute_batch(include_str!("manifest.sql"))?;
                tx.execute_batch("PRAGMA user_version=1;")?;
                tx.commit()?;
            }
            1 => {}
            _ => return Err(Error::UnsupportedSchema),
        }
        let mut store = Self {
            root: root.join("audio"),
            db: Mutex::new(db),
            recovery: Recovery::default(),
            _lock: lock,
        };
        store.recovery = store.reconcile()?;
        store.recovery.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(store)
    }

    pub fn execute_json(&self, command: &[u8], pcm: &[f32]) -> Result<Response> {
        self.execute(serde_json::from_slice(command)?, pcm)
    }

    pub fn execute(&self, command: Command, pcm: &[f32]) -> Result<Response> {
        if !pcm.is_empty() && !matches!(&command, Command::SealChunk { .. }) {
            return Err(Error::Invalid);
        }
        match command {
            Command::CreateSession {
                id,
                created_at_ms,
                clock,
                clock_origin,
                tracks,
            } => {
                if !(1..=2).contains(&tracks.len()) || clock.is_empty() {
                    return Err(Error::Invalid);
                }
                let id = String::from(id);
                let db = self.connection()?;
                let tx = db.unchecked_transaction()?;
                tx.execute(
                    "INSERT INTO sessions(id,created_at_ms,clock,clock_origin,state)
                    VALUES (?1,?2,?3,?4,1)",
                    params![id, created_at_ms, clock, clock_origin],
                )?;
                for track in tracks {
                    let source = match track.source {
                        Source::Mic => "mic",
                        Source::SystemLoopback => "system_loopback",
                    };
                    if track.endpoint_id.is_empty() {
                        return Err(Error::Invalid);
                    }
                    tx.execute(
                        "INSERT INTO tracks(id,session_id,source,endpoint_id) VALUES (?1,?2,?3,?4)",
                        params![String::from(track.id), id, source, track.endpoint_id],
                    )?;
                }
                tx.commit()?;
            }
            Command::SessionState {
                id,
                state,
                elapsed_ns,
                snapshot,
            } => {
                changed(self.connection()?.execute(
                    "UPDATE sessions SET state=?2,elapsed_ns=?3,snapshot=?4 WHERE id=?1 AND elapsed_ns<=?3",
                    params![String::from(id), state, elapsed_ns, serde_json::to_string(&snapshot)?],
                )?)?;
            }
            Command::BeginStream {
                track_id,
                epoch,
                start_ns,
                rate,
                channels,
                sample_format,
                native_format,
            } => {
                self.connection()?.execute(
                    "INSERT INTO streams(track_id,source_epoch,start_ns,rate,channels,sample_format,native_format)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![String::from(track_id), epoch, start_ns, rate, channels, sample_format, native_format],
                )?;
            }
            Command::EndStream {
                track_id,
                epoch,
                end_ns,
                reason,
            } => {
                changed(self.connection()?.execute(
                    "UPDATE streams SET end_ns=?3,reason=?4 WHERE track_id=?1 AND source_epoch=?2 AND end_ns IS NULL",
                    params![String::from(track_id), epoch, end_ns, reason],
                )?)?;
            }
            Command::BeginChunk {
                id,
                track_id,
                epoch,
                start_ns,
                end_ns,
            } => {
                changed(self.connection()?.execute(
                    "INSERT INTO chunks(id,track_id,stream_id,sequence,start_ns,end_ns,state)
                     SELECT ?1,s.track_id,s.id,COALESCE((SELECT MAX(sequence)+1 FROM chunks WHERE track_id=?2),0),?4,?5,'pending'
                     FROM streams s WHERE s.track_id=?2 AND s.source_epoch=?3 AND s.end_ns IS NULL AND s.start_ns<=?4
                     AND NOT EXISTS(SELECT 1 FROM audio_timeline WHERE track_id=?2 AND end_ns>?4)",
                    params![String::from(id), String::from(track_id), epoch, start_ns, end_ns],
                )?)?;
            }
            Command::Checkpoint { id, end_ns } => self.checkpoint(&String::from(id), end_ns)?,
            Command::SealChunk {
                id,
                end_ns,
                packets,
            } => {
                self.seal(&String::from(id), end_ns, &packets, pcm, |_| {})?;
            }
            Command::Interval {
                track_id,
                epoch,
                start_ns,
                end_ns,
                kind,
                cause,
            } => {
                let kind = match kind {
                    IntervalKind::Gap => "gap",
                    IntervalKind::Silence => "silence",
                };
                self.connection()?.execute(
                    "INSERT INTO intervals(track_id,source_epoch,start_ns,end_ns,kind,cause)
                     VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT DO NOTHING",
                    params![String::from(track_id), epoch, start_ns, end_ns, kind, cause],
                )?;
            }
            Command::History => return Response::json(self.history()?),
            Command::Recovery => return Response::json(&self.recovery),
            Command::Timeline { track_id } => {
                return Response::json(self.timeline(&String::from(track_id))?);
            }
            Command::Seek {
                track_id,
                position_ns,
            } => return self.seek(&String::from(track_id), position_ns),
        }
        Ok(Response::Empty)
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db.lock().map_err(|_| Error::Internal)
    }

    fn chunk(&self, id: &str) -> Result<Chunk> {
        self.connection()?
            .query_row(
                &format!("{} WHERE c.id=?1", Chunk::SELECT),
                [id],
                Chunk::read,
            )
            .optional()?
            .ok_or(Error::Invalid)
    }

    fn path(&self, chunk: &Chunk, extension: &str) -> Result<PathBuf> {
        for id in [&chunk.id, &chunk.track_id, &chunk.session_id] {
            StableId::try_from(id.clone()).map_err(|_| Error::Invalid)?;
        }
        Ok(self
            .root
            .join(&chunk.session_id)
            .join(&chunk.track_id)
            .join(format!("{}.{extension}", chunk.id)))
    }

    fn checkpoint(&self, id: &str, end_ns: i64) -> Result<()> {
        changed(self.connection()?.execute(
            "UPDATE chunks SET end_ns=?2 WHERE id=?1 AND state='pending' AND sha256 IS NULL AND end_ns<=?2",
            params![id, end_ns],
        )?)
    }

    fn seal(
        &self,
        id: &str,
        end_ns: i64,
        packets: &[PacketMetadata],
        input: &[f32],
        mut boundary: impl FnMut(Boundary),
    ) -> Result<()> {
        let chunk = self.chunk(id)?;
        if chunk.state != "pending"
            || chunk.sha256.is_some()
            || end_ns <= chunk.start_ns
            || end_ns - chunk.start_ns > MAX_DURATION_NS
            || packets.is_empty()
            || packets.len() > 512
        {
            return Err(Error::Invalid);
        }
        let mut cursor = chunk.start_ns;
        let mut source_frames = 0u64;
        for packet in packets {
            if packet.start_ns != cursor || packet.end_ns <= cursor || packet.frames == 0 {
                return Err(Error::Invalid);
            }
            cursor = packet.end_ns;
            source_frames += u64::from(packet.frames);
        }
        let expected = (source_frames * RATE as u64).div_ceil(u64::from(chunk.rate));
        if cursor != end_ns
            || chunk.channels > MAX_CHANNELS
            || input.len() as u64 != expected * chunk.channels as u64
        {
            return Err(Error::Invalid);
        }
        self.checkpoint(id, end_ns)?;
        let final_path = self.path(&chunk, "opus")?;
        let temporary = self.path(&chunk, "opus.partial")?;
        fs::create_dir_all(final_path.parent().ok_or(Error::Invalid)?)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        boundary(Boundary::TempCreated);
        let frames = opus::encode(&mut file, input, chunk.channels, chunk.rate)?;
        boundary(Boundary::Encoded);
        file.rewind()?;
        let bytes = read_bounded(&mut file)?;
        let media = opus::decode(Cursor::new(&bytes))?;
        if media.channels != chunk.channels
            || media.source_rate != chunk.rate
            || media.samples.len() != frames * chunk.channels
        {
            return Err(Error::InvalidMedia);
        }
        let digest = digest(&bytes);
        file.sync_all()?;
        drop(file);
        boundary(Boundary::Flushed);
        changed(self.connection()?.execute(
            "UPDATE chunks SET frames=?2,byte_size=?3,sha256=?4,packets_json=?5
             WHERE id=?1 AND state='pending' AND sha256 IS NULL",
            params![
                id,
                frames as i64,
                bytes.len() as i64,
                digest,
                serde_json::to_string(packets)?
            ],
        )?)?;
        boundary(Boundary::Prepared);
        install(&temporary, &final_path)?;
        boundary(Boundary::Renamed);
        self.publish(id, &mut boundary)
    }

    fn publish(&self, id: &str, boundary: &mut impl FnMut(Boundary)) -> Result<()> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        changed(tx.execute(
            "UPDATE chunks SET state='committed',problem=NULL WHERE id=?1 AND state='pending' AND sha256 IS NOT NULL",
            [id],
        )?)?;
        tx.execute("INSERT INTO processing_jobs(chunk_id,state) VALUES (?1,'pending') ON CONFLICT DO NOTHING", [id])?;
        boundary(Boundary::BeforeCommit);
        tx.commit()?;
        boundary(Boundary::Committed);
        Ok(())
    }

    fn mark_missing(&self, id: &str, reason: &str) -> Result<()> {
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "UPDATE chunks SET state='missing',problem=?2 WHERE id=?1",
            params![id, reason],
        )?;
        tx.execute(
            "UPDATE processing_jobs SET state='unavailable' WHERE chunk_id=?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn history(&self) -> Result<Vec<HistoryTrack>> {
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT t.id,s.id,t.source,s.created_at_ms,s.interrupted_tail,
              COALESCE(MIN(c.start_ns),0),COALESCE(MAX(c.end_ns),0),COUNT(c.id),
              (SELECT COUNT(*) FROM audio_timeline a WHERE a.track_id=t.id AND a.kind='gap')
             FROM tracks t JOIN sessions s ON s.id=t.session_id
             LEFT JOIN chunks c ON c.track_id=t.id AND c.state='committed'
             WHERE s.state IN (5,6) GROUP BY t.id ORDER BY s.created_at_ms DESC,t.source LIMIT 200",
        )?;
        let rows = statement.query_map([], |row| {
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

    fn timeline(&self, track: &str) -> Result<Vec<TimelineRow>> {
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT source_epoch,start_ns,end_ns,kind,cause FROM audio_timeline
             WHERE track_id=?1 ORDER BY start_ns LIMIT 10000",
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

    fn seek(&self, track_id: &str, at: i64) -> Result<Response> {
        let chunk = self.connection()?.query_row(
            &format!("{} WHERE c.track_id=?1 AND c.state='committed' AND c.start_ns<=?2 AND c.end_ns>?2",
                Chunk::SELECT),
            params![track_id, at], Chunk::read,
        ).optional()?.ok_or(Error::Unavailable)?;
        let bytes = match inspect(&self.path(&chunk, "opus")?, &chunk)? {
            Payload::Absent => {
                self.mark_missing(&chunk.id, "missing_file")?;
                return Err(Error::Unavailable);
            }
            Payload::Corrupt => {
                self.mark_missing(&chunk.id, "corrupt_file")?;
                return Err(Error::Unavailable);
            }
            Payload::Valid(bytes) => bytes,
        };
        let media = match opus::decode(Cursor::new(bytes)) {
            Ok(media)
                if media.channels == chunk.channels
                    && media.source_rate == chunk.rate
                    && Some(media.samples.len() / media.channels) == chunk.frames =>
            {
                media
            }
            _ => {
                self.mark_missing(&chunk.id, "corrupt_file")?;
                return Err(Error::Unavailable);
            }
        };
        let frame = ((at - chunk.start_ns) as u64 * RATE as u64 / NS) as usize;
        let mut samples = media.samples;
        let offset = (frame * media.channels).min(samples.len());
        samples.drain(..offset);
        Ok(Response::Playback {
            channels: media.channels as u32,
            next_ns: chunk.end_ns,
            samples,
        })
    }

    fn reconcile(&self) -> Result<Recovery> {
        let mut report = Recovery::default();
        let chunks = {
            let db = self.connection()?;
            let mut statement = db.prepare(&format!(
                "{} WHERE c.state IN ('committed','pending')",
                Chunk::SELECT
            ))?;
            statement
                .query_map([], Chunk::read)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut tracked = HashSet::new();
        for chunk in chunks {
            let final_path = self.path(&chunk, "opus")?;
            let partial = self.path(&chunk, "opus.partial")?;
            if chunk.state == "pending" && chunk.sha256.is_none() {
                self.mark_missing(&chunk.id, "uncommitted_tail")?;
                report.known_tail_loss_ns += (chunk.end_ns - chunk.start_ns) as u64;
                report.uncommitted_chunks += 1;
                continue;
            }
            let final_payload = inspect(&final_path, &chunk)?;
            match final_payload {
                Payload::Valid(_) => {
                    if chunk.state == "pending" {
                        self.publish(&chunk.id, &mut |_| {})?;
                        report.restored_chunks += 1;
                    }
                    tracked.insert(final_path);
                }
                Payload::Absent
                    if chunk.state == "pending"
                        && matches!(inspect(&partial, &chunk)?, Payload::Valid(_)) =>
                {
                    install(&partial, &final_path)?;
                    self.publish(&chunk.id, &mut |_| {})?;
                    report.restored_chunks += 1;
                    tracked.insert(final_path);
                }
                Payload::Absent => {
                    let problem = if partial.try_exists()? {
                        "corrupt_file"
                    } else {
                        "missing_file"
                    };
                    self.mark_missing(&chunk.id, problem)?;
                    if problem == "corrupt_file" {
                        report.corrupt_chunks += 1;
                    } else {
                        report.missing_chunks += 1;
                    }
                }
                Payload::Corrupt => {
                    self.mark_missing(&chunk.id, "corrupt_file")?;
                    report.corrupt_chunks += 1;
                }
            }
        }
        report.quarantined_files = self.quarantine_unindexed(&tracked)?;
        let db = self.connection()?;
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "UPDATE streams SET reason='process_death',end_ns=MAX(start_ns,
              COALESCE((SELECT MAX(end_ns) FROM chunks WHERE stream_id=streams.id),0),
              COALESCE((SELECT MAX(end_ns) FROM intervals WHERE track_id=streams.track_id),0))
             WHERE end_ns IS NULL",
            [],
        )?;
        report.interrupted_sessions = tx.execute(
            "UPDATE sessions SET state=6,interrupted_tail=1,elapsed_ns=MAX(elapsed_ns,COALESCE((
               SELECT MAX(s.end_ns) FROM streams s JOIN tracks t ON t.id=s.track_id WHERE t.session_id=sessions.id
             ),0)) WHERE state IN (1,2,3,4)", [],
        )? as u32;
        tx.commit()?;
        Ok(report)
    }

    fn quarantine_unindexed(&self, expected: &HashSet<PathBuf>) -> Result<u32> {
        if !self.root.try_exists()? {
            return Ok(0);
        }
        let mut count = 0;
        for session in fs::read_dir(&self.root)? {
            let session = session?;
            if !session.file_type()?.is_dir() {
                continue;
            }
            for track in fs::read_dir(session.path())? {
                let track = track?;
                if !track.file_type()?.is_dir() {
                    continue;
                }
                for entry in fs::read_dir(track.path())? {
                    let entry = entry?;
                    let path = entry.path();
                    if !entry.file_type()?.is_file()
                        || expected.contains(&path)
                        || !matches!(
                            path.extension().and_then(|value| value.to_str()),
                            Some("opus" | "partial")
                        )
                    {
                        continue;
                    }
                    for number in 0.. {
                        let orphan = path.with_extension(format!("orphan.{number}"));
                        if !orphan.try_exists()? {
                            install(&path, &orphan)?;
                            count += 1;
                            break;
                        }
                    }
                }
            }
        }
        Ok(count)
    }
}

fn changed(rows: usize) -> Result<()> {
    if rows != 1 {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_CHUNK_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_CHUNK_BYTES {
        return Err(Error::InvalidMedia);
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn inspect(path: &Path, chunk: &Chunk) -> Result<Payload> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Payload::Absent),
        Err(error) => return Err(error.into()),
    };
    if Some(file.metadata()?.len()) != chunk.byte_size {
        return Ok(Payload::Corrupt);
    }
    match read_bounded(&mut file) {
        Ok(bytes)
            if chunk
                .sha256
                .as_ref()
                .is_some_and(|expected| digest(&bytes) == *expected) =>
        {
            Ok(Payload::Valid(bytes))
        }
        Ok(_) | Err(Error::InvalidMedia) => Ok(Payload::Corrupt),
        Err(error) => Err(error),
    }
}

fn install(source: &Path, destination: &Path) -> Result<()> {
    if source.parent() != destination.parent() || destination.try_exists()? {
        return Err(Error::Invalid);
    }
    fs::rename(source, destination)?;
    #[cfg(unix)]
    File::open(destination.parent().ok_or(Error::Invalid)?)?.sync_all()?;
    Ok(())
}
