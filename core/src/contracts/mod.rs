mod types;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::track::{TrackCommand, TrackMachine, TrackState};
use crate::{Session, SessionCommand, SessionState};

pub use types::*;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum ContractError {
    UnsupportedVersion = -10,
    InvalidJson = -11,
    DuplicateIdentity = -12,
    InvalidReference = -13,
    InvalidTransition = -14,
    InvalidTimeline = -15,
    InvalidDurability = -16,
    InvalidRevision = -17,
    InvalidMetadata = -18,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DomainBatch {
    pub schema_version: u32,
    pub sessions: Vec<SessionRecord>,
    pub tracks: Vec<TrackRecord>,
    pub audio_chunks: Vec<AudioChunk>,
    pub gaps: Vec<Gap>,
    pub silence: Vec<Silence>,
    pub utterance_revisions: Vec<UtteranceRevision>,
    pub utterance_selections: Vec<UtteranceSelection>,
    pub translation_revisions: Vec<TranslationRevision>,
    pub speaker_assignments: Vec<SpeakerAssignment>,
    pub speaker_profiles: Vec<SpeakerProfile>,
    pub ai_jobs: Vec<AiJob>,
    pub device_capability_profiles: Vec<DeviceCapabilityProfile>,
    pub benchmark_results: Vec<BenchmarkResult>,
    pub sync_events: Vec<SyncEvent>,
}

type Index<'a, T> = BTreeMap<&'a StableId, &'a T>;

struct Indexes<'a> {
    sessions: Index<'a, SessionRecord>,
    tracks: Index<'a, TrackRecord>,
    chunks: Index<'a, AudioChunk>,
    utterances: Index<'a, UtteranceRevision>,
    profiles: Index<'a, SpeakerProfile>,
    devices: Index<'a, DeviceCapabilityProfile>,
}

fn index<T>(records: &[T], id: impl Fn(&T) -> &StableId) -> Index<'_, T> {
    records.iter().map(|record| (id(record), record)).collect()
}

fn require(condition: bool, failure: ContractError) -> Result<(), ContractError> {
    condition.then_some(()).ok_or(failure)
}

impl DomainBatch {
    pub fn decode(data: &[u8]) -> Result<Self, ContractError> {
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }

