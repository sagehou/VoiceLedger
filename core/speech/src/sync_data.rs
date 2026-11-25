use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::Path;

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::Serialize;
use voiceledger_audio::{CommittedReader, FinishedSession, SavedSession};
use voiceledger_core::contracts::{
    Count, Language, Source, StableId, SyncEntity, UtteranceKind, UtteranceRevision,
};
use voiceledger_core::sync::{Journal, SessionTextMetadata, SyncedTranscript};

use crate::transcript::{Transcripts, timestamp};
use crate::translations::{MAX_TEXT, selected_source};
use crate::{Error, Result};

pub(crate) const SCHEMA: &str = "
CREATE TABLE synced_sessions (
    id TEXT PRIMARY KEY,
    created_at_ms INTEGER NOT NULL,
    elapsed_ns INTEGER NOT NULL,
    interrupted INTEGER NOT NULL
) STRICT;
CREATE TABLE synced_tracks (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES synced_sessions(id),
    source TEXT NOT NULL CHECK(source IN ('mic','system_loopback'))
) STRICT;
CREATE TABLE synced_revisions (
    id TEXT PRIMARY KEY REFERENCES revisions(id)
) STRICT;
CREATE TABLE synced_tombstones (
    entity_id TEXT NOT NULL,
    scope TEXT NOT NULL CHECK(scope IN ('session','utterance')),
    PRIMARY KEY (entity_id,scope)
) STRICT;
CREATE TABLE sync_conflicts (
    utterance_id TEXT PRIMARY KEY REFERENCES utterances(id)
) STRICT;
CREATE TABLE sync_device (
    slot INTEGER PRIMARY KEY CHECK(slot=1),
    id TEXT NOT NULL
) STRICT;";

#[derive(Serialize)]
pub(crate) struct ConflictRevision {
    revision_id: StableId,
    text: String,
    language: Language,
    manual: bool,
}

pub(crate) fn open_audio(root: &Path) -> Result<Option<Connection>> {
    let manifest = root.join("manifest.sqlite3");
    match manifest.metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(manifest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_millis(150))?;
    Ok(Some(db))
}

pub(crate) fn source(
    db: &Connection,
    audio: Option<&Connection>,
    session: &str,
    track: &str,
    version: i64,
) -> Result<String> {
    if let Some(audio) = audio
        && let Some(source) = audio
            .query_row(
                "SELECT source FROM tracks WHERE id=?1 AND session_id=?2",
                params![track, session],
                |row| row.get(0),
            )
            .optional()?
    {
        return Ok(source);
    }
    if version < 5 {
        return Err(Error::Storage);
    }
    Ok(db.query_row(
        "SELECT source FROM synced_tracks WHERE id=?1 AND session_id=?2",
        params![track, session],
        |row| row.get(0),
    )?)
}

pub(crate) fn hidden(db: &Connection, scope: &str, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM synced_tombstones WHERE scope=?1 AND entity_id=?2)",
        params![scope, id],
        |row| row.get(0),
    )?)
}

