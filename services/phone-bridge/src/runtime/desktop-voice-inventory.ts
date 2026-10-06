import { readFileSync, statSync, openSync, closeSync, fstatSync, readSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { requireThat } from "../../../../packages/sdk/src/node.js";

export interface DesktopVoiceCandidate {
  threadId: string;
  descendantIds: string[];
}
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/** Persisted readiness for this exact chat. Transfer acknowledgement can precede
 * realtime startup; a loaded/idle task alone is not ready for a spoken greeting. */
export function readDesktopVoiceSession(databasePath: string, threadId: string): {
  sessionId: string; active: boolean;
} | null {
  requireThat(isAbsolute(databasePath) && uuid.test(threadId), 'invalid_arguments',
    'Explicit Desktop database and exact local task ID required.');
  const db = new DatabaseSync(databasePath, { readOnly: true });
  let path: unknown;
  try {
    db.exec('PRAGMA query_only=ON; PRAGMA busy_timeout=1000');
    path = db.prepare('SELECT rollout_path FROM threads WHERE id=? AND archived=0').get(threadId)?.['rollout_path'];
  } finally { db.close(); }
  if (path === undefined || path === null) return null;
  requireThat(typeof path === 'string' && isAbsolute(path), 'phone_voice_reference_invalid',
    'Desktop Voice rollout path is invalid.');
  let file: number;
  try { file = openSync(path, 'r'); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return null; throw error; }
  try {
    const size = fstatSync(file).size, head = Buffer.alloc(Math.min(size, 65536));
    readSync(file, head, 0, head.length, 0);
    const end = head.indexOf(10);
    if (end < 0) return null; // the first record may still be publishing
    const metadata = JSON.parse(head.subarray(0, end).toString('utf8')) as { type?: unknown; payload?: { id?: unknown } };
    requireThat(metadata.type === 'session_meta' && metadata.payload?.id === threadId,
      'phone_voice_reference_changed', 'Desktop Voice rollout belongs to another task.');
    const offset = Math.max(0, size - 512 * 1024), tail = Buffer.alloc(size - offset);
    readSync(file, tail, 0, tail.length, offset);
    const text = tail.toString('utf8');
    const preceding = Buffer.alloc(1);
    if (offset) readSync(file, preceding, 0, 1, offset - 1);
    // Ignore the partial first/last records of this bounded append-only window.
    const last = text.lastIndexOf('\n');
    if (last < 0) return null;
    const lines = text.slice(offset && preceding[0] !== 10 ? text.indexOf('\n') + 1 : 0, last).split('\n');
    let session: { sessionId: string; active: boolean } | null = null;
    for (const line of lines) {
      if (!line) continue;
      const event = JSON.parse(line) as { type?: unknown; payload?: { type?: unknown; realtime_session_id?: unknown } };
      const value = event.payload;
      if (event.type !== 'realtime_item' || !['realtime_session_started', 'realtime_session_closed'].includes(String(value?.type))) continue;
      requireThat(typeof value?.realtime_session_id === 'string' && uuid.test(value.realtime_session_id),
        'phone_voice_reference_invalid', 'Desktop Voice session identity is invalid.');
      session = { sessionId: value.realtime_session_id, active: value.type === 'realtime_session_started' };
    }
    return session;
  } finally { closeSync(file); }
}

/** A bounded startup baseline; archive classification of descendants is separate. */
export function readDesktopVoiceRoots(databasePath: string): string[] {
  requireThat(isAbsolute(databasePath), 'invalid_arguments', 'An absolute Desktop state database is required.');
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec('PRAGMA query_only=ON; PRAGMA busy_timeout=1000');
    const rows = db.prepare("SELECT id FROM threads WHERE thread_source='voice_chat' AND archived=0 ORDER BY recency_at_ms DESC,id LIMIT 257").all();
    requireThat(rows.length <= 256, 'phone_archive_capacity', 'Voice startup inventory exceeds its explicit bound.');
    return rows.map(row => {
      requireThat(typeof row['id'] === 'string' && uuid.test(row['id']),
        'phone_voice_reference_invalid', 'Desktop Voice startup identity is invalid.');
      return row['id'];
    });
  } finally { db.close(); }
}

/** Read the exact local task reference retained by Codex Desktop's Voice overlay. The
 * reference only identifies what Desktop itself will resume; App Tools and the state
 * database must still confirm that task before PhoneBridge relies on it. */
