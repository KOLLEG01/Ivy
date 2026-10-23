import {isAbsolute} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {requireThat} from '../../packages/sdk/src/node.js';
import {CodexAppTools} from '../../packages/sdk/src/codex-app-tools.js';
import {validatePhoneService} from './phone-contracts.js';
export interface DesktopVoiceCandidate {threadId:string;descendantIds:string[]}
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
// Explicit, read-only operator setup. It creates, sends and archives no tasks.
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

/** Read-only setup of installed ordinary App Tools. It records selected contracts and
 * binds an explicitly assigned existing controller, never creates a task or archives one. */
export async function preparePhoneVoiceArchive(input: unknown) {
  validatePhoneService('PhoneVoiceArchiveSetupRequest', input);
  const value = input as { databasePath: string; nodeExecutable?: string; serverPath?: string; pipePath?: string; actorThreadId: string; internalProjectRoot?: string };
  requireThat(isAbsolute(value.databasePath) && (!value.nodeExecutable || isAbsolute(value.nodeExecutable)) &&
    (!value.serverPath || isAbsolute(value.serverPath)),
    'invalid_arguments', 'Voice archive setup requires an absolute Desktop database and optional absolute overrides.');
  requireThat(!value.internalProjectRoot || isAbsolute(value.internalProjectRoot),
    'invalid_arguments', 'Voice task internalProjectRoot must be absolute.');
  verifyDesktopArchiveActor(value.databasePath, value.actorThreadId);
  const appTools = await CodexAppTools.inspect({
    ...(value.nodeExecutable ? { nodeExecutable: value.nodeExecutable } : {}),
    ...(value.serverPath ? { serverPath: value.serverPath } : {}),
    pipePath: value.pipePath ?? 'auto', actorThreadId: value.actorThreadId,
    ...(value.internalProjectRoot ? { internalProjectRoot: value.internalProjectRoot } : {}) });
  verifyDesktopArchiveActor(value.databasePath, value.actorThreadId);
  const result = { schemaVersion: 1, voiceArchive: { databasePath: value.databasePath, appTools },
    candidateCount: readDesktopVoiceCandidates(value.databasePath).length, archived: false };
  validatePhoneService('PhoneVoiceArchiveSetupResult', result); return result;
}
