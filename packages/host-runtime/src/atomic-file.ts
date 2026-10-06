import { rename } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';

/** Preserve the old destination while Windows readers temporarily deny FILE_SHARE_DELETE. */
export async function replaceFile(temporary: string, destination: string): Promise<void> {
  const end = Date.now() + 2000;
  let attempts = 0;
  while (true) {
    try { await rename(temporary, destination); return; }
    catch (error) {
      if (process.platform !== 'win32' || !['EPERM', 'EACCES', 'EBUSY'].includes((error as NodeJS.ErrnoException).code ?? '') || Date.now() >= end) throw error;
      await delay(Math.min(100, 10 * 2 ** Math.min(attempts++, 4), end - Date.now()));
    }
  }
}
