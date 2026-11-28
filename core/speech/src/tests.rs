use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

use rusqlite::Connection;
use voiceledger_core::contracts::{
    Authorship, ChunkSpan, Language, TimeRange, UtteranceKind, UtteranceRevision,
};

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
        model: &model_ref(model).unwrap(),
        language: LanguagePolicy::Auto,
        job: None,
        translate: None,
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
fn external_transcription_sends_one_saved_track_and_keeps_manual_text_selected() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use voiceledger_audio::Store;

    let root = Directory::new();
    let audio = Store::open(&root.0).unwrap();
    let send = |value, samples: &[f32]| {
        audio
            .execute_json(&serde_json::to_vec(&value).unwrap(), samples)
            .unwrap();
    };
    let session = id(1);
    let mic = id(2);
    let loopback = id(3);
    let end_ns = 240_000_000_i64;
    let samples: Vec<f32> = (0..11_520)
        .map(|index| {
            let phase = index as f32 * std::f32::consts::TAU * 220.0 / 48_000.0;
            0.4 * phase.sin() + 0.1 * (3.0 * phase).sin() + 0.05 * (6.0 * phase).sin()
        })
        .collect();
    send(
        json!({"op":"create_session","id":session,"created_at_ms":1,"clock":"synthetic","clock_origin":0,
            "tracks":[{"id":mic,"source":"mic","endpoint_id":"synthetic"},
                      {"id":loopback,"source":"system_loopback","endpoint_id":"synthetic"}]}),
        &[],
    );
    for (track, chunk) in [(&mic, id(4)), (&loopback, id(5))] {
        send(
            json!({"op":"begin_stream","track_id":track,"epoch":0,"start_ns":0,"rate":48000,
                "channels":1,"sample_format":"pcm_f32le","native_format":"synthetic"}),
            &[],
        );
        send(
            json!({"op":"begin_chunk","id":chunk,"track_id":track,"epoch":0,"start_ns":0,"end_ns":end_ns}),
            &[],
        );
        send(
            json!({"op":"seal_chunk","id":chunk,"end_ns":end_ns,"packets":[
                {"start_ns":0,"end_ns":end_ns,"frames":samples.len(),"device_position_frames":0,"qpc100ns":0,
                "flags":0,"estimated_timestamp":false}]}),
            &samples,
        );
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let action = |approved, model_id: &str| {
        json!({"op":"transcribe_external","session_id":session,"track_id":mic,"language":"en","approved":approved,
            "settings":{"provider":"custom","base_url":base_url,"model_id":model_id,"api_key":"fixture"}})
    };
    let request = |action| {
        let bytes = serde_json::to_vec(
            &json!({"action":action,"recordings":root.0,"profiles":root.0,"adapters":root.0}),
        )
        .unwrap();
        Request::decode(&bytes)
            .unwrap()
            .run(&|| false, &|| None, |_| {})
    };
    assert!(matches!(
        request(action(false, "fixture-first")),
        Err(Error::Blocked(Block::ApprovalRequired))
    ));
    assert!(matches!(
        request(action(true, "fixture-first")),
        Err(Error::Blocked(Block::RecordingActive))
    ));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert!(!root.0.join(transcript::PATH).exists());

    send(
        json!({"op":"session_state","id":session,"state":6,"elapsed_ns":end_ns,"snapshot":{}}),
        &[],
    );
    let server = thread::spawn(move || {
        for (model, status, text) in [
            ("fixture-first", "200 OK", r#"{"text":"initial draft"}"#),
            (
                "fixture-second",
                "307 Temporary Redirect",
                "provider diagnostic",
            ),
            (
                "fixture-second",
                "429 Too Many Requests",
                "provider diagnostic",
            ),
            ("fixture-second", "200 OK", r#"{"text":"external draft"}"#),
        ] {
            let deadline = Instant::now() + Duration::from_secs(12);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("fixture did not receive its expected request: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(4)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "POST /v1/audio/transcriptions HTTP/1.1\r\n");
            let mut length = 0;
            let mut authorized = false;
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                if name.eq_ignore_ascii_case("authorization") {
                    authorized = value.trim() == "Bearer fixture";
                }
            }
            assert!(authorized && (45..=16_384).contains(&length));
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let model_field = format!("name=\"model\"\r\n\r\n{model}\r\n");
            assert!(
                body.windows(model_field.len())
                    .any(|part| part == model_field.as_bytes())
            );
            let language = b"name=\"language\"\r\n\r\nen\r\n";
            assert!(body.windows(language.len()).any(|part| part == language));
            let start = body.windows(4).position(|part| part == b"RIFF").unwrap();
            let wave = &body[start..];
            assert_eq!(&wave[8..12], b"WAVE");
            assert_eq!(&wave[22..24], &1u16.to_le_bytes());
            assert_eq!(&wave[24..28], &16_000u32.to_le_bytes());
            assert_eq!(&wave[34..36], &16u16.to_le_bytes());
            let data_bytes = u32::from_le_bytes(wave[40..44].try_into().unwrap());
            assert!((640..=7_680).contains(&data_bytes));
            write!(
                socket,
                "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nLocation: /other-endpoint\r\nContent-Length: {}\r\n\r\n{text}",
                text.len()
            )
            .unwrap();
        }
    });

    assert_eq!(
        request(action(true, "fixture-first")).unwrap(),
        br#"{"saved":1}"#
    );
    let inspect = || {
        let bytes = request(json!({"op":"inspect","session_id":session})).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };
    let report = inspect();
    assert_eq!(
        report["external_transcription_providers"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let line = &report["lines"][0];
    assert_eq!(line["text"], "initial draft");
    assert_eq!(line["source"], "mic");
    request(json!({
        "op":"save_utterance","session_id":session,"utterance_id":line["utterance_id"],
        "expected_revision_id":line["revision_id"],"text":"manual edit",
    }))
    .unwrap();
    let rerun = action(true, "fixture-second");
    assert!(matches!(
        request(rerun.clone()),
        Err(Error::ExternalResponse)
    ));
    assert!(matches!(
        request(rerun.clone()),
        Err(Error::ExternalRateLimit)
    ));
    assert_eq!(request(rerun.clone()).unwrap(), br#"{"saved":1}"#);
    server.join().unwrap();
    assert_eq!(request(rerun).unwrap(), br#"{"saved":0}"#);
    let report = inspect();
    assert_eq!(report["lines"].as_array().unwrap().len(), 1);
    let line = &report["lines"][0];
    assert_eq!(line["text"], "manual edit");
    assert_eq!(line["external_draft"], "external draft");
}

#[test]
fn session_note_pages_and_summary_cover_the_saved_transcript() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use voiceledger_audio::Store;

    let root = Directory::new();
    let audio = Store::open(&root.0).unwrap();
    let session = id(1);
    let mic = id(2);
    let loopback = id(3);
    audio.execute_json(&serde_json::to_vec(&json!({
        "op":"create_session","id":session,"created_at_ms":1,"clock":"synthetic","clock_origin":0,
        "tracks":[{"id":mic,"source":"mic","endpoint_id":"fixture"},
                  {"id":loopback,"source":"system_loopback","endpoint_id":"fixture"}]
    })).unwrap(), &[]).unwrap();
    let store = Transcripts::open(&root.0).unwrap();
    let catalog = Catalog::bundled().unwrap();
    let model = model_ref(&catalog.models()[0]).unwrap();
    let mut writer = Writer {
        transcripts: &store,
        session: &session,
        model: &model,
        language: LanguagePolicy::En,
        job: None,
        translate: None,
        publish_partial: &mut |_| {},
        saved: 0,
    };
    for index in 0..85 {
        let start = i64::from(index / 2) * 1_000_000_000;
        let range = TimeRange {
            start_ns: start.try_into().unwrap(),
            end_ns: (start + 100_000_000).try_into().unwrap(),
        };
        writer
            .finish(Final {
                id: id(index + 100),
                track_id: if index % 2 == 0 {
                    mic.clone()
                } else {
                    loopback.clone()
                },
                range,
                spans: vec![ChunkSpan {
                    chunk_id: id(4),
                    range,
                }],
                recognition: Recognition {
                    language: Language::En,
                    text: format!("Item {index:03} {}", "fixture ".repeat(46)),
                },
            })
            .unwrap();
    }
    drop(store);
    let request = |action| {
        let input =
            json!({"action":action,"recordings":root.0,"profiles":root.0,"adapters":root.0});
        Request::decode(&serde_json::to_vec(&input).unwrap())
            .unwrap()
            .run(&|| false, &|| None, |_| {})
    };
    let read = |action| serde_json::from_slice::<Value>(&request(action).unwrap()).unwrap();
    let note = || read(json!({"op":"inspect","session_id":session}));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let generate = |approved| {
        json!({
            "op":"summarize","session_id":session,"approved":approved,
            "settings":{"provider":"custom","base_url":base_url,"model_id":"fixture-chat"}
        })
    };
    assert!(matches!(
        request(generate(false)),
        Err(Error::Blocked(Block::ApprovalRequired))
    ));
    assert!(matches!(
        request(generate(true)),
        Err(Error::Blocked(Block::RecordingActive))
    ));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    audio.execute_json(&serde_json::to_vec(&json!({
        "op":"session_state","id":session,"state":6,"elapsed_ns":43_000_000_000_i64,"snapshot":{}
    })).unwrap(), &[]).unwrap();

    let mut cursor = Value::Null;
    let mut count = 0;
    for page_count in [40, 40, 5] {
        let page = read(json!({"op":"transcript_page","session_id":session,"after":cursor}));
        let lines = page["lines"].as_array().unwrap();
        assert_eq!(lines.len(), page_count);
        for line in lines {
            assert!(
                line["text"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("Item {count:03} "))
            );
            count += 1;
        }
        cursor = page["next"].clone();
    }
    assert!(cursor.is_null());
    let filtered = read(json!({"op":"transcript_page","session_id":session,"track_id":loopback}));
    let filtered_tail = read(
        json!({"op":"transcript_page","session_id":session,"track_id":loopback,"after":filtered["next"]}),
    );
    assert_eq!(filtered_tail["lines"].as_array().unwrap().len(), 2);
    assert!(
        filtered_tail["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["track_id"] == json!(loopback))
    );

    let server = thread::spawn(move || {
        for round in 0..2 {
            for stage in 0..3 {
                let deadline = Instant::now() + Duration::from_secs(12);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => {
                            panic!("summary fixture did not receive the expected request: {error}")
                        }
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(4)))
                    .unwrap();
                let mut input = BufReader::new(&mut socket);
                let mut line = String::new();
                input.read_line(&mut line).unwrap();
                assert_eq!(line, "POST /v1/chat/completions HTTP/1.1\r\n");
                let mut length = 0;
                loop {
                    line.clear();
                    assert_ne!(input.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
                assert!((1..32 * 1024).contains(&length));
                let mut body = vec![0; length];
                input.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["model"], "fixture-chat");
                assert_eq!(request["messages"][1]["role"], "user");
                assert_eq!(request["stream"], false);
                assert!(request.get("tools").is_none());
                let text = request["messages"][1]["content"].as_str().unwrap();
                let reply = match stage {
                    0 => {
                        assert!(text.starts_with(if round == 0 {
                            "Item 000 "
                        } else {
                            "Corrected opening"
                        }));
                        "Start of meeting"
                    }
                    1 => {
                        assert!(text.contains("Item 084 "));
                        "End of meeting"
                    }
                    _ => {
                        assert_eq!(text, "Start of meeting\nEnd of meeting");
                        if round == 0 {
                            "Initial summary"
                        } else {
                            "Updated summary"
                        }
                    }
                };
                let response =
                    json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]})
                        .to_string();
                write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{response}",
                    response.len()
                )
                .unwrap();
            }
        }
    });
    request(generate(true)).unwrap();
    let initial = note();
    assert_eq!(initial["summary"]["current"]["text"], "Initial summary");
    assert_eq!(initial["summary"]["current"]["generated"], true);
    let edit = |expected: &Value, draft: &Value, text: &str| json!({"op":"save_summary","session_id":session,"expected_revision_id":expected,"draft_id":draft,"text":text});
    request(edit(
        &initial["summary"]["current"]["id"],
        &Value::Null,
        "Manual summary",
    ))
    .unwrap();
    assert!(matches!(
        request(edit(
            &initial["summary"]["current"]["id"],
            &Value::Null,
            "Competing edit"
        )),
        Err(Error::RevisionConflict)
    ));
    let line = &initial["lines"][0];
    request(
        json!({"op":"save_utterance","session_id":session,"utterance_id":line["utterance_id"],
        "expected_revision_id":line["revision_id"],"text":"Corrected opening"}),
    )
    .unwrap();
    assert_eq!(note()["summary"]["current"]["stale"], true);
    request(generate(true)).unwrap();
    server.join().unwrap();
    let saved = note();
    assert_eq!(saved["summary"]["current"]["text"], "Manual summary");
    assert_eq!(saved["summary"]["draft"]["text"], "Updated summary");
    assert_eq!(saved["summary"]["draft"]["stale"], false);
    request(edit(
        &saved["summary"]["current"]["id"],
        &saved["summary"]["draft"]["id"],
        "Reviewed summary",
    ))
    .unwrap();
    let exported = read(json!({"op":"export","session_id":session,"format":"json"}));
    assert_eq!(exported["summary"]["text"], "Reviewed summary");
    assert_eq!(exported["summary"]["generated"], false);
    assert_eq!(exported["lines"].as_array().unwrap().len(), 85);
    assert!(note()["summary"]["draft"].is_null());
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
            model: &model_ref(model).unwrap(),
            language: LanguagePolicy::Auto,
            job: Some(&asr),
            translate: None,
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
             CREATE TABLE revisions(id TEXT PRIMARY KEY);
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
        5
    );
}

