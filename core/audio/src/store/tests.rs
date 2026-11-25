use super::*;
use std::process::{Command as Process, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const TRACK: &str = "00000000-0000-4000-8000-000000000002";
const FIRST: &str = "00000000-0000-4000-8000-000000000003";
const SECOND: &str = "00000000-0000-4000-8000-000000000004";
const END: i64 = 100_000_000;
const FRAMES: usize = 4800;
const ROOT_ENV: &str = "VOICELEDGER_AUDIO_CRASH_TEST_ROOT";
const BOUNDARY_ENV: &str = "VOICELEDGER_AUDIO_CRASH_TEST_BOUNDARY";
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "voiceledger-durability-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stable(value: &str) -> StableId {
    StableId::try_from(value.to_owned()).unwrap()
}

fn prepare(store: &Store) {
    store
        .execute(
            Command::CreateSession {
                id: stable(SESSION),
                created_at_ms: 1,
                clock: "test_monotonic".into(),
                clock_origin: 0,
                tracks: vec![NewTrack {
                    id: stable(TRACK),
                    source: Source::Mic,
                    endpoint_id: "synthetic".into(),
                }],
            },
            &[],
        )
        .unwrap();
    store
        .execute(
            Command::BeginStream {
                track_id: stable(TRACK),
                epoch: 0,
                start_ns: 0,
                rate: RATE as u32,
                channels: 1,
                sample_format: "pcm_f32le".into(),
                native_format: "synthetic".into(),
            },
            &[],
        )
        .unwrap();
}

fn begin(store: &Store, id: &str, start_ns: i64) {
    store
        .execute(
            Command::BeginChunk {
                id: stable(id),
                track_id: stable(TRACK),
                epoch: 0,
                start_ns,
                end_ns: start_ns + END,
            },
            &[],
        )
        .unwrap();
}

fn pcm() -> Vec<f32> {
    (0..FRAMES)
        .map(|index| 0.15 * (index as f32 * 0.06).sin())
        .collect()
}

fn packet(start_ns: i64) -> PacketMetadata {
    PacketMetadata {
        start_ns,
        end_ns: start_ns + END,
        frames: FRAMES as u32,
        device_position_frames: 123,
        qpc100ns: 456,
        flags: 0,
        estimated_timestamp: false,
    }
}