pub(crate) fn finished(
    root: &Path,
    reader: Option<&CommittedReader>,
    session: &StableId,
) -> Result<Option<FinishedSession>> {
    let local = reader
        .map(|reader| reader.finished(session))
        .transpose()?
        .flatten();
    let Some((db, version)) = crate::library::open_readonly(root)? else {
        return Ok(local);
    };
    if version < 5 {
        return Ok(local);
    }
    let id = String::from(session.clone());
    if hidden(&db, "session", &id)? {
        return Ok(None);
    }
    if local.is_some() {
        return Ok(local);
    }
    let created_at_ms = db
        .query_row(
            "SELECT created_at_ms FROM synced_sessions WHERE id=?1",
            [&id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(created_at_ms.map(|created_at_ms| FinishedSession {
        id: session.clone(),
        created_at_ms,
    }))
}

pub(crate) fn imported_sessions(db: &Connection) -> Result<Vec<SavedSession>> {
    let mut query = db.prepare(
        "SELECT s.id,s.created_at_ms,s.elapsed_ns,s.interrupted,
         (SELECT count(*) FROM synced_tracks t WHERE t.session_id=s.id)
         FROM synced_sessions s
         WHERE NOT EXISTS(SELECT 1 FROM synced_tombstones t WHERE t.scope='session' AND t.entity_id=s.id)",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, created_at_ms, elapsed_ns, interrupted, tracks) = row?;
        Ok(SavedSession {
            id: stable(id)?,
            created_at_ms,
            elapsed_ns,
            interrupted,
            tracks,
            committed: 0,
            missing: 0,
        })
    })
    .collect()
}

impl Transcripts {
    pub(crate) fn sync_device(&self) -> Result<StableId> {
        let id = String::from(self.id()?);
        self.db.execute(
            "INSERT INTO sync_device(slot,id) VALUES (1,?1) ON CONFLICT(slot) DO NOTHING",
            [id],
        )?;
        stable(
            self.db
                .query_row("SELECT id FROM sync_device WHERE slot=1", [], |row| {
                    row.get(0)
                })?,
        )
    }

    pub(crate) fn outgoing(&self, root: &Path) -> Result<Vec<SyncedTranscript>> {
        let audio = open_audio(root)?;
        let mut query = self.db.prepare(
            "SELECT u.session_id, u.track_id, r.payload FROM revisions r
             JOIN utterances u ON r.utterance_id=u.id
             WHERE u.selected_revision IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM synced_revisions s WHERE s.id=r.id)
               AND NOT EXISTS (SELECT 1 FROM synced_tombstones t WHERE (t.scope='session' AND t.entity_id=u.session_id)
                 OR (t.scope='utterance' AND t.entity_id=u.id))
             ORDER BY u.session_id,u.track_id,u.start_ns,r.revision,r.id",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut sessions = BTreeMap::new();
        let mut tracks = BTreeMap::new();
        let mut output = Vec::new();
        for row in rows {
            let (session, track, payload) = row?;
            let revision: UtteranceRevision = serde_json::from_str(&payload)?;
            if !revision.is_durable() {
                continue;
            }
            if !sessions.contains_key(&session) {
                let metadata = session_metadata(&self.db, audio.as_ref(), &session)?;
                sessions.insert(session.clone(), metadata);
            }
            let Some(metadata) = &sessions[&session] else {
                continue;
            };
            let kind = match tracks.entry((track.clone(), session.clone())) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => *entry.insert(
                    match source(&self.db, audio.as_ref(), &session, &track, 5)?.as_str() {
                        "mic" => Source::Mic,
                        "system_loopback" => Source::SystemLoopback,
                        _ => return Err(Error::Storage),
                    },
                ),
            };
            output.push(SyncedTranscript {
                session_id: stable(session)?,
                source: kind,
                revision,
                session: Some(metadata.clone()),
            });
        }
        Ok(output)
    }

    pub(crate) fn import(&self, journal: &Journal) -> Result<usize> {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        for event in journal.tombstones() {
            let scope = match event.entity_kind {
                SyncEntity::Session => "session",
                SyncEntity::UtteranceRevision => "utterance",
                _ => continue,
            };
            tx.execute(
                "INSERT INTO synced_tombstones(entity_id,scope) VALUES (?1,?2) ON CONFLICT DO NOTHING",
                params![String::from(event.entity_id.clone()), scope],
            )?;
        }
        let mut changed = BTreeSet::new();
        let mut count = 0;
        for text in journal.transcripts() {
            let revision = &text.revision;
            let session = String::from(text.session_id.clone());
            let track = String::from(revision.track_id.clone());
            let utterance = String::from(revision.utterance_id.clone());
            let id = String::from(revision.id.clone());
            if let Some(payload) = tx
                .query_row("SELECT payload FROM revisions WHERE id=?1", [&id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
            {
                if serde_json::from_str::<UtteranceRevision>(&payload)? != *revision {
                    return Err(Error::RevisionConflict);
                }
                continue;
            }
            register_metadata(&tx, text, &session, &track)?;
            let start = i64::from(revision.range.start_ns);
            let end = i64::from(revision.range.end_ns);
            tx.execute(
                "INSERT INTO utterances(id,session_id,track_id,start_ns,end_ns) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(id) DO NOTHING", params![utterance, session, track, start, end],
            )?;
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM utterances WHERE id=?1 AND session_id=?2 AND track_id=?3 AND start_ns=?4 AND end_ns=?5)",
                params![utterance, session, track, start, end], |row| row.get(0),
            )?;
            if !valid {
                return Err(Error::RevisionConflict);
            }
            // The index is device-local. The authenticated payload keeps the original revision and parents.
            tx.execute(
                "INSERT INTO revisions(id,utterance_id,revision,payload) VALUES (?1,?2,
                 (SELECT coalesce(max(revision),0)+1 FROM revisions WHERE utterance_id=?2),?3)",
                params![id, utterance, serde_json::to_string(revision)?],
            )?;
            tx.execute("INSERT INTO synced_revisions(id) VALUES (?1)", [&id])?;
            changed.insert(utterance);
            count += 1;
        }
        for utterance in changed {
            select_incoming(&tx, &utterance)?;
        }
        tx.commit()?;
        Ok(count)
    }

    pub(crate) fn resolve_conflict(
        &self,
        session: &StableId,
        utterance: &StableId,
        expected_heads: &[StableId],
        text: &str,
    ) -> Result<()> {
        let text = text.trim();
        if text.is_empty()
            || text.len() > MAX_TEXT
            || text.contains('\0')
            || !(2..=64).contains(&expected_heads.len())
        {
            return Err(Error::Invalid);
        }
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let utterance_id = String::from(utterance.clone());
        if hidden(&tx, "session", &String::from(session.clone()))?
            || hidden(&tx, "utterance", &utterance_id)?
        {
            return Err(Error::RevisionConflict);
        }
        let revisions = revisions(&tx, &utterance_id)?;
        let heads = valid_heads(&revisions);
        if heads.len() != expected_heads.len() || !heads.keys().copied().eq(expected_heads.iter()) {
            return Err(Error::RevisionConflict);
        }
        let mut revision =
            selected_source(&tx, session, utterance)?.ok_or(Error::RevisionConflict)?;
        let ordinal = next_ordinal(&tx, &utterance_id)?;
        revision.id = self.id()?;
        revision.parent_revision_ids = expected_heads.to_vec();
        revision.revision = NonZeroU32::new(ordinal).ok_or(Error::Storage)?;
        revision.kind = UtteranceKind::Manual;
        revision.text = text.to_owned();
        revision.model = None;
        revision.confidence = None;
        revision.created_at_unix_ms = timestamp()?;
        let id = String::from(revision.id.clone());
        tx.execute(
            "INSERT INTO revisions(id,utterance_id,revision,payload) VALUES (?1,?2,?3,?4)",
            params![id, utterance_id, ordinal, serde_json::to_string(&revision)?],
        )?;
        tx.execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2",
            params![id, utterance_id],
        )?;
        tx.execute(
            "DELETE FROM sync_conflicts WHERE utterance_id=?1",
            [&utterance_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn session_metadata(
    db: &Connection,
    audio: Option<&Connection>,
    session: &str,
) -> Result<Option<SessionTextMetadata>> {
    let query = |db: &Connection, source: &str| -> Result<Option<(i64, i64, bool)>> {
        Ok(db
            .query_row(source, [session], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?)
    };
    let local = audio.map(|audio| query(audio,
        "SELECT created_at_ms, elapsed_ns, (state=6 OR interrupted_tail=1) FROM sessions WHERE id=?1 AND state IN (5,6)",
    )).transpose()?.flatten();
    let metadata = match local {
        Some(meta) => Some(meta),
        None => query(
            db,
            "SELECT created_at_ms,elapsed_ns,interrupted FROM synced_sessions WHERE id=?1",
        )?,
    };
    metadata
        .map(|(created, elapsed, interrupted)| {
            Ok(SessionTextMetadata {
                created_at_ms: Count::try_from(created).map_err(|_| Error::Storage)?,
                elapsed_ns: Count::try_from(elapsed).map_err(|_| Error::Storage)?,
                interrupted,
                title: crate::library::read_title(db, session)?,
            })
        })
        .transpose()
}

fn register_metadata(
    db: &Connection,
    incoming: &SyncedTranscript,
    session: &str,
    track: &str,
) -> Result<()> {
    let revision = &incoming.revision;
    let meta = incoming.session.as_ref();
    let created = i64::from(meta.map_or(revision.created_at_unix_ms, |value| value.created_at_ms));
    let elapsed = i64::from(meta.map_or(revision.range.end_ns, |value| value.elapsed_ns));
    let interrupted = meta.is_some_and(|value| value.interrupted);
    db.execute(
        "INSERT INTO synced_sessions(id,created_at_ms,elapsed_ns,interrupted) VALUES (?1,?2,?3,?4)
         ON CONFLICT(id) DO UPDATE SET elapsed_ns=max(elapsed_ns,excluded.elapsed_ns)",
        params![session, created, elapsed, interrupted],
    )?;
    if let Some(title) = meta.and_then(|value| value.title.as_deref()) {
        db.execute(
            "INSERT INTO session_titles(session_id,title) VALUES (?1,?2) ON CONFLICT DO NOTHING",
            params![session, title],
        )?;
    }
    let source = match incoming.source {
        Source::Mic => "mic",
        Source::SystemLoopback => "system_loopback",
    };
    db.execute("INSERT INTO synced_tracks(id,session_id,source) VALUES (?1,?2,?3) ON CONFLICT(id) DO NOTHING",
        params![track, session, source])?;
    if !db.query_row::<bool, _, _>(
        "SELECT EXISTS(SELECT 1 FROM synced_tracks WHERE id=?1 AND session_id=?2 AND source=?3)",
        params![track, session, source],
        |row| row.get(0),
    )? {
        return Err(Error::RevisionConflict);
    }
    Ok(())
}

pub(crate) fn next_ordinal(db: &Connection, utterance: &str) -> Result<u32> {
    let ordinal: u32 = db.query_row(
        "SELECT coalesce(max(max(revision, json_extract(payload, '$.revision'))),0) FROM revisions WHERE utterance_id=?1",
        [utterance], |row| row.get(0),
    )?;
    ordinal.checked_add(1).ok_or(Error::Storage)
}

fn revisions(db: &Connection, utterance: &str) -> Result<Vec<UtteranceRevision>> {
    let mut query = db.prepare(
        "SELECT payload FROM revisions WHERE utterance_id=?1 ORDER BY json_extract(payload,'$.revision'),id",
    )?;
    let rows = query.query_map([utterance], |row| row.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

fn valid_heads(revisions: &[UtteranceRevision]) -> BTreeMap<&StableId, &UtteranceRevision> {
    let mut ready = BTreeSet::new();
    let mut heads = BTreeMap::<&StableId, &UtteranceRevision>::new();
    for revision in revisions {
        if !revision
            .parent_revision_ids
            .iter()
            .all(|parent| ready.contains(parent))
        {
            continue;
        }
        ready.insert(&revision.id);
        for parent in &revision.parent_revision_ids {
            if revision.kind == UtteranceKind::Manual
                || heads
                    .get(parent)
                    .is_some_and(|value| value.kind != UtteranceKind::Manual)
            {
                heads.remove(parent);
            }
        }
        heads.insert(&revision.id, revision);
    }
    heads
}

fn select_incoming(db: &Connection, utterance: &str) -> Result<()> {
    let revisions = revisions(db, utterance)?;
    let heads = valid_heads(&revisions);
    if heads.len() > 1 {
        db.execute(
            "INSERT INTO sync_conflicts(utterance_id) VALUES (?1) ON CONFLICT DO NOTHING",
            [utterance],
        )?;
    } else {
        db.execute(
            "DELETE FROM sync_conflicts WHERE utterance_id=?1",
            [utterance],
        )?;
    }
    if let Some(best) = heads.values().max_by_key(|value| {
        (
            value.kind == UtteranceKind::Manual,
            value.revision,
            &value.id,
        )
    }) {
        let id = String::from(best.id.clone());
        db.execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2 AND
             (selected_revision IS NULL OR (?3=1 AND ?4=1))",
            params![
                id,
                utterance,
                heads.len() == 1,
                best.kind == UtteranceKind::Manual
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn conflict(db: &Connection, utterance: &StableId) -> Result<Vec<ConflictRevision>> {
    let utterance = String::from(utterance.clone());
    if !db.query_row::<bool, _, _>(
        "SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE utterance_id=?1)",
        [&utterance],
        |row| row.get(0),
    )? {
        return Ok(Vec::new());
    }
    let revisions = revisions(db, &utterance)?;
    Ok(valid_heads(&revisions)
        .into_values()
        .map(|revision| ConflictRevision {
            revision_id: revision.id.clone(),
            text: revision.text.clone(),
            language: revision.language,
            manual: revision.kind == UtteranceKind::Manual,
        })
        .collect())
}

fn stable(value: String) -> Result<StableId> {
    value.try_into().map_err(|_| Error::Storage)
}
