CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
    clock TEXT NOT NULL,
    clock_origin INTEGER NOT NULL CHECK (clock_origin >= 0),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 6),
    elapsed_ns INTEGER NOT NULL DEFAULT 0 CHECK (elapsed_ns >= 0),
    snapshot TEXT,
    interrupted_tail INTEGER NOT NULL DEFAULT 0 CHECK (interrupted_tail IN (0, 1))
) STRICT;

CREATE TABLE tracks (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    source TEXT NOT NULL CHECK (source IN ('mic', 'system_loopback')),
    endpoint_id TEXT NOT NULL,
    UNIQUE (session_id, source)
) STRICT;

CREATE TABLE streams (
    id INTEGER PRIMARY KEY,
    track_id TEXT NOT NULL REFERENCES tracks(id),
    source_epoch INTEGER NOT NULL CHECK (source_epoch BETWEEN 0 AND 4294967295),
    start_ns INTEGER NOT NULL CHECK (start_ns >= 0),
    end_ns INTEGER CHECK (end_ns >= start_ns),
    reason TEXT,
    rate INTEGER NOT NULL CHECK (rate > 0),
    channels INTEGER NOT NULL CHECK (channels BETWEEN 1 AND 32),
    sample_format TEXT NOT NULL,
    native_format TEXT NOT NULL,
    UNIQUE (track_id, start_ns)
) STRICT;
CREATE UNIQUE INDEX active_stream ON streams(track_id) WHERE end_ns IS NULL;

CREATE TABLE chunks (
    id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL REFERENCES tracks(id),
    stream_id INTEGER NOT NULL REFERENCES streams(id),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    start_ns INTEGER NOT NULL CHECK (start_ns >= 0),
    end_ns INTEGER NOT NULL CHECK (end_ns > start_ns AND end_ns - start_ns <= 10000000000),
    codec TEXT NOT NULL DEFAULT 'opus' CHECK (codec = 'opus'),
    playback_rate INTEGER NOT NULL DEFAULT 48000 CHECK (playback_rate = 48000),
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'missing')),
    frames INTEGER CHECK (frames BETWEEN 1 AND 480000),
    byte_size INTEGER CHECK (byte_size > 0),
    sha256 TEXT CHECK (length(sha256) = 64),
    packets_json TEXT,
    problem TEXT,
    CHECK (state <> 'committed' OR (sha256 IS NOT NULL AND byte_size IS NOT NULL AND frames IS NOT NULL)),
    UNIQUE (track_id, sequence),
    UNIQUE (track_id, start_ns)
) STRICT;
CREATE UNIQUE INDEX pending_chunk ON chunks(track_id) WHERE state = 'pending';
CREATE INDEX chunks_by_track_time ON chunks(track_id, start_ns, end_ns);

CREATE TABLE intervals (
    id INTEGER PRIMARY KEY,
    track_id TEXT NOT NULL REFERENCES tracks(id),
    source_epoch INTEGER NOT NULL CHECK (source_epoch BETWEEN 0 AND 4294967295),
    start_ns INTEGER NOT NULL CHECK (start_ns >= 0),
    end_ns INTEGER NOT NULL CHECK (end_ns > start_ns),
    kind TEXT NOT NULL CHECK (kind IN ('gap', 'silence')),
    cause TEXT,
    CHECK ((kind = 'silence') = (cause IS NULL)),
    UNIQUE (track_id, source_epoch, start_ns, end_ns, kind)
) STRICT;

CREATE TABLE processing_jobs (
    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id),
    state TEXT NOT NULL CHECK (state IN ('pending', 'unavailable'))
) STRICT;

CREATE VIEW audio_timeline AS
SELECT c.track_id, s.source_epoch, c.start_ns, c.end_ns,
       CASE c.state WHEN 'missing' THEN 'gap' WHEN 'committed' THEN 'audio' ELSE 'pending' END AS kind,
       c.problem AS cause
FROM chunks c JOIN streams s ON s.id = c.stream_id
UNION ALL
SELECT track_id, source_epoch, start_ns, end_ns, kind, cause FROM intervals;
