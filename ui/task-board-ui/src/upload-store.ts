import type { SavedAction } from './action';

export const maximumUploadBytes = 8 * 1024 * 1024;
const maximumRetainedBytes = 32 * 1024 * 1024, maximumRetainedFiles = 8;
interface RetainedUpload { id: string; scope: string; action: SavedAction; file: Blob; createdAt: number }
let connection: Promise<IDBDatabase> | undefined;
const database = () => connection ??= new Promise<IDBDatabase>((resolve, reject) => {
  let blocked = false;
  const request = indexedDB.open('ivy-task-board-uploads', 1);
  request.onupgradeneeded = () => request.result.createObjectStore('uploads', { keyPath: 'id' }).createIndex('scope', 'scope');
  request.onsuccess = () => {
    const db = request.result;
    if (blocked) { db.close(); return; }
    db.onversionchange = () => { db.close(); connection = undefined; };
    resolve(db);
  };
  request.onerror = () => { connection = undefined; reject(request.error); };
  request.onblocked = () => { blocked = true; connection = undefined; reject(new Error('The original upload storage is open in another tab. Close that tab and retry.')); };
});
const metadata = (action: SavedAction) => JSON.parse(JSON.stringify(action)) as SavedAction;
const terminal = (action: SavedAction) => action.phase === 'succeeded' || action.phase === 'failed';
const sameOriginal = (left: SavedAction, right: SavedAction) => left.callerPrincipalId === right.callerPrincipalId &&
  left.principalId === right.principalId && left.rootObjectId === right.rootObjectId &&
  JSON.stringify(left.call) === JSON.stringify(right.call) && JSON.stringify(left.upload) === JSON.stringify(right.upload) &&
  (!left.requestHash || !right.requestHash || left.requestHash === right.requestHash);

/** The transaction completes before dispatch. Unresolved originals are never evicted for space. */
export async function retainUpload(scope: string, action: SavedAction, file: Blob) {
  if (!action.upload || file.size !== action.upload.byteLength || file.size > maximumUploadBytes) throw new Error('The file differs from the original upload.');
  const db = await database(), value = metadata(action);
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction('uploads', 'readwrite'), store = tx.objectStore('uploads');
    let failure: Error | undefined;
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(failure ?? tx.error ?? new Error('The original file could not be retained.'));
    const request = store.getAll();
    request.onsuccess = () => {
      const records = request.result as RetainedUpload[];
      const original = records.find(row => row.id === action.call.operationId);
      if (original && (original.scope !== scope || !sameOriginal(original.action, value) || terminal(original.action))) {
        failure = new Error('The retained upload has another original identity.'); tx.abort(); return;
      }
      const unresolved = records.filter(row => !terminal(row.action) && row.id !== action.call.operationId);
      if (unresolved.length >= maximumRetainedFiles || unresolved.reduce((sum, row) => sum + row.file.size, file.size) > maximumRetainedBytes) {
        failure = new Error('Finish the original pending uploads before adding more files. At most 8 files and 32 MiB can be retained.'); tx.abort(); return;
      }
      for (const row of records) if (terminal(row.action)) store.delete(row.id);
      store.put({ id: action.call.operationId!, scope, action: value, file, createdAt: original?.createdAt ?? Date.now() } satisfies RetainedUpload);
    };
  });
}

export async function uploadFile(action: SavedAction): Promise<Blob | null> {
  const db = await database();
  return new Promise((resolve, reject) => {
    const request = db.transaction('uploads').objectStore('uploads').get(action.call.operationId!);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const row = request.result as RetainedUpload | undefined;
      if (!row) return resolve(null);
      if (!sameOriginal(row.action, action)) return reject(new Error('The retained file belongs to another original action.'));
      resolve(row.file);
    };
  });
}

/** Keep the compact original call beside its Blob so a lost tab pointer remains recoverable. */
export async function saveUpload(action: SavedAction) {
  const db = await database(), value = metadata(action);
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction('uploads', 'readwrite'), store = tx.objectStore('uploads'), request = store.get(action.call.operationId!);
    let failure: Error | undefined;
    tx.oncomplete = () => resolve(); tx.onabort = () => reject(failure ?? tx.error ?? new Error('The original upload could not be retained.'));
    request.onsuccess = () => {
      const row = request.result as RetainedUpload | undefined;
      if (!row) return;
      if (!sameOriginal(row.action, value)) { failure = new Error('The retained upload has another original identity.'); tx.abort(); return; }
      if (!terminal(row.action)) store.put({ ...row, action: value });
    };
  });
}

export async function releaseUpload(action: SavedAction) {
  if (!terminal(action)) throw new Error('The original upload has no confirmed terminal outcome.');
  const db = await database();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction('uploads', 'readwrite'), store = tx.objectStore('uploads'), request = store.get(action.call.operationId!);
    let failure: Error | undefined;
    request.onsuccess = () => {
      const row = request.result as RetainedUpload | undefined;
      if (row && !sameOriginal(row.action, action)) { failure = new Error('The retained upload has another original identity.'); tx.abort(); return; }
      store.delete(action.call.operationId!);
    };
    tx.oncomplete = () => resolve(); tx.onabort = () => reject(failure ?? tx.error);
  });
}

export async function recoverUpload(scope: string, owner: { callerPrincipalId: string; principalId: string; rootObjectId: string | null; serviceNodeId: string }): Promise<SavedAction | null> {
  const db = await database();
  return new Promise((resolve, reject) => {
    const request = db.transaction('uploads').objectStore('uploads').index('scope').getAll(scope);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const row = (request.result as RetainedUpload[]).filter(row => !terminal(row.action) && row.action.callerPrincipalId === owner.callerPrincipalId &&
        row.action.principalId === owner.principalId && row.action.rootObjectId === owner.rootObjectId && row.action.call.serviceNodeId === owner.serviceNodeId)
        .sort((left, right) => left.createdAt - right.createdAt)[0];
      resolve(row?.action ?? null);
    };
  });
}
