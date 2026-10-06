import { randomUUID } from 'node:crypto';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { CodexAppTools } from '../../../../packages/sdk/src/codex-app-tools.js';
import type { AppToolsSettings } from '../../../../packages/sdk/src/codex-app-tools.js';
import type { PhoneJournal, RetiredVoiceTask } from './journal.js';
import type { PhoneVoiceArchiveSettings } from './desktop-voice-archive.js';
import { PhoneVoiceController } from './desktop-voice-controller.js';
import { desktopThreadArchiveState, readDesktopVoiceReference } from './desktop-voice-inventory.js';

type ArchivePort = Pick<CodexAppTools, 'readThread' | 'archiveThread' | 'close'>;

/** Superseded created tasks and proven transient startup chats enter this lifecycle.
 * A confirmed replacement retires them atomically in the journal; this worker may
 * continue archival across process restarts. Archived tasks remain in Desktop. */
export class PhoneVoiceRetention {
  private running: Promise<void> | null = null;
  private cursor = '';
  constructor(
    private readonly journal: PhoneJournal,
    private readonly settings: PhoneVoiceArchiveSettings,
    private readonly open: (settings: AppToolsSettings) => ArchivePort = settings => new CodexAppTools(settings),
    private readonly controller = new PhoneVoiceController(open),
    private readonly now: () => Date = () => new Date(),
    private readonly onIssue: (threadId: string, code: string) => void = () => undefined,
  ) {}

  tick(): Promise<void> {
    return this.running ??= this.scan().finally(() => { this.running = null; });
  }

  private safe(task: RetiredVoiceTask): boolean {
    requireThat(this.journal.reusableVoiceTask(task.principalId) !== task.threadId,
      'phone_voice_retention_changed', 'A reusable Voice task cannot be retired.');
    const reference = readDesktopVoiceReference(this.settings.databasePath);
    if (reference === task.threadId) return false;
    for (const call of this.journal.currentCalls()) {
      const generation = this.journal.latestVoiceGeneration(call.callId);
      if (generation !== null && this.journal.voiceTask(call.callId, generation) === task.threadId) return false;
    }
    return true;
  }

  private async archive(task: RetiredVoiceTask): Promise<void> {
    if (!this.safe(task)) return;
    const state = desktopThreadArchiveState(this.settings.databasePath, task.threadId);
    if (state !== 'unarchived') {
      this.journal.finishVoiceArchive(task.threadId, this.now().toISOString());
      return;
    }
    // A submitted App mutation with a lost acknowledgement is never submitted again.
    // A later exact Desktop observation can still confirm that it succeeded.
    if (task.phase === 'archiving') return;
    const resolved = await this.controller.resolve(this.settings.databasePath, this.settings.appTools);
    requireThat(resolved.actorThreadId !== task.threadId,
      'phone_voice_retention_changed', 'Voice task is the assigned App Tools controller.');
    const port = this.open(resolved);
    try {
      const observed = await port.readThread(task.threadId);
      if (observed.id !== task.threadId || observed.hostId !== 'local' || observed.kind !== 'codex' ||
        !['idle', 'notLoaded'].includes(observed.status.type) || observed.status.activeFlags.length) return;
      const operationId = randomUUID();
      await port.archiveThread(task.threadId, operationId, async () => {
        requireThat(this.safe(task) && desktopThreadArchiveState(this.settings.databasePath, task.threadId) === 'unarchived',
          'phone_voice_retention_changed', 'Retired Voice task changed before archival.');
        this.journal.beginVoiceArchive(task.threadId, operationId);
      });
      this.journal.finishVoiceArchive(task.threadId, this.now().toISOString());
    } finally { await port.close(); }
  }

  private async scan(): Promise<void> {
    let page = this.journal.retiredVoicePage(64, this.cursor);
    if (!page.length && this.cursor) {
      this.cursor = '';
      page = this.journal.retiredVoicePage(64);
    }
    for (const task of page) {
      this.cursor = task.threadId;
      try {
        await this.archive(task);
      } catch (error) {
        this.onIssue(task.threadId, IvyError.from(error).code);
      }
    }
  }
}
