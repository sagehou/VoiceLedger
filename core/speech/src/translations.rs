use std::num::NonZeroU32;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use voiceledger_core::contracts::{
    Authorship, Language, ModelRef, StableId, TranslationRevision, UtteranceRevision,
};

use crate::transcript::{Transcripts, timestamp};
use crate::{Error, Result};

const TARGET: Language = Language::Zh;
pub(crate) const MAX_TEXT: usize = 8 * 1024;
pub(crate) const SCHEMA: &str = "
CREATE TABLE translation_revisions (
    id TEXT PRIMARY KEY,
    utterance_id TEXT NOT NULL REFERENCES utterances(id),
    source_revision_id TEXT NOT NULL REFERENCES revisions(id),
    target_language TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    authorship TEXT NOT NULL CHECK (authorship IN ('manual','machine')),
    machine_key TEXT,
    payload TEXT NOT NULL,
    CHECK ((authorship='machine') = (machine_key IS NOT NULL)),
    UNIQUE(utterance_id,target_language,revision),
    UNIQUE(utterance_id,target_language,source_revision_id,machine_key)
) STRICT;";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManualEdit {
    pub session_id: StableId,
    pub utterance_id: StableId,
    pub source_revision_id: StableId,
    pub expected_translation_id: Option<StableId>,
    pub text: String,
}

#[derive(Serialize)]
pub struct TranslationLine {
    revision_id: StableId,
    target_language: Language,
    text: String,
    authorship: Authorship,
    stale: bool,
}

pub(crate) fn display(
    db: &Connection,
    source: &UtteranceRevision,
) -> Result<Option<TranslationLine>> {
    Ok(
        selected(db, &source.utterance_id)?.map(|revision| TranslationLine {
            stale: revision.is_stale(&source.id),
            revision_id: revision.id,
            target_language: revision.target_language,
            text: revision.text,
            authorship: revision.authorship,
        }),
    )
}

pub(crate) fn selected(
    db: &Connection,
    utterance: &StableId,
) -> Result<Option<TranslationRevision>> {
    let payload: Option<String> = db
        .query_row(
            "SELECT payload FROM translation_revisions WHERE utterance_id=?1 AND target_language=?2
             ORDER BY (authorship='manual') DESC,revision DESC LIMIT 1",
            params![String::from(utterance.clone()), TARGET.code()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(payload
        .map(|payload| serde_json::from_str(&payload))
        .transpose()?)
}

impl Transcripts {
    pub(crate) fn translation_sources(
        &self,
        session: &StableId,
        after: &str,
    ) -> Result<Vec<UtteranceRevision>> {
        let mut query = self.db.prepare(
            "SELECT r.payload FROM utterances u JOIN revisions r ON r.id=u.selected_revision
             WHERE u.session_id=?1 AND u.id>?2 ORDER BY u.id LIMIT 32",
        )?;
        let payloads = query.query_map(params![String::from(session.clone()), after], |row| {
            row.get::<_, String>(0)
        })?;
        payloads
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect()
    }

    pub(crate) fn save_manual_translation(&self, edit: ManualEdit) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let source = selected_source(&tx, &edit.session_id, &edit.utterance_id)?;
        let Some(source) = source.filter(|source| source.id == edit.source_revision_id) else {
            return Err(Error::RevisionConflict);
        };
        let head = selected(&tx, &edit.utterance_id)?;
        if head.as_ref().map(|revision| &revision.id) != edit.expected_translation_id.as_ref() {
            return Err(Error::RevisionConflict);
        }
        self.append_translation(&tx, &source, edit.text, None)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn save_machine_translation(
        &self,
        session: &StableId,
        source: &UtteranceRevision,
        text: String,
        model: ModelRef,
    ) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        if selected_source(&tx, session, &source.utterance_id)?
            .is_none_or(|selected| selected.id != source.id)
        {
            return Err(Error::RevisionConflict);
        }
        let added = self.append_translation(&tx, source, text, Some(model))?;
        tx.commit()?;
        Ok(added)
    }

    fn append_translation(
        &self,
        db: &Connection,
        source: &UtteranceRevision,
        text: String,
        model: Option<ModelRef>,
    ) -> Result<bool> {
        let text = text.trim().to_owned();
        if !source.is_durable()
            || source.language != Language::En
            || text.is_empty()
            || text.len() > MAX_TEXT
            || text.contains('\0')
        {
            return Err(Error::Invalid);
        }
        let utterance = String::from(source.utterance_id.clone());
        let prior: Option<String> = db
            .query_row(
                "SELECT payload FROM translation_revisions WHERE utterance_id=?1 AND target_language=?2
                 ORDER BY revision DESC LIMIT 1",
                params![utterance, TARGET.code()],
                |row| row.get(0),
            )
            .optional()?;
        let (translation_id, ordinal, parents) = match prior {
            Some(payload) => {
                let revision: TranslationRevision = serde_json::from_str(&payload)?;
                (
                    revision.translation_id,
                    revision
                        .revision
                        .get()
                        .checked_add(1)
                        .ok_or(Error::Storage)?,
                    vec![revision.id],
                )
            }
            None => (self.id()?, 1, Vec::new()),
        };
        let machine_key = model.as_ref().map(serde_json::to_string).transpose()?;
        let authorship = if model.is_some() {
            Authorship::Machine
        } else {
            Authorship::Manual
        };
        let revision = TranslationRevision {
            id: self.id()?,
            translation_id,
            revision: NonZeroU32::new(ordinal).ok_or(Error::Storage)?,
            parent_revision_ids: parents,
            source_revision_id: source.id.clone(),
            target_language: TARGET,
            text,
            authorship,
            model,
            created_at_unix_ms: timestamp()?,
        };
        let changed = db.execute(
            "INSERT INTO translation_revisions(id,utterance_id,source_revision_id,target_language,revision,authorship,machine_key,payload)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(utterance_id,target_language,source_revision_id,machine_key) DO NOTHING",
            params![
                String::from(revision.id.clone()),
                utterance,
                String::from(source.id.clone()),
                TARGET.code(),
                ordinal,
                if authorship == Authorship::Manual { "manual" } else { "machine" },
                machine_key,
                serde_json::to_string(&revision)?,
            ],
        )?;
        Ok(changed != 0)
    }
}

pub(crate) fn selected_source(
    db: &Connection,
    session: &StableId,
    utterance: &StableId,
) -> Result<Option<UtteranceRevision>> {
    let payload: Option<String> = db
        .query_row(
            "SELECT r.payload FROM revisions r JOIN utterances u ON r.id=u.selected_revision
             WHERE u.id=?1 AND u.session_id=?2",
            params![
                String::from(utterance.clone()),
                String::from(session.clone())
            ],
            |row| row.get(0),
        )
        .optional()?;
    Ok(payload
        .map(|payload| serde_json::from_str(&payload))
        .transpose()?)
}