        let version: Version =
            serde_json::from_slice(data).map_err(|_| ContractError::InvalidJson)?;
        require(
            version.schema_version == SCHEMA_VERSION,
            ContractError::UnsupportedVersion,
        )?;
        let batch: Self = serde_json::from_slice(data).map_err(|_| ContractError::InvalidJson)?;
        batch.validate()?;
        Ok(batch)
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        require(
            self.schema_version == SCHEMA_VERSION,
            ContractError::UnsupportedVersion,
        )?;
        self.validate_identities()?;
        let indexes = Indexes {
            sessions: index(&self.sessions, |item| &item.id),
            tracks: index(&self.tracks, |item| &item.id),
            chunks: index(&self.audio_chunks, |item| &item.id),
            utterances: index(&self.utterance_revisions, |item| &item.id),
            profiles: index(&self.speaker_profiles, |item| &item.id),
            devices: index(&self.device_capability_profiles, |item| &item.id),
        };
        self.validate_lifecycle(&indexes)?;
        self.validate_audio_timeline()?;
        self.validate_revisions(&indexes)?;
        self.validate_people(&indexes)?;
        self.validate_devices(&indexes)?;
        self.validate_jobs(&indexes)?;
        self.validate_sync(&indexes)
    }

    fn validate_identities(&self) -> Result<(), ContractError> {
        let ids = self
            .sessions
            .iter()
            .map(|record| &record.id)
            .chain(self.tracks.iter().map(|record| &record.id))
            .chain(self.audio_chunks.iter().map(|record| &record.id))
            .chain(self.gaps.iter().map(|record| &record.id))
            .chain(self.silence.iter().map(|record| &record.id))
            .chain(self.utterance_revisions.iter().map(|record| &record.id))
            .chain(self.translation_revisions.iter().map(|record| &record.id))
            .chain(self.speaker_assignments.iter().map(|record| &record.id))
            .chain(self.speaker_profiles.iter().map(|record| &record.id))
            .chain(self.ai_jobs.iter().map(|record| &record.id))
            .chain(
                self.device_capability_profiles
                    .iter()
                    .map(|record| &record.id),
            )
            .chain(self.benchmark_results.iter().map(|record| &record.id))
            .chain(self.sync_events.iter().map(|record| &record.id));
        let mut unique = BTreeSet::new();
        for id in ids {
            require(unique.insert(id), ContractError::DuplicateIdentity)?;
        }
        Ok(())
    }

    fn validate_lifecycle(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        let mut tracks_by_session: BTreeMap<&StableId, Vec<TrackState>> = BTreeMap::new();
        let mut sources = BTreeSet::new();
        for track in &self.tracks {
            require(
                indexes.sessions.contains_key(&track.session_id),
                ContractError::InvalidReference,
            )?;
            require(
                sources.insert((&track.session_id, track.source)),
                ContractError::InvalidTransition,
            )?;
            require(
                !track.endpoint_id.is_empty()
                    && !track.source_clock.is_empty()
                    && !track.format.sample_format.is_empty(),
                ContractError::InvalidMetadata,
            )?;
            let mut machine = TrackMachine::new(&track.endpoint_id);
            require_ordered(track.transitions.iter().map(|step| step.at_ns))?;
            for step in &track.transitions {
                if step.command == TrackCommand::ReconnectConfirmed {
                    require(
                        step.reason == TransitionReason::UserRequest,
                        ContractError::InvalidTransition,
                    )?;
                }
                machine
                    .apply(
                        step.command,
                        step.at_ns.get(),
                        step.confirmed_endpoint_id.as_deref(),
                    )
                    .map_err(|_| ContractError::InvalidTransition)?;
            }
            require(
                machine.state() == track.capture_state
                    && machine.source_epoch() == track.source_epoch,
                ContractError::InvalidTransition,
            )?;
            tracks_by_session
                .entry(&track.session_id)
                .or_default()
                .push(machine.state());
        }

        for record in &self.sessions {
            let mut machine = Session::default();
            require_ordered(record.transitions.iter().map(|step| step.at_ns))?;
            for step in &record.transitions {
                machine
                    .apply(step.command, step.at_ns.get())
                    .map_err(|_| ContractError::InvalidTransition)?;
            }
            require(
                machine.state() == record.capture_state,
                ContractError::InvalidTransition,
            )?;
            let started = record
                .transitions
                .iter()
                .any(|step| step.command == SessionCommand::Start);
            let ended = matches!(
                record.capture_state,
                SessionState::Completed | SessionState::Interrupted
            );
            require(
                started == record.started_at_unix_ms.is_some()
                    && ended == record.ended_at_unix_ms.is_some(),
                ContractError::InvalidTransition,
            )?;
            let tracks = tracks_by_session
                .get(&record.id)
                .map_or(&[][..], Vec::as_slice);
            require(
                match record.capture_state {
                    SessionState::Recording => tracks.contains(&TrackState::Recording),
                    SessionState::Idle | SessionState::Preparing | SessionState::Paused => {
                        !tracks.contains(&TrackState::Recording)
                    }
                    SessionState::Stopping => true,
                    SessionState::Completed | SessionState::Interrupted => {
                        tracks.iter().all(|state| {
                            matches!(state, TrackState::Completed | TrackState::Interrupted)
                        })
                    }
                },
                ContractError::InvalidTransition,
            )?;
        }
        Ok(())
    }

    fn validate_audio_timeline(&self) -> Result<(), ContractError> {
        let epoch_changes: BTreeMap<_, Vec<_>> = self
            .tracks
            .iter()
            .map(|track| {
                let changes = track
                    .transitions
                    .iter()
                    .filter(|step| step.command == TrackCommand::ReconnectConfirmed)
                    .map(|step| step.at_ns)
                    .collect();
                (&track.id, changes)
            })
            .collect();
        let mut last_chunk: BTreeMap<&StableId, &AudioChunk> = BTreeMap::new();
        let mut spans = Vec::new();
        for chunk in &self.audio_chunks {
            validate_span(
                &epoch_changes,
                &chunk.track_id,
                chunk.source_epoch,
                chunk.range,
            )?;
            require(
                !chunk.codec.is_empty() && !chunk.format.sample_format.is_empty(),
                ContractError::InvalidMetadata,
            )?;
            require(
                chunk.commit_state != AudioCommitState::Committed || chunk.is_saved(),
                ContractError::InvalidDurability,
            )?;
            if let Some(previous) = last_chunk.insert(&chunk.track_id, chunk) {
                require(
                    previous.sequence < chunk.sequence
                        && previous.range.end_ns <= chunk.range.start_ns
                        && previous.source_epoch <= chunk.source_epoch,
                    ContractError::InvalidTimeline,
                )?;
            }
            if chunk.commit_state != AudioCommitState::Missing {
                spans.push((&chunk.track_id, chunk.range));
            }
        }
        for (track_id, epoch, range) in self
            .gaps
            .iter()
            .map(|gap| (&gap.track_id, gap.source_epoch, gap.range))
            .chain(
                self.silence
                    .iter()
                    .map(|silence| (&silence.track_id, silence.source_epoch, silence.range)),
            )
        {
            validate_span(&epoch_changes, track_id, epoch, range)?;
            spans.push((track_id, range));
        }
        spans.sort_unstable_by_key(|(track, range)| (*track, range.start_ns));
        require(
            spans
                .windows(2)
                .all(|pair| pair[0].0 != pair[1].0 || pair[0].1.end_ns <= pair[1].1.start_ns),
            ContractError::InvalidTimeline,
        )
    }

    fn validate_revisions(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        let mut utterance_tracks = BTreeMap::new();
        let mut manual = BTreeSet::new();
        for revision in &self.utterance_revisions {
            require(
                indexes.tracks.contains_key(&revision.track_id),
                ContractError::InvalidReference,
            )?;
            require(
                revision.range.is_nonempty() && !revision.source.is_empty(),
                ContractError::InvalidTimeline,
            )?;
            let track_id = utterance_tracks
                .entry(&revision.utterance_id)
                .or_insert(&revision.track_id);
            require(
                *track_id == &revision.track_id,
                ContractError::InvalidRevision,
            )?;
            for span in &revision.source {
                let chunk = referenced_chunk(indexes, span)?;
                require(
                    chunk.track_id == revision.track_id,
                    ContractError::InvalidReference,
                )?;
                require(
                    revision.range.contains(span.range),
                    ContractError::InvalidTimeline,
                )?;
            }
            validate_confidence(revision.confidence)?;
            validate_authorship(
                revision.kind != UtteranceKind::Manual,
                revision.model.as_ref(),
            )?;
            if revision.kind == UtteranceKind::Manual {
                require(
                    !revision.parent_revision_ids.is_empty(),
                    ContractError::InvalidRevision,
                )?;
                manual.insert(&revision.utterance_id);
            }
            let mut parents = BTreeSet::new();
            for parent_id in &revision.parent_revision_ids {
                let parent = indexes
                    .utterances
                    .get(parent_id)
                    .ok_or(ContractError::InvalidReference)?;
                require(
                    parents.insert(parent_id)
                        && parent.utterance_id == revision.utterance_id
                        && parent.revision < revision.revision
                        && parent.is_durable(),
                    ContractError::InvalidRevision,
                )?;
            }
        }
        let mut selections = BTreeSet::new();
        for selection in &self.utterance_selections {
            let revision = indexes
                .utterances
                .get(&selection.revision_id)
                .ok_or(ContractError::InvalidReference)?;
            require(
                selections.insert(&selection.utterance_id)
                    && revision.utterance_id == selection.utterance_id
                    && revision.is_durable()
                    && (revision.kind == UtteranceKind::Manual
                        || !manual.contains(&selection.utterance_id)),
                ContractError::InvalidRevision,
            )?;
        }
        let translations = index(&self.translation_revisions, |revision| &revision.id);
        for revision in &self.translation_revisions {
            let source = indexes
                .utterances
                .get(&revision.source_revision_id)
                .ok_or(ContractError::InvalidReference)?;
            require(source.is_durable(), ContractError::InvalidRevision)?;
            validate_authorship(
                revision.authorship == Authorship::Machine,
                revision.model.as_ref(),
            )?;
            let mut parents = BTreeSet::new();
            for parent_id in &revision.parent_revision_ids {
                let parent = translations
                    .get(parent_id)
                    .ok_or(ContractError::InvalidReference)?;
                let parent_source = indexes
                    .utterances
                    .get(&parent.source_revision_id)
                    .ok_or(ContractError::InvalidReference)?;
                require(
                    parents.insert(parent_id)
                        && parent.translation_id == revision.translation_id
                        && parent.target_language == revision.target_language
                        && parent_source.utterance_id == source.utterance_id
                        && parent.revision < revision.revision,
                    ContractError::InvalidRevision,
                )?;
            }
        }
        Ok(())
    }

    fn validate_people(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        for assignment in &self.speaker_assignments {
            let track = indexes
                .tracks
                .get(&assignment.track_id)
                .ok_or(ContractError::InvalidReference)?;
            require(
                track.session_id == assignment.session_id,
                ContractError::InvalidReference,
            )?;
            require(
                assignment.range.is_nonempty(),
                ContractError::InvalidTimeline,
            )?;
            validate_confidence(assignment.confidence)?;
            validate_authorship(
                assignment.authorship == Authorship::Machine,
                assignment.model.as_ref(),
            )?;
            if assignment.confirmed_name.is_some() || assignment.confirmed_profile_id.is_some() {
                require(
                    assignment.authorship == Authorship::Manual && assignment.speaker_id.is_some(),
                    ContractError::InvalidRevision,
                )?;
            }
            if let Some(profile_id) = &assignment.confirmed_profile_id {
                require(
                    indexes.profiles.contains_key(profile_id),
                    ContractError::InvalidReference,
                )?;
            }
        }
        for profile in &self.speaker_profiles {
            require(
                !profile.display_name.is_empty() && !profile.encrypted_sample_refs.is_empty(),
                ContractError::InvalidMetadata,
            )?;
        }
        Ok(())
    }

    fn validate_devices(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        for profile in &self.device_capability_profiles {
            require(
                !profile.os_version.is_empty()
                    && !profile.abi.is_empty()
                    && profile.battery_percent.is_none_or(|percent| percent <= 100)
                    && profile
                        .total_memory_bytes
                        .is_none_or(|total| total.get() > 0),
                ContractError::InvalidMetadata,
            )?;
        }
        for result in &self.benchmark_results {
            require(
                indexes.devices.contains_key(&result.device_profile_id),
                ContractError::InvalidReference,
            )?;
            validate_model(&result.model)?;
            require(
                result
                    .warm_rtf
                    .is_none_or(|rtf| rtf.is_finite() && rtf >= 0.0),
                ContractError::InvalidMetadata,
            )?;
            if result.stage != BenchmarkStage::Estimate {
                require(
                    result.corpus_sha256.is_some()
                        && result.duration_ms.is_some_and(|time| time.get() > 0)
                        && result
                            .evidence_ref
                            .as_ref()
                            .is_some_and(|reference| !reference.is_empty()),
                    ContractError::InvalidMetadata,
                )?;
            }
        }
        Ok(())
    }

    fn validate_jobs(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        for job in &self.ai_jobs {
            require(
                indexes.sessions.contains_key(&job.session_id),
                ContractError::InvalidReference,
            )?;
            require(!job.inputs.is_empty(), ContractError::InvalidMetadata)?;
            validate_model(&job.model)?;
            for input in &job.inputs {
                let track_id = match (&job.kind, input) {
                    (AiJobKind::Asr | AiJobKind::Speaker, AiInput::Audio { span }) => {
                        let chunk = referenced_chunk(indexes, span)?;
                        require(
                            chunk.commit_state != AudioCommitState::Pending,
                            ContractError::InvalidDurability,
                        )?;
                        if matches!(job.state, AiJobState::Queued | AiJobState::Running) {
                            require(chunk.is_saved(), ContractError::InvalidDurability)?;
                        }
                        &chunk.track_id
                    }
                    (AiJobKind::Translation, AiInput::UtteranceRevision { revision_id }) => {
                        let revision = indexes
                            .utterances
                            .get(revision_id)
                            .ok_or(ContractError::InvalidReference)?;
                        require(revision.is_durable(), ContractError::InvalidRevision)?;
                        &revision.track_id
                    }
                    _ => return Err(ContractError::InvalidReference),
                };
                let track = indexes
                    .tracks
                    .get(track_id)
                    .ok_or(ContractError::InvalidReference)?;
                require(
                    track.session_id == job.session_id,
                    ContractError::InvalidReference,
                )?;
            }
        }
        Ok(())
    }

    fn validate_sync(&self, indexes: &Indexes<'_>) -> Result<(), ContractError> {
        let mut sequences = BTreeMap::new();
        let events = index(&self.sync_events, |event| &event.id);
        for event in &self.sync_events {
            if let Some(previous) = sequences.insert(&event.device_id, event.sequence) {
                require(previous < event.sequence, ContractError::InvalidTimeline)?;
            }
            require(
                (event.operation == SyncOperation::Restore)
                    == event.restored_from_tombstone_id.is_some(),
                ContractError::InvalidReference,
            )?;
            if let Some(tombstone) = event
                .restored_from_tombstone_id
                .as_ref()
                .and_then(|id| events.get(id))
            {
                require(
                    tombstone.operation == SyncOperation::Tombstone
                        && event.entity_kind == tombstone.entity_kind
                        && event.entity_id != tombstone.entity_id,
                    ContractError::InvalidReference,
                )?;
            }
            if indexes.utterances.contains_key(&event.entity_id) {
                require(
                    event.entity_kind == SyncEntity::UtteranceRevision,
                    ContractError::InvalidReference,
                )?;
            }
            for id in [&event.entity_id, &event.revision_id] {
                require(
                    indexes
                        .utterances
                        .get(id)
                        .is_none_or(|revision| revision.is_durable()),
                    ContractError::InvalidRevision,
                )?;
            }
            if let Some(chunk) = indexes.chunks.get(&event.entity_id) {
                require(
                    event.entity_kind == SyncEntity::AudioChunk,
                    ContractError::InvalidReference,
                )?;
                if event.operation != SyncOperation::Tombstone {
                    require(
                        chunk.commit_state != AudioCommitState::Pending,
                        ContractError::InvalidDurability,
                    )?;
                }
            }
        }
        Ok(())
    }
}