fn jobs(store: &Store) -> i64 {
    store
        .connection()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM processing_jobs", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn speech_reader_uses_committed_completed_audio_without_the_writer_lock() {
    let scratch = Scratch::new();
    let store = Store::open(&scratch.0).unwrap();
    prepare(&store);
    begin(&store, FIRST, 0);
    store
        .seal(FIRST, END, &[packet(0)], &pcm(), |_| {})
        .unwrap();
    begin(&store, SECOND, END);
    let reader = CommittedReader::open(&scratch.0).unwrap();
    let id = stable(SESSION);
    assert!(reader.recording().unwrap());
    assert!(reader.latest().unwrap().is_none());
    assert!(reader.next_chunk(&id, None).unwrap().is_none());
    let track = stable(TRACK);
    assert_eq!(reader.live_backlog(&track, 0).unwrap(), 1);
    assert_eq!(reader.session_tracks(&id).unwrap(), vec![track.clone()]);
    assert_eq!(
        reader.live_chunk(&id, &track, 0).unwrap().unwrap().id,
        stable(FIRST)
    );
    assert!(reader.live_chunk(&id, &track, END).unwrap().is_none());
    assert!(
        reader
            .live_chunk(&stable(SECOND), &track, 0)
            .unwrap()
            .is_none()
    );
    assert!(reader.tracks(&id).unwrap().is_empty());
    assert!(matches!(
        reader.playback(&id, &track, 0),
        Err(Error::Unavailable)
    ));

    store
        .execute(
            Command::SessionState {
                id: id.clone(),
                state: 6,
                elapsed_ns: END * 2,
                snapshot: serde_json::json!({}),
            },
            &[],
        )
        .unwrap();
    assert!(!reader.recording().unwrap());
    assert_eq!(reader.latest().unwrap().unwrap().id, id);
    let timeline = reader.timeline(&id, &track).unwrap();
    assert_eq!(timeline[0].kind, "audio");
    assert!(reader.timeline(&stable(SECOND), &track).is_err());
    let playback = reader
        .playback(&id, &track, END / 2)
        .unwrap()
        .into_bytes()
        .unwrap();
    assert_eq!(i64::from_le_bytes(playback[4..12].try_into().unwrap()), END);
    assert_eq!(playback.len(), 12 + FRAMES / 2 * 4);
    assert!(matches!(
        reader.playback(&id, &track, END),
        Err(Error::Unavailable)
    ));
    let archive = serde_json::to_value(reader.archive(&id, &track).unwrap()).unwrap();
    assert_eq!(archive["source"], "mic");
    assert_eq!(archive["chunks"].as_array().unwrap().len(), 1);
    let opus = reader.archive_chunk(&id, &track, &stable(FIRST)).unwrap();
    assert_eq!(archive["chunks"][0]["sha256"], digest(&opus));
    assert_eq!(archive["timeline"][0]["end_ns"], END);
    assert!(reader.archive_chunk(&id, &track, &stable(SECOND)).is_err());
    assert!(
        reader
            .archive_chunk(&stable(SECOND), &track, &stable(FIRST))
            .is_err()
    );
    let tracks = reader.inputs(&id).unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].first_chunk_id, stable(FIRST));
    assert_eq!(tracks[0].last_chunk_id, stable(FIRST));
    assert_eq!(
        reader
            .input_chunk(&id, &tracks[0], END / 2)
            .unwrap()
            .unwrap()
            .id,
        stable(FIRST)
    );
    assert!(reader.input_chunk(&id, &tracks[0], END).unwrap().is_none());
    let audio = reader.next_chunk(&id, None).unwrap().unwrap();
    assert_eq!(audio.id, stable(FIRST));
    assert_eq!(audio.track_id, stable(TRACK));
    assert_eq!((audio.epoch, audio.start_ns, audio.end_ns), (0, 0, END));
    assert_eq!(audio.samples.unwrap().len(), FRAMES);
    assert!(
        reader
            .next_chunk(&id, Some((&stable(TRACK), 0)))
            .unwrap()
            .is_none()
    );
    let first = store.chunk(FIRST).unwrap();
    fs::write(store.path(&first, "opus").unwrap(), [0_u8; 4]).unwrap();
    assert!(
        reader
            .next_chunk(&id, None)
            .unwrap()
            .unwrap()
            .samples
            .is_none()
    );
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os(ROOT_ENV) else {
        return;
    };
    let stop = std::env::var(BOUNDARY_ENV).unwrap();
    let store = Store::open(Path::new(&root)).unwrap();
    prepare(&store);
    begin(&store, FIRST, 0);
    store
        .seal(FIRST, END, &[packet(0)], &pcm(), |boundary| {
            if format!("{boundary:?}") == stop {
                // Exit without dropping the file handle or an in-flight SQLite transaction.
                std::process::exit(77);
            }
        })
        .unwrap();
    panic!("Unreached crash boundary");
}

#[test]
fn restart_reconciles_each_commit_boundary() {
    let boundaries = [
        Boundary::TempCreated,
        Boundary::Encoded,
        Boundary::Flushed,
        Boundary::Prepared,
        Boundary::Renamed,
        Boundary::BeforeCommit,
        Boundary::Committed,
    ];
    for boundary in boundaries {
        let scratch = Scratch::new();
        let status = Process::new(std::env::current_exe().unwrap())
            .args(["--exact", "store::tests::crash_child"])
            .env(ROOT_ENV, &scratch.0)
            .env(BOUNDARY_ENV, format!("{boundary:?}"))
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77), "{boundary:?}");
        let preserved = matches!(
            boundary,
            Boundary::Prepared | Boundary::Renamed | Boundary::BeforeCommit | Boundary::Committed
        );

        for restart in 0..2 {
            let store = Store::open(&scratch.0).unwrap();
            let chunk = store.chunk(FIRST).unwrap();
            assert_eq!(
                chunk.state,
                if preserved { "committed" } else { "missing" },
                "{boundary:?}"
            );
            assert_eq!(jobs(&store), i64::from(preserved), "{boundary:?}");
            assert!(store.history().unwrap()[0].interrupted_tail);
            assert_eq!(store.recovery.interrupted_sessions, u32::from(restart == 0));
            let timeline = store.timeline(TRACK).unwrap();
            assert_eq!(timeline[0].kind, if preserved { "audio" } else { "gap" });
            assert_eq!(timeline[0].end_ns, END);
            assert_eq!(
                store.recovery.known_tail_loss_ns,
                if !preserved && restart == 0 {
                    END as u64
                } else {
                    0
                }
            );
            if restart == 0 {
                println!(
                    "{boundary:?} recovered in {} ms, known uncommitted tail loss {} ns, committed={preserved}",
                    store.recovery.elapsed_ms, store.recovery.known_tail_loss_ns
                );
            }
        }
    }
}

