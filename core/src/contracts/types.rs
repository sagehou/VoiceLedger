use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::track::{TrackCommand, TrackState};
use crate::{SessionCommand, SessionState};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct StableId(String);

impl TryFrom<String> for StableId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let bytes = value.as_bytes();
        if bytes.len() == 36
            && bytes[14] == b'4'
            && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
            && bytes.iter().copied().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    is_lower_hex(byte)
                }
            })
        {
            Ok(Self(value))
        } else {
            Err("expected a canonical UUIDv4")
        }
    }
}

impl From<StableId> for String {
    fn from(id: StableId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256(String);

impl TryFrom<String> for Sha256 {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() == 64 && value.bytes().all(is_lower_hex) {
            Ok(Self(value))
        } else {
            Err("expected a lowercase SHA-256 digest")
        }
    }
}

impl From<Sha256> for String {
    fn from(hash: Sha256) -> Self {
        hash.0
    }
}

fn is_lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Count(i64);

impl TryFrom<i64> for Count {
    type Error = &'static str;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value >= 0 {
            Ok(Self(value))
        } else {
            Err("expected a nonnegative signed 64-bit integer")
        }
    }
}

impl From<Count> for i64 {
    fn from(value: Count) -> Self {
        value.0
    }
}

impl Count {
    pub fn get(self) -> u64 {
        self.0 as u64
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start_ns: Count,
    pub end_ns: Count,
}

impl TimeRange {
    pub(crate) fn is_nonempty(self) -> bool {
        self.start_ns < self.end_ns
    }

