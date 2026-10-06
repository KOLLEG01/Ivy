import { canonical } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { CodexAppTools } from '../../../../packages/sdk/src/codex-app-tools.js';
import type { AppToolsSettings, AppThreadObservation } from '../../../../packages/sdk/src/codex-app-tools.js';
import { readDesktopOrdinaryCandidates, readDesktopVoiceCandidates, verifyDesktopArchiveActor } from './desktop-voice-inventory.js';

export type DesktopVoiceControllerPort = Pick<CodexAppTools, 'readThread' | 'close'>;

/** Resolves the configured App Tools actor without changing HostConfig.
 * The configured ID remains the first choice; after it is archived, only an
 * unarchived non-Voice task that confirms itself through App Tools is accepted. */
export class PhoneVoiceController {
  private readonly selected = new Map<string, AppToolsSettings>();
  private readonly pending = new Map<string, Promise<AppToolsSettings>>();
  private readonly open: (settings: AppToolsSettings) => DesktopVoiceControllerPort;

  constructor(open: (settings: AppToolsSettings) => DesktopVoiceControllerPort = settings => new CodexAppTools(settings)) {
    this.open = open;
  }

  async resolve(databasePath: string, settings: AppToolsSettings): Promise<AppToolsSettings> {
    const key = canonical({ databasePath, appTools: { ...settings, actorThreadId: null } });
    const current = this.selected.get(key) ?? settings;
    try {
      verifyDesktopArchiveActor(databasePath, current.actorThreadId);
      this.selected.set(key, structuredClone(current));
      return structuredClone(current);
    } catch (error) {
      if (IvyError.from(error).code !== 'phone_archive_actor_changed') throw error;
      const running = this.pending.get(key);
      if (running) return running;
      const replacement = this.selectReplacement(databasePath, settings, key).finally(() => this.pending.delete(key));
      this.pending.set(key, replacement);
      return replacement;
    }
  }

  private async selectReplacement(databasePath: string, settings: AppToolsSettings, key: string): Promise<AppToolsSettings> {
    const voiceIds = new Set(readDesktopVoiceCandidates(databasePath).flatMap(candidate => [candidate.threadId, ...candidate.descendantIds]));
    const candidates = readDesktopOrdinaryCandidates(databasePath, voiceIds).filter(id => id !== settings.actorThreadId);
    for (const actorThreadId of candidates) {
      const replacement = { ...structuredClone(settings), actorThreadId };
      try {
        verifyDesktopArchiveActor(databasePath, actorThreadId);
        const port = this.open(replacement);
        try {
          const actor = await port.readThread(actorThreadId);
          requireThat(this.isUsable(actor, actorThreadId), 'phone_archive_actor_changed', 'Replacement controller is not a usable local Codex task.');
          verifyDesktopArchiveActor(databasePath, actorThreadId);
          this.selected.set(key, structuredClone(replacement));
          return replacement;
        } finally { await port.close(); }
      } catch {
        // A candidate that disappears or changes identity is simply skipped;
        // the next positive App-observed candidate remains eligible.
      }
    }
    throw new IvyError('phone_archive_actor_unavailable', 'No unarchived ordinary local Codex task is available as Phone controller.');
  }

  private isUsable(actor: AppThreadObservation, actorThreadId: string): boolean {
    return actor.id === actorThreadId && actor.hostId === 'local' && actor.kind === 'codex' &&
      ['active', 'idle', 'notLoaded'].includes(actor.status.type) && actor.status.activeFlags.length === 0;
  }
}
