use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

use rusqlite::Connection;
use voiceledger_core::contracts::{ChunkSpan, Language, TimeRange, UtteranceRevision};

use super::*;
use crate::jobs::{Cursor, Outcome};
use crate::pipeline::{Final, Output, Recognition};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static INDEX: AtomicU32 = AtomicU32::new(0);
        let id = INDEX.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("voiceledger-speech-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(number: u32) -> StableId {
    format!("00000000-0000-4000-8000-{number:012}")
        .try_into()
        .unwrap()
}

#[test]
fn retry_and_second_language_preserve_the_selected_durable_revision() {
    let root = Directory::new();
    let catalog = Catalog::bundled().unwrap();
    let model = &catalog.models()[0];
    let session = id(1);
    let track = id(2);
    let chunk = id(3);
    let range = TimeRange {
        start_ns: 8_i64.try_into().unwrap(),
        end_ns: 40_i64.try_into().unwrap(),
    };
    let store = Transcripts::open(&root.0).unwrap();
    let mut writer = Writer {
        transcripts: &store,
        session: &session,
        model,
        language: LanguagePolicy::Auto,
        job: None,
        publish_partial: &mut |_| {},
        saved: 0,
    };
    let mut persist = |language, text: &str| {
        writer.language = language;
        let provisional = writer.id().unwrap();
        writer
            .finish(Final {
                id: provisional,
                track_id: track.clone(),
                spans: vec![ChunkSpan {
                    chunk_id: chunk.clone(),
                    range,
                }],
                range,
                recognition: Recognition {
                    text: text.into(),
                    language: Language::Ja,
                },
            })
            .unwrap();
    };
    persist(LanguagePolicy::Auto, "synthetic first");
    persist(LanguagePolicy::Auto, "synthetic retry");
    persist(LanguagePolicy::Ja, "synthetic candidate");
    assert_eq!(writer.saved, 2);
    drop(store);
    let db = Connection::open(root.0.join("transcript-v1.sqlite3")).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM revisions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let row: String = db
        .query_row(
            "SELECT payload FROM revisions r JOIN utterances u ON u.selected_revision=r.id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let selected: UtteranceRevision = serde_json::from_str(&row).unwrap();
    assert_eq!(selected.text, "synthetic first");
    assert_eq!(selected.track_id, track);
    assert_eq!(selected.source[0].chunk_id, chunk);
    assert_eq!(
        selected.model.unwrap().runtime_version,
        model.runtime.version
    );
}

#[test]
fn unapproved_install_does_not_create_model_storage_or_network_operation() {
    let root = Directory::new();
    let request = Request::decode(&serde_json::to_vec(&serde_json::json!({
        "recordings": root.0, "profiles": root.0, "adapters": root.0,
        "action": {"op": "install", "model_id": "whisper-tiny-multilingual-q5-1", "approved": false}
    })).unwrap()).unwrap();
    assert!(matches!(
        request.run(&|| false, &|| None, |_| {}),
        Err(Error::Blocked(Block::ApprovalRequired))
    ));
    assert!(!root.0.join("models").exists());
}

#[test]
fn interrupted_jobs_resume_with_one_identity_and_atomic_final_progress() {
    let root = Directory::new();
    let catalog = Catalog::bundled().unwrap();
    let model = &catalog.models()[0];
    let session = id(1);
    let track = id(2);
    let source = id(3);
    let inputs = [voiceledger_audio::TrackInput {
        track_id: track.clone(),
        first_chunk_id: source.clone(),
        last_chunk_id: source.clone(),
        start_ns: 0,
        end_ns: 100,
    }];
    let spec = jobs::SourceJob::new(&inputs, model, LanguagePolicy::Auto);
    let asr = {
        let store = Transcripts::open(&root.0).unwrap();
        let queue = store.queue();
        let vad = queue.enqueue(id(4), &session, JobKind::Vad, &spec).unwrap();
        let asr = queue.enqueue(id(5), &session, JobKind::Asr, &spec).unwrap();
        queue
            .enqueue(id(6), &session, JobKind::Speaker, &spec)
            .unwrap();
        queue
            .enqueue(id(7), &session, JobKind::Translation, &spec)
            .unwrap();
        assert!(queue.begin(&[&vad, &asr]).unwrap());
        assert_eq!(queue.backlog().unwrap(), 2);
        let range = TimeRange {
            start_ns: 0_i64.try_into().unwrap(),
            end_ns: 40_i64.try_into().unwrap(),
        };
        let mut writer = Writer {
            transcripts: &store,
            session: &session,
            model,
            language: LanguagePolicy::Auto,
            job: Some(&asr),
            publish_partial: &mut |_| {},
            saved: 0,
        };
        writer
            .finish(Final {
                id: id(8),
                track_id: track.clone(),
                spans: vec![ChunkSpan {
                    chunk_id: source,
                    range,
                }],
                range,
                recognition: Recognition {
                    text: "synthetic final".into(),
                    language: Language::En,
                },
            })
            .unwrap();
        assert_eq!(
            queue.cursor(&asr).unwrap(),
            Some(Cursor {
                track: track.clone(),
                next_ns: 40
            })
        );
        asr
    };

    let store = Transcripts::open(&root.0).unwrap();
    let queue = store.queue();
    assert!(
        queue
            .status(&session, false)
            .unwrap()
            .unwrap()
            .reason
            .as_deref()
            == Some("worker_interrupted")
    );
    queue.recover().unwrap();
    let retry = queue.enqueue(id(9), &session, JobKind::Asr, &spec).unwrap();
    assert_eq!(retry, asr);
    assert_eq!(
        queue.cursor(&retry).unwrap(),
        Some(Cursor {
            track: track.clone(),
            next_ns: 40
        })
    );
    assert!(queue.begin(&[&retry]).unwrap());
    queue.checkpoint(&retry, &track, 20).unwrap();
    assert_eq!(queue.cursor(&retry).unwrap().unwrap().next_ns, 40);
    queue.finish(&[&retry], Outcome::Completed).unwrap();
    assert!(!queue.begin(&[&retry]).unwrap());
    let pending: String = queue.0.query_row(
        "SELECT group_concat(kind, ',') FROM (SELECT kind FROM speech_jobs WHERE state='queued' OR state='deferred' ORDER BY priority,kind)",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(pending, "vad,speaker,translation");
}

#[test]
fn migration_preserves_the_version_one_transcript_database() {
    let root = Directory::new();
    let path = root.0.join("transcript-v1.sqlite3");
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE utterances(id TEXT PRIMARY KEY);
             INSERT INTO utterances VALUES ('synthetic');
             PRAGMA user_version=1;",
        )
        .unwrap();
    }
    let migrated = Transcripts::open(&root.0).unwrap();
    let queue = migrated.queue();
    assert_eq!(queue.backlog().unwrap(), 0);
    assert_eq!(
        queue
            .0
            .query_row("SELECT id FROM utterances", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "synthetic"
    );
    assert_eq!(
        queue
            .0
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}