fn require_ordered(times: impl IntoIterator<Item = Count>) -> Result<(), ContractError> {
    let mut previous = 0;
    for time in times {
        require(previous <= time.get(), ContractError::InvalidTransition)?;
        previous = time.get();
    }
    Ok(())
}

fn validate_span(
    epoch_changes: &BTreeMap<&StableId, Vec<Count>>,
    track_id: &StableId,
    epoch: u32,
    range: TimeRange,
) -> Result<(), ContractError> {
    let changes = epoch_changes
        .get(track_id)
        .ok_or(ContractError::InvalidReference)?;
    let epoch = epoch as usize;
    require(
        range.is_nonempty()
            && changes.partition_point(|time| *time <= range.start_ns) == epoch
            && changes.partition_point(|time| *time < range.end_ns) == epoch,
        ContractError::InvalidTimeline,
    )
}

fn referenced_chunk<'a>(
    indexes: &Indexes<'a>,
    span: &ChunkSpan,
) -> Result<&'a AudioChunk, ContractError> {
    let chunk = indexes
        .chunks
        .get(&span.chunk_id)
        .copied()
        .ok_or(ContractError::InvalidReference)?;
    require(
        span.range.is_nonempty() && chunk.range.contains(span.range),
        ContractError::InvalidTimeline,
    )?;
    Ok(chunk)
}