#[test]
fn translation_versions_keep_manual_edits_and_bind_to_the_selected_source() {
    use rusqlite::params;

    let root = Directory::new();
    let catalog = Catalog::bundled().unwrap();
    let model = &catalog.models()[0];
    let translator = &catalog.models()[2];
    let session = id(1);
    let track = id(2);
    let range = TimeRange {
        start_ns: 0_i64.try_into().unwrap(),
        end_ns: 40_i64.try_into().unwrap(),
    };
    let store = Transcripts::open(&root.0).unwrap();
    let mut writer = Writer {
        transcripts: &store,
        session: &session,
        model: &model_ref(model).unwrap(),
        language: LanguagePolicy::En,
        job: None,
        translate: None,
        publish_partial: &mut |_| {},
        saved: 0,
    };
    writer
        .finish(Final {
            id: id(3),
            track_id: track,
            spans: vec![ChunkSpan {
                chunk_id: id(4),
                range,
            }],
            range,
            recognition: Recognition {
                language: Language::En,
                text: "Save the recording.".into(),
            },
        })
        .unwrap();
    let mut source = store
        .translation_sources(&session, "")
        .unwrap()
        .pop()
        .unwrap();
    let model_ref = ModelRef {
        model_id: translator.id.clone(),
        model_version: translator.version.clone(),
        runtime_version: translator.runtime.version.clone(),
        sha256: translator.artifacts[0].sha256.clone(),
    };
    assert!(
        store
            .save_machine_translation(&session, &source, "保存录音。".into(), model_ref.clone())
            .unwrap()
    );
    assert!(
        !store
            .save_machine_translation(&session, &source, "保存录音。".into(), model_ref.clone())
            .unwrap()
    );
    let selected = || translations::selected(&store.db, &id(3)).unwrap().unwrap();
    let machine = selected();
    assert_eq!(machine.source_revision_id, source.id);
    assert_eq!(machine.authorship, Authorship::Machine);
    assert_eq!(
        machine.model.unwrap().sha256,
        translator.artifacts[0].sha256
    );
    let edit = |source_revision_id, expected_translation_id, text: &str| translations::ManualEdit {
        session_id: session.clone(),
        utterance_id: id(3),
        source_revision_id,
        expected_translation_id,
        text: text.into(),
    };
    store
        .save_manual_translation(edit(
            source.id.clone(),
            Some(machine.id.clone()),
            "请保存录音。",
        ))
        .unwrap();
    let manual = selected();
    assert_eq!(manual.parent_revision_ids, vec![machine.id]);
    assert_eq!(manual.translation_id, machine.translation_id);
    assert_eq!(manual.authorship, Authorship::Manual);
    assert!(matches!(
        store.save_manual_translation(edit(source.id.clone(), None, "旧编辑")),
        Err(Error::RevisionConflict)
    ));

    let original = std::mem::replace(&mut source.id, id(5));
    source.revision = 2.try_into().unwrap();
    source.kind = UtteranceKind::Manual;
    source.model = None;
    source.parent_revision_ids = vec![original.clone()];
    source.text = "Save the original recording.".into();
    store
        .db
        .execute(
            "INSERT INTO revisions (id,utterance_id,revision,payload) VALUES (?1,?2,2,?3)",
            params![
                String::from(source.id.clone()),
                String::from(id(3)),
                serde_json::to_string(&source).unwrap()
            ],
        )
        .unwrap();
    store
        .db
        .execute(
            "UPDATE utterances SET selected_revision=?1 WHERE id=?2",
            params![String::from(source.id.clone()), String::from(id(3))],
        )
        .unwrap();
    assert!(selected().is_stale(&source.id));
    assert!(matches!(
        store.save_manual_translation(edit(original, Some(manual.id.clone()), "旧原文")),
        Err(Error::RevisionConflict)
    ));
    assert!(
        store
            .save_machine_translation(&session, &source, "保存原始录音。".into(), model_ref)
            .unwrap()
    );
    assert_eq!(selected().id, manual.id);
    assert_eq!(selected().text, "请保存录音。");
    store
        .save_manual_translation(edit(
            source.id.clone(),
            Some(manual.id.clone()),
            "请保存原始录音。",
        ))
        .unwrap();
    assert!(!selected().is_stale(&source.id));
    drop(store);
    let reopened = Transcripts::open(&root.0).unwrap();
    assert_eq!(
        translations::selected(&reopened.db, &id(3))
            .unwrap()
            .unwrap()
            .text,
        "请保存原始录音。"
    );
}

