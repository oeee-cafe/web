-- What is kept about a collaborative session's recording, summarised where the
-- admin session list can read it.
--
-- The recording itself lives in object storage (collaborate::archive), and the
-- list cannot fetch a manifest, a transcript and a directory of reports for
-- every row it shows. So each is noted here as it is written: the span of the
-- log when a manifest is, the transcript's length when it is stored, a report
-- when one is filed -- and the replay check when the inspector runs one.
--
-- Sessions recorded before this table existed have no row until somebody
-- opens them in the inspector, which notes what it read.
CREATE TABLE collaborative_session_recordings (
    session_id UUID PRIMARY KEY REFERENCES collaborative_sessions(id) ON DELETE CASCADE,
    -- The span the stored log covers. A first_seq other than 1 is a recording
    -- that began mid-session and cannot be rendered from nothing.
    first_seq BIGINT,
    last_seq BIGINT,
    messages BIGINT NOT NULL DEFAULT 0,
    sealed BOOLEAN NOT NULL DEFAULT FALSE,
    chat_lines INTEGER NOT NULL DEFAULT 0,
    reports INTEGER NOT NULL DEFAULT 0,
    -- The last replay check: the recording played to its end and compared,
    -- pixel for pixel, with the post the session was saved as.
    check_outcome TEXT CHECK (check_outcome IN ('match', 'differs', 'incomplete', 'unavailable')),
    check_differing_pixels BIGINT,
    check_total_pixels BIGINT,
    -- The last sequence the check played through.
    check_seq BIGINT,
    check_note TEXT,
    checked_at TIMESTAMPTZ,
    checked_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- "Needs a look": reported, or replayed to something other than what was saved.
CREATE INDEX idx_collab_recordings_flagged ON collaborative_session_recordings(session_id)
    WHERE reports > 0 OR check_outcome = 'differs';
