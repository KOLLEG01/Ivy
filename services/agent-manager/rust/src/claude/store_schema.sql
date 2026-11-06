
      CREATE TABLE IF NOT EXISTS threads (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        forked_from_id TEXT,
        is_pinned INTEGER NOT NULL DEFAULT 0,
        section_id TEXT,
        section_entered_at INTEGER,
        section_position INTEGER,
        preview TEXT NOT NULL,
        name TEXT,
        archived INTEGER NOT NULL DEFAULT 0,
        cwd TEXT NOT NULL,
        model TEXT NOT NULL,
        reasoning_effort TEXT,
        model_provider TEXT NOT NULL,
        claude_session_id TEXT,
        source TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        status_json TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS turns (
        id TEXT PRIMARY KEY,
        thread_id TEXT NOT NULL,
        status TEXT NOT NULL,
        started_at INTEGER,
        completed_at INTEGER,
        duration_ms INTEGER,
        diff TEXT NOT NULL DEFAULT '',
        error_json TEXT,
        FOREIGN KEY(thread_id) REFERENCES threads(id)
      );
      CREATE INDEX IF NOT EXISTS idx_threads_updated ON threads(updated_at DESC);
      CREATE INDEX IF NOT EXISTS idx_turns_thread ON turns(thread_id, started_at);
      CREATE TABLE IF NOT EXISTS turn_items (
        turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        item_key TEXT,
        kind TEXT,
        body_json TEXT NOT NULL,
        PRIMARY KEY(turn_id, position)
      );
      CREATE INDEX IF NOT EXISTS idx_turn_items_key ON turn_items(turn_id, item_key, position);
      CREATE INDEX IF NOT EXISTS idx_turn_items_kind ON turn_items(turn_id, kind, position);
      CREATE TABLE IF NOT EXISTS item_deltas (
        id INTEGER PRIMARY KEY,
        turn_id TEXT NOT NULL,
        position INTEGER NOT NULL,
        target INTEGER NOT NULL CHECK(target BETWEEN 0 AND 2),
        delta_json TEXT NOT NULL,
        FOREIGN KEY(turn_id, position) REFERENCES turn_items(turn_id, position) ON DELETE CASCADE
      );
      CREATE INDEX IF NOT EXISTS idx_item_deltas_item ON item_deltas(turn_id, position, id);
      CREATE TABLE IF NOT EXISTS thread_sections (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        appearance_json TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_thread_sections_created ON thread_sections(created_at, id);