#[test]
fn translation_requires_explicit_approval() {
    let root = Directory::new();
    let request = Request::decode(&serde_json::to_vec(&serde_json::json!({
        "recordings": root.0, "profiles": root.0, "adapters": root.0,
        "action": {"op": "translate", "model_id": "opus-mt-en-zh-int8", "session_id": id(1), "approved": false}
    })).unwrap()).unwrap();
    assert!(matches!(
        request.run(&|| false, &|| None, |_| {}),
        Err(Error::Blocked(Block::ApprovalRequired))
    ));
    assert!(!root.0.join("models").exists());
    assert!(!root.0.join("speech-worker.lock").exists());
}

#[test]
#[ignore = "requires the pinned model and native adapter from GitHub Actions"]
fn native_translation_app_flow() {
    use serde_json::json;
    use voiceledger_audio::Store;

    let root = Directory::new();
    let catalog = Catalog::bundled().unwrap();
    let model = &catalog.models()[2];
    let session = id(1);
    let track = id(2);
    let chunk = id(3);
    let utterance = id(4);
    let directory = model_dir(&root.0.join("models"), model);
    fs::create_dir_all(&directory).unwrap();
    let weights = PathBuf::from(std::env::var_os("VOICELEDGER_TEST_TRANSLATION_MODEL").unwrap());
    let adapters =
        PathBuf::from(std::env::var_os("VOICELEDGER_TEST_TRANSLATION_ADAPTERS").unwrap());
    for artifact in &model.artifacts {
        fs::copy(weights.join(&artifact.file), directory.join(&artifact.file)).unwrap();
    }
    device_profile::save(
        &root.0,
        &serde_json::to_vec(&json!({
            "id": id(5), "device_id": id(6), "observed_at_unix_ms": 1,
            "platform":"windows", "os_version":"10.0.26100", "logical_cpu_count":2,
            "process_memory_bytes":128_000_000, "available_memory_bytes":4_000_000_000_u64,
            "storage_available_bytes":2_000_000_000_u64, "low_memory":false
        }))
        .unwrap(),
    )
    .unwrap();

    let audio = Store::open(&root.0).unwrap();
    let send = |value, samples: &[f32]| {
        audio
            .execute_json(&serde_json::to_vec(&value).unwrap(), samples)
            .unwrap();
    };
    let end_ns = 20_000_000i64;
    send(
        json!({"op":"create_session","id":session,"created_at_ms":1,"clock":"synthetic","clock_origin":0,
                "tracks":[{"id":track,"source":"mic","endpoint_id":"synthetic"}]}),
        &[],
    );
    send(
        json!({"op":"begin_stream","track_id":track,"epoch":0,"start_ns":0,"rate":48000,
                "channels":1,"sample_format":"pcm_f32le","native_format":"synthetic"}),
        &[],
    );
    send(
        json!({"op":"begin_chunk","id":chunk,"track_id":track,"epoch":0,"start_ns":0,"end_ns":end_ns}),
        &[],
    );
    send(
        json!({"op":"seal_chunk","id":chunk,"end_ns":end_ns,"packets":[
                {"start_ns":0,"end_ns":end_ns,"frames":960,"device_position_frames":0,"qpc100ns":0,
                "flags":0,"estimated_timestamp":false}]}),
        &[0.25; 960],
    );
    send(
        json!({"op":"session_state","id":session,"state":6,"elapsed_ns":end_ns,"snapshot":{}}),
        &[],
    );
    let store = Transcripts::open(&root.0).unwrap();
    let range = TimeRange {
        start_ns: 0i64.try_into().unwrap(),
        end_ns: end_ns.try_into().unwrap(),
    };
    Writer {
        transcripts: &store,
        session: &session,
        model: &model_ref(&catalog.models()[0]).unwrap(),
        language: LanguagePolicy::En,
        job: None,
        translate: None,
        publish_partial: &mut |_| {},
        saved: 0,
    }
    .finish(Final {
        id: utterance.clone(),
        track_id: track,
        spans: vec![ChunkSpan {
            chunk_id: chunk,
            range,
        }],
        range,
        recognition: Recognition {
            language: Language::En,
            text: "Save the recording.".into(),
        },
    })
    .unwrap();
    let request = |action| {
        let value =
            json!({"action":action,"recordings":root.0,"profiles":root.0,"adapters":adapters});
        Request::decode(&serde_json::to_vec(&value).unwrap())
            .unwrap()
            .run(&|| false, &|| None, |_| {})
    };
    let inspect = || {
        serde_json::from_slice::<serde_json::Value>(&request(json!({"op":"inspect"})).unwrap())
            .unwrap()
    };
    assert!(inspect()["lines"][0]["translation"].is_null());
    let translate =
        json!({"op":"translate","model_id":model.id,"session_id":session,"approved":true});
    assert_eq!(request(translate.clone()).unwrap(), br#"{"saved":1}"#);
    let report = inspect();
    let line = &report["lines"][0];
    assert_eq!(line["translation"]["authorship"], "machine");
    assert_eq!(line["translation"]["stale"], false);
    request(json!({
        "op":"save_translation", "session_id":session, "utterance_id":utterance,
        "source_revision_id":line["revision_id"], "expected_translation_id":line["translation"]["revision_id"],
        "text":"请保存原始录音。"
    })).unwrap();
    assert_eq!(request(translate).unwrap(), br#"{"saved":0}"#);
    assert_eq!(
        inspect()["lines"][0]["translation"]["text"],
        "请保存原始录音。"
    );
    assert_eq!(inspect()["session"]["id"], json!(session));
    request(json!({"op":"rename_session","session_id":session,"text":"Synthetic weekly review"}))
        .unwrap();
    let speaker_edit = json!({
        "op": "save_speaker", "session_id": session, "utterance_id": utterance,
        "expected_revision_id": line["revision_id"], "expected_speaker_id": null, "text": "Synthetic host"
    });
    request(speaker_edit.clone()).unwrap();
    assert!(matches!(
        request(speaker_edit),
        Err(Error::RevisionConflict)
    ));
    request(json!({
        "op": "save_utterance", "session_id": session, "utterance_id": utterance,
        "expected_revision_id": line["revision_id"], "text": "Save the corrected recording."
    }))
    .unwrap();
    let updated = inspect();
    assert_eq!(updated["lines"][0]["text"], "Save the corrected recording.");
    assert_eq!(updated["lines"][0]["translation"]["stale"], true);
    assert_eq!(updated["lines"][0]["speaker"]["name"], "Synthetic host");
    let json_export = request(json!({"op":"export","session_id":session,"format":"json"})).unwrap();
    let json_export: serde_json::Value = serde_json::from_slice(&json_export).unwrap();
    assert_eq!(
        json_export["lines"][0]["text"],
        "Save the corrected recording."
    );
    assert_eq!(json_export["lines"][0]["translation"]["stale"], true);
    assert_eq!(json_export["title"], "Synthetic weekly review");
    assert_eq!(json_export["lines"][0]["speaker"]["name"], "Synthetic host");
    for format in ["text", "markdown"] {
        let bytes = request(json!({"op":"export","session_id":session,"format":format})).unwrap();
        let export = std::str::from_utf8(&bytes).unwrap();
        assert!(export.contains("Microphone") && export.contains("请保存原始录音。"));
    }
    for query in ["CORRECTED", "WEEKLY", "HOST"] {
        let matches = request(json!({"op":"history", "query":query})).unwrap();
        let sessions: serde_json::Value = serde_json::from_slice(&matches).unwrap();
        assert_eq!(sessions[0]["id"], json!(session));
        assert_eq!(sessions[0]["committed"], 1);
        assert_eq!(sessions[0]["has_transcript"], true);
    }
    assert_eq!(
        request(json!({"op":"history","query":"unmatched"}))
            .unwrap()
            .as_slice(),
        b"[]"
    );
}

#[test]
fn sync_import_preserves_actual_branches_and_recovers_after_device_removal() {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    use voiceledger_core::contracts::{DomainBatch, Source, SyncEntity, SyncEvent, SyncOperation};
    use voiceledger_core::sync::{
        Journal, ObjectKey, Page, PendingEvent, PutOutcome, SyncBackend, SyncError,
        SyncedTranscript, VaultSync,
    };
    use voiceledger_core::vault::{SealedObject, SyncScopes, Vault};

    #[derive(Default)]
    struct Storage(RefCell<BTreeMap<ObjectKey, Vec<u8>>>, Cell<bool>);
    impl SyncBackend for Storage {
        fn put_immutable(
            &self,
            key: &ObjectKey,
            object: &SealedObject,
        ) -> std::result::Result<PutOutcome, SyncError> {
            let mut data = self.0.borrow_mut();
            if data.contains_key(key) {
                return Ok(PutOutcome::Exists);
            }
            data.insert(key.clone(), object.envelope().to_vec());
            if key.value().ends_with("/retirement") && self.1.replace(false) {
                return Err(SyncError::Transport);
            }
            Ok(PutOutcome::Created)
        }
        fn get(&self, key: &ObjectKey) -> std::result::Result<Vec<u8>, SyncError> {
            self.0.borrow().get(key).cloned().ok_or(SyncError::NotFound)
        }
        fn list(
            &self,
            prefix: &ObjectKey,
            _: Option<&str>,
        ) -> std::result::Result<Page, SyncError> {
            let here = format!("{}/", prefix.value());
            let keys = self
                .0
                .borrow()
                .keys()
                .filter(|key| key.value().starts_with(&here))
                .cloned()
                .collect();
            Ok(Page {
                keys,
                continuation: None,
            })
        }
        fn delete(&self, _: &ObjectKey) -> std::result::Result<(), SyncError> {
            panic!("Text transfer and rotation retain encrypted history")
        }
    }

    let batch: DomainBatch =
        serde_json::from_str(include_str!("../../../contracts/fixtures/domain-v1.json")).unwrap();
    let session = batch.sessions[0].id.clone();
    let lines = batch.utterance_revisions;
    let entity = lines[1].utterance_id.clone();
    let remote = Storage::default();
    let vault = Vault::create().unwrap();
    let a_root = Directory::new();
    let b_root = Directory::new();
    let c_root = Directory::new();
    let a_text = Transcripts::open(&a_root.0).unwrap();
    let a_id = a_text.sync_device().unwrap();
    let b_text = Transcripts::open(&b_root.0).unwrap();
    let b_id = b_text.sync_device().unwrap();
    let wrap = |index: usize| SyncedTranscript {
        session_id: session.clone(),
        source: Source::Mic,
        revision: lines[index].clone(),
        session: None,
    };
    let stage = |sync: &mut VaultSync<'_>, text: &Transcripts, items: Vec<SyncedTranscript>| {
        sync.stage_text(items, || text.id().map_err(|_| SyncError::LocalStorage))
            .unwrap();
        sync.publish().unwrap();
    };
    let mut first = VaultSync::open(&a_root.0, &vault, &remote, a_id.clone()).unwrap();
    first.pull().unwrap();
    stage(&mut first, &a_text, vec![wrap(1), wrap(2)]);
    let mut second = VaultSync::open(&b_root.0, &vault, &remote, b_id.clone()).unwrap();
    second.pull().unwrap();
    // A second device's offline edit shares the same ancestor and ordinal.
    stage(&mut second, &b_text, vec![wrap(4)]);
    assert_eq!(b_text.import(second.journal()).unwrap(), 3);
    assert_eq!(b_text.import(second.journal()).unwrap(), 0);
    let sessions = crate::library::history(&b_root.0, None, None).unwrap();
    let sessions = serde_json::to_value(sessions).unwrap();
    assert_eq!(sessions[0]["id"], serde_json::json!(session));
    assert_eq!(sessions[0]["text_only"], true);
    assert!(!b_root.0.join("manifest.sqlite3").exists());
    let visible = crate::transcript::selected(&b_root.0, &session).unwrap();
    assert_eq!(visible[0].source, "mic");
    assert_eq!(visible[0].conflicts.len(), 2);
    let heads = [lines[2].id.clone(), lines[4].id.clone()];
    assert!(matches!(
        b_text.resolve_conflict(
            &session,
            &entity,
            &[heads[0].clone(), heads[0].clone()],
            "Stale"
        ),
        Err(Error::RevisionConflict)
    ));
    b_text
        .resolve_conflict(&session, &entity, &heads, "Reviewed synthetic notes")
        .unwrap();
    let outgoing = b_text.outgoing(&b_root.0).unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].revision.parent_revision_ids, heads);
    stage(&mut second, &b_text, outgoing);
    first.pull().unwrap();
    assert_eq!(a_text.import(first.journal()).unwrap(), 4);
    assert_eq!(
        crate::transcript::selected(&a_root.0, &session).unwrap()[0].text,
        "Reviewed synthetic notes"
    );

    let replacement = vault.rotate().unwrap();
    remote.1.set(true);
    assert_eq!(
        second.remove(&replacement, &a_id),
        Err(SyncError::Transport)
    );
    assert!(VaultSync::finished_rotation(&vault, &replacement, &remote, &a_id).unwrap());
    assert!(matches!(
        VaultSync::verify_key(&vault, &remote, &b_id),
        Err(SyncError::GenerationRetired)
    ));
    assert!(matches!(
        VaultSync::verify_key(&replacement, &remote, &a_id),
        Err(SyncError::DeviceRemoved)
    ));
    let clean_key = Vault::recover(&replacement.recovery_code()).unwrap();
    let clean = Transcripts::open(&c_root.0).unwrap();
    let mut third =
        VaultSync::open(&c_root.0, &clean_key, &remote, clean.sync_device().unwrap()).unwrap();
    third.pull().unwrap();
    assert_eq!(clean.import(third.journal()).unwrap(), 4);
    let restored = crate::transcript::selected(&c_root.0, &session).unwrap();
    assert_eq!(restored[0].text, "Reviewed synthetic notes");
    assert!(restored[0].conflicts.is_empty());
    assert_eq!(third.devices().unwrap().len(), 1);
    let tombstone = SyncEvent {
        id: clean.id().unwrap(),
        device_id: clean.id().unwrap(),
        sequence: 0.try_into().unwrap(),
        entity_kind: SyncEntity::Session,
        entity_id: session.clone(),
        operation: SyncOperation::Tombstone,
        restored_from_tombstone_id: None,
        revision_id: clean.id().unwrap(),
        object_refs: Vec::new(),
        created_at_unix_ms: 1.try_into().unwrap(),
    };
    let scopes = SyncScopes {
        enabled: true,
        ..SyncScopes::default()
    };
    let deletion = PendingEvent::seal(&clean_key, scopes, &tombstone).unwrap();
    deletion.publish(&remote).unwrap();
    let mut tombstones = Journal::new(&clean_key);
    tombstones
        .receive(&clean_key, &remote, deletion.key())
        .unwrap();
    clean.import(&tombstones).unwrap();
    clean.import(third.journal()).unwrap();
    assert!(
        crate::library::history(&c_root.0, None, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::transcript::selected(&c_root.0, &session)
            .unwrap()
            .is_empty()
    );
}