fn validate_model(model: &ModelRef) -> Result<(), ContractError> {
    require(
        !model.model_id.is_empty()
            && !model.model_version.is_empty()
            && !model.runtime_version.is_empty(),
        ContractError::InvalidMetadata,
    )
}

fn validate_authorship(machine: bool, model: Option<&ModelRef>) -> Result<(), ContractError> {
    require(!machine || model.is_some(), ContractError::InvalidRevision)?;
    model.map_or(Ok(()), validate_model)
}

fn validate_confidence(confidence: Option<f64>) -> Result<(), ContractError> {
    require(
        confidence.is_none_or(|value| (0.0..=1.0).contains(&value)),
        ContractError::InvalidMetadata,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../../../contracts/fixtures/domain-v1.json");

    #[test]
    fn reads_and_writes_the_versioned_batch() {
        let batch = DomainBatch::decode(FIXTURE).unwrap();
        let encoded = serde_json::to_vec(&batch).unwrap();
        let recovered = DomainBatch::decode(&encoded).unwrap();
        assert_eq!(recovered.schema_version, SCHEMA_VERSION);
        assert_eq!(batch.tracks[0].id, recovered.tracks[0].id);
        assert_eq!(
            batch.utterance_revisions[2].text,
            recovered.utterance_revisions[2].text
        );
    }

    #[test]
    fn derives_saved_audio_only_from_chunk_commit_state() {
        let mut batch = DomainBatch::decode(FIXTURE).unwrap();
        batch.sessions[0].ai_state = AiState::Completed;
        batch.sessions[0].sync_state = SyncState::Idle;
        assert_eq!(
            batch
                .audio_chunks
                .iter()
                .map(AudioChunk::is_saved)
                .collect::<Vec<_>>(),
            [true, true, false, false]
        );
        batch.sessions[0].ai_state = AiState::Failed;
        batch.sessions[0].sync_state = SyncState::Syncing;
        assert_eq!(
            batch
                .audio_chunks
                .iter()
                .map(AudioChunk::is_saved)
                .collect::<Vec<_>>(),
            [true, true, false, false]
        );
        batch.audio_chunks[3].byte_size = Count::try_from(64).unwrap();
        batch.audio_chunks[3].sha256 = batch.audio_chunks[0].sha256.clone();
        assert!(!batch.audio_chunks[3].is_saved());
    }

    #[test]
    fn keeps_manual_history_and_derives_translation_staleness() {
        let batch = DomainBatch::decode(FIXTURE).unwrap();
        assert!(!batch.utterance_revisions[0].is_durable());
        let manual = &batch.utterance_revisions[2];
        let concurrent = &batch.utterance_revisions[4];
        assert_eq!(manual.parent_revision_ids, concurrent.parent_revision_ids);
        assert_ne!(manual.id, concurrent.id);
        let selected = &batch.utterance_selections[0].revision_id;
        assert_eq!(selected, &manual.id);
        let translation = &batch.translation_revisions[1];
        assert!(translation.is_stale(selected));
        assert!(!translation.is_stale(&batch.utterance_revisions[1].id));
        assert!(
            !translation
                .authorship
                .allows_replacement_by(Authorship::Machine)
        );
    }
}
