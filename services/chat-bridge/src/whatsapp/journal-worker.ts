import { parentPort, workerData } from 'node:worker_threads';
import { DatabaseSync } from 'node:sqlite';
const db = new DatabaseSync(workerData as string);
db.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;');
parentPort!.on('message', ({ id, batches }: { id: number; batches: Array<Array<{ sql: string; args: Array<string|number|null> }>> }) => {
  try {
    db.exec('BEGIN IMMEDIATE');
    for (const batch of batches) for (const statement of batch) db.prepare(statement.sql).run(...statement.args);
    db.exec('COMMIT'); parentPort!.postMessage({ id });
  } catch {
    try { db.exec('ROLLBACK'); } catch {}
    // SQL values include credentials. Never forward them in diagnostics.
    parentPort!.postMessage({ id, error: true });
  }
});