    pub(crate) fn contains(self, other: Self) -> bool {
        self.start_ns <= other.start_ns && other.end_ns <= self.end_ns
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Mic,
    SystemLoopback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Zh,
    En,
    Ja,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguagePolicy {
    Auto,
    Zh,
    En,
    Ja,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCommitState {
    Pending,
    Committed,
    Missing,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiState {
    Disabled,
    Pending,
    Running,
    Deferred,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    NotConfigured,
    Configured,
    Idle,
    Syncing,
    Offline,
    WaitingKey,
    AuthenticationRequired,
    ProviderUnsupported,
    QuotaExceeded,
    ConflictNeedsReview,
    RetryScheduled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionReason {
    UserRequest,
    SourceReady,
    CaptureDrained,
    EndpointLost,
    PermissionRevoked,
    AudioRouteChanged,
    WriterFailure,
    StorageExhausted,
    ProcessDeath,
    SystemSuspend,
    PlatformError,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionChange {
    pub command: SessionCommand,
    pub at_ns: Count,
    pub reason: TransitionReason,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub id: StableId,
    pub title: String,
    pub device_id: StableId,
    pub created_at_unix_ms: Count,
    pub started_at_unix_ms: Option<Count>,
    pub ended_at_unix_ms: Option<Count>,
    pub language_policy: LanguagePolicy,
    pub translation_target: Option<Language>,
    pub capture_state: SessionState,
    pub ai_state: AiState,
    pub sync_state: SyncState,
    pub transitions: Vec<SessionChange>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackChange {
    pub command: TrackCommand,
    pub at_ns: Count,
    pub reason: TransitionReason,
    pub confirmed_endpoint_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioFormat {
    pub sample_rate_hz: NonZeroU32,
    pub channels: NonZeroU32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackRecord {
    pub id: StableId,
    pub session_id: StableId,
    pub source: Source,
    pub endpoint_id: String,
    pub format: AudioFormat,
    pub source_epoch: u32,
    pub capture_state: TrackState,
    pub transitions: Vec<TrackChange>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioChunk {
    pub id: StableId,
    pub track_id: StableId,
    pub source_epoch: u32,
    pub sequence: Count,
    pub range: TimeRange,
    pub codec: String,
    pub format: AudioFormat,
    pub byte_size: Count,
    pub sha256: Option<Sha256>,
    pub commit_state: AudioCommitState,
}

impl AudioChunk {
    pub fn is_saved(&self) -> bool {
        self.commit_state == AudioCommitState::Committed
            && self.sha256.is_some()
            && self.byte_size.get() > 0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapCause {
    Paused,
    EndpointLost,
    FormatChanged,
    AudioRouteChanged,
    PermissionRevoked,
    WriterFailure,
    StorageExhausted,
    QueueOverflow,
    ProcessDeath,
    SystemSuspend,
    UnknownCapture,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gap {
    pub id: StableId,
    pub track_id: StableId,
    pub source_epoch: u32,
    pub range: TimeRange,
    pub cause: GapCause,
    pub recoverable: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Silence {
    pub id: StableId,
    pub track_id: StableId,
    pub source_epoch: u32,
    pub range: TimeRange,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkSpan {
    pub chunk_id: StableId,
    pub range: TimeRange,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub model_id: String,
    pub model_version: String,
    pub runtime_version: String,
    pub sha256: Sha256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UtteranceKind {
    Partial,
    Final,
    Manual,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtteranceRevision {
    pub id: StableId,
    pub utterance_id: StableId,
    pub revision: NonZeroU32,
    pub parent_revision_ids: Vec<StableId>,
    pub track_id: StableId,
    pub source: Vec<ChunkSpan>,
    pub range: TimeRange,
    pub kind: UtteranceKind,
    pub language: Language,
    pub text: String,
    pub confidence: Option<f64>,
    pub model: Option<ModelRef>,
    pub created_at_unix_ms: Count,
}

impl UtteranceRevision {
    pub fn is_durable(&self) -> bool {
        self.kind != UtteranceKind::Partial
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UtteranceSelection {
    pub utterance_id: StableId,
    pub revision_id: StableId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorship {
    Machine,
    Manual,
}

impl Authorship {
    pub fn allows_replacement_by(self, proposal: Self) -> bool {
        self == Self::Machine || proposal == Self::Manual
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationRevision {
    pub id: StableId,
    pub translation_id: StableId,
    pub revision: NonZeroU32,
    pub source_revision_id: StableId,
    pub target_language: Language,
    pub text: String,
    pub authorship: Authorship,
    pub model: Option<ModelRef>,
    pub created_at_unix_ms: Count,
}

impl TranslationRevision {
    pub fn is_stale(&self, selected_source_revision_id: &StableId) -> bool {
        &self.source_revision_id != selected_source_revision_id
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerAssignment {
    pub id: StableId,
    pub session_id: StableId,
    pub track_id: StableId,
    pub speaker_id: Option<StableId>,
    pub range: TimeRange,
    pub revision: NonZeroU32,
    pub authorship: Authorship,
    pub confidence: Option<f64>,
    pub model: Option<ModelRef>,
    pub confirmed_name: Option<String>,
    pub confirmed_profile_id: Option<StableId>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakerProfile {
    pub id: StableId,
    pub display_name: String,
    pub consented_at_unix_ms: Count,
    pub encrypted_sample_refs: Vec<StableId>,
    pub encrypted_embedding_refs: Vec<StableId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiJobState {
    Queued,
    Running,
    Deferred,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiJobKind {
    Asr,
    Translation,
    Speaker,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AiInput {
    Audio { span: ChunkSpan },
    UtteranceRevision { revision_id: StableId },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AiJob {
    pub id: StableId,
    pub session_id: StableId,
    pub kind: AiJobKind,
    pub inputs: Vec<AiInput>,
    pub model: ModelRef,
    pub state: AiJobState,
    pub attempts: u32,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Windows,
    Android,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCapabilityProfile {
    pub id: StableId,
    pub device_id: StableId,
    pub observed_at_unix_ms: Count,
    pub platform: Platform,
    pub os_version: String,
    pub abi: String,
    pub logical_cpu_count: Option<NonZeroU32>,
    pub cpu_features: Vec<String>,
    pub total_memory_bytes: Option<Count>,
    pub available_memory_bytes: Option<Count>,
    pub low_memory_threshold_bytes: Option<Count>,
    pub process_memory_bytes: Option<Count>,
    pub low_memory: Option<bool>,
    pub low_ram_device: Option<bool>,
    pub storage_available_bytes: Option<Count>,
    pub thermal_state: Option<String>,
    pub power_saver: Option<bool>,
    pub battery_percent: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkStage {
    Estimate,
    Short,
    Sustained,
    Extended,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkResult {
    pub id: StableId,
    pub device_profile_id: StableId,
    pub model: ModelRef,
    pub stage: BenchmarkStage,
    pub observed_at_unix_ms: Count,
    pub corpus_sha256: Option<Sha256>,
    pub duration_ms: Option<Count>,
    pub cold_start_ms: Option<Count>,
    pub warm_rtf: Option<f64>,
    pub peak_memory_bytes: Option<Count>,
    pub final_p95_ms: Option<Count>,
    pub threads: Option<NonZeroU32>,
    pub thermal_note: Option<String>,
    pub battery_note: Option<String>,
    pub evidence_ref: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncEntity {
    Session,
    Track,
    AudioChunk,
    Gap,
    Silence,
    UtteranceRevision,
    TranslationRevision,
    SpeakerAssignment,
    SpeakerProfile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncOperation {
    Upsert,
    Tombstone,
    Restore,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyncEvent {
    pub id: StableId,
    pub device_id: StableId,
    pub sequence: Count,
    pub entity_kind: SyncEntity,
    pub entity_id: StableId,
    pub operation: SyncOperation,
    pub revision_id: StableId,
    pub object_refs: Vec<StableId>,
    pub created_at_unix_ms: Count,
}