export function readDesktopVoiceReference(databasePath: string): string | null {
  requireThat(
    isAbsolute(databasePath),
    "invalid_arguments",
    "An explicit absolute Desktop state database is required.",
  );
  const statePath = join(dirname(databasePath), ".codex-global-state.json");
  const stateFile = statSync(statePath, { throwIfNoEntry: false });
  if (!stateFile) return null;
  const size = stateFile.size;
  requireThat(
    size > 0 && size <= 16 * 1024 * 1024,
    "phone_voice_reference_invalid",
    "Desktop Voice state exceeds its bounded format.",
  );
  const state = JSON.parse(readFileSync(statePath, "utf8")) as {
    "electron-persisted-atom-state"?: {
      "realtime-voice-most-recent-thread"?: {
        conversationId?: unknown;
        hostId?: unknown;
      };
    };
  };
  const reference =
    state["electron-persisted-atom-state"]?.[
      "realtime-voice-most-recent-thread"
    ];
  if (reference === undefined) return null;
  requireThat(
    reference.hostId === "local" &&
      typeof reference.conversationId === "string" &&
      uuid.test(reference.conversationId),
    "phone_voice_reference_invalid",
    "Desktop Voice retained an invalid or non-local task reference.",
  );
  return reference.conversationId;
}

/** A previously bound PhoneBridge task can be reused only while Desktop still
 * retains it as an unarchived local task. An absent or archived task needs a new
 * generation; an unreadable database must fail instead of guessing. */
export function isDesktopThreadUnarchived(databasePath: string, threadId: string): boolean {
  requireThat(isAbsolute(databasePath) && uuid.test(threadId), "invalid_arguments",
    "Explicit Desktop database and exact local task ID required.");
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec("PRAGMA query_only=ON; PRAGMA busy_timeout=1000");
    const row = db.prepare("SELECT archived FROM threads WHERE id=?").get(threadId);
    requireThat(!row || row["archived"] === 0 || row["archived"] === 1,
      "phone_voice_task_changed", "Desktop task archive state is invalid.");
    return row?.["archived"] === 0;
  } finally {
    db.close();
  }
}

/** Exact local archive state for a task already proven to be PhoneBridge-owned. */
export function desktopThreadArchiveState(
  databasePath: string, threadId: string,
): 'unarchived' | 'archived' | 'missing' {
  requireThat(isAbsolute(databasePath) && uuid.test(threadId), 'invalid_arguments',
    'Explicit Desktop database and exact local task ID required.');
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec('PRAGMA query_only=ON; PRAGMA busy_timeout=1000');
    const row = db.prepare('SELECT archived FROM threads WHERE id=?').get(threadId);
    requireThat(!row || row['archived'] === 0 || row['archived'] === 1,
      'phone_voice_task_changed', 'Desktop task archive state is invalid.');
    return !row ? 'missing' : row['archived'] === 1 ? 'archived' : 'unarchived';
  } finally { db.close(); }
}

/** Read-only classification from the explicitly configured Desktop home. This is not
 * archive authority: the owning App Tools executor must still confirm identity/idle
 * state immediately before dispatch, and retain the original operation outcome. */