#[test]
fn seeks_by_track_time_and_detects_missing_and_corrupt_files() {
    let scratch = Scratch::new();
    let legacy = scratch.0.join("legacy.wav");
    fs::write(&legacy, b"previous prototype").unwrap();
    let store = Store::open(&scratch.0).unwrap();
    assert!(matches!(Store::open(&scratch.0), Err(Error::Locked)));
    prepare(&store);
    begin(&store, FIRST, 0);
    store
        .seal(FIRST, END, &[packet(0)], &pcm(), |_| {})
        .unwrap();
    assert!(matches!(
        store.execute(
            Command::Interval {
                track_id: stable(TRACK),
                epoch: 0,
                start_ns: END / 2,
                end_ns: END,
                kind: IntervalKind::Silence,
                cause: None,
            },
            &[]
        ),
        Err(Error::Invalid)
    ));
    store
        .execute(
            Command::Interval {
                track_id: stable(TRACK),
                epoch: 0,
                start_ns: END,
                end_ns: END * 2,
                kind: IntervalKind::Silence,
                cause: None,
            },
            &[],
        )
        .unwrap();
    begin(&store, SECOND, 2 * END);
    // The first processing job remains pending while the writer commits another segment.
    store
        .seal(SECOND, 3 * END, &[packet(2 * END)], &pcm(), |_| {})
        .unwrap();
    assert_eq!(jobs(&store), 2);
    assert!(matches!(store.seek(TRACK, END), Err(Error::Unavailable)));
    let Response::Playback {
        channels,
        next_ns,
        samples,
    } = store.seek(TRACK, END / 2).unwrap()
    else {
        panic!("Expected committed playback");
    };
    assert_eq!((channels, next_ns, samples.len()), (1, END, FRAMES / 2));
    assert!(samples.iter().any(|sample| sample.abs() > 0.01));
    let metadata: String = store
        .connection()
        .unwrap()
        .query_row(
            "SELECT packets_json FROM chunks WHERE id=?1",
            [FIRST],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        metadata.contains("\"qpc100ns\":456")
            && metadata.contains("\"device_position_frames\":123")
    );
    let first = store.path(&store.chunk(FIRST).unwrap(), "opus").unwrap();
    let second = store.path(&store.chunk(SECOND).unwrap(), "opus").unwrap();
    fs::remove_file(&first).unwrap();
    let mut bytes = fs::read(&second).unwrap();
    bytes[0] ^= 1;
    fs::write(&second, bytes).unwrap();
    fs::write(first.with_file_name("unindexed.opus"), b"orphan").unwrap();
    drop(store);

    let recovered = Store::open(&scratch.0).unwrap();
    assert_eq!(recovered.recovery.missing_chunks, 1);
    assert_eq!(recovered.recovery.corrupt_chunks, 1);
    assert_eq!(recovered.recovery.quarantined_files, 2);
    let timeline = recovered.timeline(TRACK).unwrap();
    assert_eq!(timeline[0].cause.as_deref(), Some("missing_file"));
    assert_eq!(timeline[1].kind, "silence");
    assert_eq!(timeline[2].cause.as_deref(), Some("corrupt_file"));
    assert!(matches!(recovered.seek(TRACK, 0), Err(Error::Unavailable)));
    assert!(matches!(
        recovered.seek(TRACK, 2 * END),
        Err(Error::Unavailable)
    ));
    let available: i64 = recovered
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM processing_jobs WHERE state='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(available, 0);
    assert_eq!(fs::read(legacy).unwrap(), b"previous prototype");
}
