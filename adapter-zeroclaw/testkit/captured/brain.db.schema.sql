CREATE TABLE embedding_cache (
                content_hash TEXT PRIMARY KEY,
                embedding    BLOB NOT NULL,
                created_at   TEXT NOT NULL,
                accessed_at  TEXT NOT NULL
            );
CREATE INDEX idx_cache_accessed ON embedding_cache(accessed_at);
CREATE TABLE agents (
                id          TEXT PRIMARY KEY,
                alias       TEXT NOT NULL UNIQUE,
                created_at  TEXT NOT NULL
             );
CREATE TABLE IF NOT EXISTS "memories" (
                id            TEXT PRIMARY KEY,
                key           TEXT NOT NULL,
                content       TEXT NOT NULL,
                category      TEXT NOT NULL DEFAULT 'core',
                embedding     BLOB,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                session_id    TEXT,
                namespace     TEXT DEFAULT 'default',
                importance    REAL DEFAULT 0.5,
                superseded_by TEXT,
                agent_id      TEXT NOT NULL REFERENCES agents(id),
                UNIQUE (agent_id, key)
             );
CREATE INDEX idx_memories_category  ON memories(category);
CREATE INDEX idx_memories_key       ON memories(key);
CREATE INDEX idx_memories_session   ON memories(session_id);
CREATE INDEX idx_memories_namespace ON memories(namespace);
CREATE INDEX idx_memories_agent_id  ON memories(agent_id);
CREATE VIRTUAL TABLE memories_fts USING fts5(
                key, content, content=memories, content_rowid=rowid
             )
/* memories_fts("key",content) */;
CREATE TABLE IF NOT EXISTS 'memories_fts_data'(id INTEGER PRIMARY KEY, block BLOB);
CREATE TABLE IF NOT EXISTS 'memories_fts_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS 'memories_fts_docsize'(id INTEGER PRIMARY KEY, sz BLOB);
CREATE TABLE IF NOT EXISTS 'memories_fts_config'(k PRIMARY KEY, v) WITHOUT ROWID;
CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
             END;
CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
             END;
CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, key, content)
                VALUES ('delete', old.rowid, old.key, old.content);
                INSERT INTO memories_fts(rowid, key, content)
                VALUES (new.rowid, new.key, new.content);
             END;
CREATE TABLE schema_version (
            component  TEXT PRIMARY KEY,
            version    INTEGER NOT NULL,
            applied_at TEXT NOT NULL
         );
