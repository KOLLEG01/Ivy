import { isAbsolute } from 'node:path';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { CodexAppTools } from '../../../../packages/sdk/src/codex-app-tools.js';
import { readDesktopVoiceCandidates, verifyDesktopArchiveActor } from './desktop-voice-inventory.js';
import { validatePhoneService } from './registry.js';

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