export function readDesktopVoiceCandidates(
  databasePath: string,
  exactThreadId?: string,
  archivedOnly = false,
): DesktopVoiceCandidate[] {
  requireThat(
    isAbsolute(databasePath),
    "invalid_arguments",
    "An explicit absolute Desktop state database is required.",
  );
  requireThat(
    exactThreadId === undefined || uuid.test(exactThreadId),
    "invalid_arguments",
    "Exact Voice task ID required.",
  );
  requireThat(
    !archivedOnly || exactThreadId !== undefined,
    "invalid_arguments",
    "Archive reconciliation requires an original exact task.",
  );
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec("PRAGMA query_only=ON; PRAGMA busy_timeout=1000; BEGIN");
    for (const [table, expected] of [
      ["threads", ["id", "archived", "thread_source", "recency_at_ms"]],
      ["thread_spawn_edges", ["parent_thread_id", "child_thread_id"]],
    ] as const) {
      const columns = new Set(
        db
          .prepare("PRAGMA table_info(" + table + ")")
          .all()
          .map((row) => row["name"]),
      );
      requireThat(
        expected.every((column) => columns.has(column)),
        "phone_archive_schema_changed",
        "Desktop archive classification schema is unavailable.",
      );
    }
    const roots =
      exactThreadId === undefined
        ? db
            .prepare(
              "SELECT id FROM threads WHERE thread_source='voice_chat' AND archived=0 ORDER BY recency_at_ms DESC,id LIMIT 257",
            )
            .all()
        : db
            .prepare(
              "SELECT id FROM threads WHERE id=? AND thread_source='voice_chat' AND archived=?",
            )
            .all(exactThreadId, archivedOnly ? 1 : 0);
    requireThat(
      roots.length <= 256,
      "phone_archive_capacity",
      "Voice archive inventory exceeds its explicit bound.",
    );
    const tree = db.prepare(`WITH RECURSIVE descendants(id) AS (
      SELECT child_thread_id FROM thread_spawn_edges WHERE parent_thread_id=?
      UNION SELECT e.child_thread_id FROM thread_spawn_edges e JOIN descendants d ON e.parent_thread_id=d.id LIMIT 513
    ) SELECT d.id,t.thread_source,t.archived FROM descendants d LEFT JOIN threads t ON t.id=d.id ORDER BY d.id LIMIT 513`);
    const candidates = roots.map((root) => {
      const threadId = root["id"];
      requireThat(
        typeof threadId === "string" && uuid.test(threadId),
        "phone_archive_identity_invalid",
        "Desktop Voice identity is invalid.",
      );
      const descendants = tree.all(threadId);
      requireThat(
        descendants.length <= 512,
        "phone_archive_capacity",
        "Voice archive subtree exceeds its explicit bound.",
      );
      const descendantIds = descendants.map((row) => {
        const id = row["id"];
        requireThat(
          typeof id === "string" && uuid.test(id) && id !== threadId,
          "phone_archive_identity_invalid",
          "Desktop Voice subtree identity is invalid or cyclic.",
        );
        // Native archive prepares even previously archived descendants. A missing row,
        // non-Voice descendant or unknown source cannot be silently swept into this action.
        requireThat(
          row["thread_source"] === "voice_chat",
          "phone_archive_scope_conflict",
          "Native archive would include a descendant not positively classified as Voice.",
        );
        requireThat(
          !archivedOnly || row["archived"] === 1,
          "phone_archive_unconfirmed",
          "An original Voice descendant is not confirmed archived.",
        );
        return id;
      });
      return { threadId, descendantIds };
    });
    db.exec("COMMIT");
    return candidates;
  } finally {
    db.close();
  }
}

/** Return bounded candidates for a replacement controller. This is only a
 * discovery hint: each returned ID still has to pass an App-owned observation
 * with that ID as the controller before it can be used. */
export function readDesktopOrdinaryCandidates(
  databasePath: string,
  exclude = new Set<string>(),
): string[] {
  requireThat(
    isAbsolute(databasePath),
    "invalid_arguments",
    "An explicit absolute Desktop state database is required.",
  );
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec("PRAGMA query_only=ON; PRAGMA busy_timeout=1000; BEGIN");
    for (const [table, expected] of [
      ["threads", ["id", "archived", "thread_source", "recency_at_ms"]],
    ] as const) {
      const columns = new Set(
        db
          .prepare("PRAGMA table_info(" + table + ")")
          .all()
          .map((row) => row["name"]),
      );
      requireThat(
        expected.every((column) => columns.has(column)),
        "phone_archive_schema_changed",
        "Desktop controller classification schema is unavailable.",
      );
    }
    const rows = db
      .prepare(
        "SELECT id,thread_source FROM threads WHERE archived=0 AND (thread_source IS NULL OR thread_source!='voice_chat') ORDER BY recency_at_ms DESC,id LIMIT 257",
      )
      .all();
    requireThat(
      rows.length <= 256,
      "phone_archive_capacity",
      "Ordinary controller inventory exceeds its explicit bound.",
    );
    const ids = rows
      .map((row) => {
        const id = row["id"];
        requireThat(
          typeof id === "string" && uuid.test(id),
          "phone_archive_identity_invalid",
          "Desktop controller identity is invalid.",
        );
        return id;
      })
      .filter((id) => !exclude.has(id));
    db.exec("COMMIT");
    return ids;
  } finally {
    db.close();
  }
}

/** Bind the explicitly assigned ordinary controller to the same local Desktop database.
 * The database is never opened for writes, including while checking archived rows. */
export function verifyDesktopArchiveActor(
  databasePath: string,
  actorThreadId: string,
): void {
  requireThat(
    isAbsolute(databasePath) && uuid.test(actorThreadId),
    "invalid_arguments",
    "Explicit Desktop database and controller ID required.",
  );
  const db = new DatabaseSync(databasePath, { readOnly: true });
  try {
    db.exec("PRAGMA query_only=ON; PRAGMA busy_timeout=1000");
    const row = db
      .prepare("SELECT archived,thread_source FROM threads WHERE id=?")
      .get(actorThreadId);
    requireThat(
      row && row["archived"] === 0 && row["thread_source"] !== "voice_chat",
      "phone_archive_actor_changed",
      "Assigned controller must be an unarchived ordinary task in the configured Desktop home.",
    );
  } finally {
    db.close();
  }
}
