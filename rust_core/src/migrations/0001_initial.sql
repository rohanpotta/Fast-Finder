-- Core file record. One row per indexed path.
CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    path        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    parent_dir  TEXT NOT NULL,
    ext         TEXT,
    size        INTEGER NOT NULL DEFAULT 0,
    is_dir      INTEGER NOT NULL DEFAULT 0,
    mtime       INTEGER NOT NULL DEFAULT 0,
    ctime       INTEGER NOT NULL DEFAULT 0,
    file_kind   TEXT NOT NULL DEFAULT '',
    indexed_at  INTEGER NOT NULL
);

CREATE INDEX files_mtime_desc       ON files(mtime DESC);
CREATE INDEX files_parent_name      ON files(parent_dir, name);
CREATE INDEX files_ext              ON files(ext);

-- Spotlight + usage signals. Populated from MDItem attrs at index time
-- (kMDItemLastUsedDate, kMDItemUsedDates, etc.) Reserved columns kept NULL
-- until the puller lands; queries that need them must tolerate NULL.
CREATE TABLE file_signals (
    file_id          INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    last_used_date   INTEGER,
    use_count        INTEGER NOT NULL DEFAULT 0,
    content_created  INTEGER,
    user_tags        TEXT,  -- JSON array
    keywords         TEXT,  -- JSON array
    updated_at       INTEGER NOT NULL
);

CREATE INDEX file_signals_last_used ON file_signals(last_used_date DESC);

-- First-class action records. Every FS mutation writes one of these with a
-- populated inverse_payload so undo is just "execute inverse_payload".
-- Composite intents (e.g. "organize Downloads") use parent_id to group leaves.
CREATE TABLE blocks (
    id               INTEGER PRIMARY KEY,
    parent_id        INTEGER REFERENCES blocks(id) ON DELETE CASCADE,
    kind             TEXT NOT NULL,        -- moveFiles | trashFiles | rename | compress | userIntent | ...
    payload          TEXT NOT NULL,        -- JSON
    inverse_payload  TEXT,                 -- JSON; NULL only if action is irreversible
    status           TEXT NOT NULL DEFAULT 'executed', -- pending|executed|failed|undone
    user_query       TEXT,                 -- the NL query that produced this, if any
    error            TEXT,
    created_at       INTEGER NOT NULL,
    executed_at      INTEGER
);

CREATE INDEX blocks_created_desc ON blocks(created_at DESC);
CREATE INDEX blocks_status       ON blocks(status, created_at DESC);
CREATE INDEX blocks_parent       ON blocks(parent_id);

-- Vector store. Model id in the PK so multiple embedding models can coexist
-- during migrations; queries pick (file_id, current_model). Vec is a raw
-- f32[] blob; ANN index lives in the application layer (or sqlite-vec when wired).
CREATE TABLE embeddings (
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    model_id    TEXT NOT NULL,
    dims        INTEGER NOT NULL,
    vec         BLOB NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (file_id, model_id)
);

-- Key/value for tracking which embedding model is "current" and other settings.
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- FTS5 over name + path. Contentless table mirrored from files via triggers
-- so we don't double-store strings.
CREATE VIRTUAL TABLE files_fts USING fts5(
    name,
    path,
    content='files',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER files_ai AFTER INSERT ON files BEGIN
    INSERT INTO files_fts(rowid, name, path) VALUES (new.id, new.name, new.path);
END;

CREATE TRIGGER files_ad AFTER DELETE ON files BEGIN
    INSERT INTO files_fts(files_fts, rowid, name, path) VALUES ('delete', old.id, old.name, old.path);
END;

CREATE TRIGGER files_au AFTER UPDATE ON files BEGIN
    INSERT INTO files_fts(files_fts, rowid, name, path) VALUES ('delete', old.id, old.name, old.path);
    INSERT INTO files_fts(rowid, name, path) VALUES (new.id, new.name, new.path);
END;
