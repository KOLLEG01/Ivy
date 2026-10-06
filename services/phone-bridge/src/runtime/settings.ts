import { isAbsolute, win32 } from 'node:path';
import { jsonFile } from '../../../../packages/host-runtime/src/config.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { PhoneAdmission } from './admission.js';
import type { PhonePolicyDefinition } from './admission.js';
import type { PhoneFlowSettings } from './flow.js';
import { validatePhoneService } from './registry.js';

export interface PhoneSettings extends PhoneFlowSettings {
  native: { executable: string; executableHash: string };
  binding: Record<string, unknown>; codecs: Record<string, unknown>;
  registration: Record<string, unknown> | null; credentialsPath: string | null;
  policy: PhonePolicyDefinition; pollMs: number; incomingPrincipalId: string | null;
}
export function phoneSettings(value: unknown): PhoneSettings {
  validatePhoneService('PhoneServiceSettings', value); const settings = value as PhoneSettings;
  new PhoneAdmission(settings.policy);
  requireThat(!settings.policy.challengedIncoming?.length || typeof settings.accessCodePath === 'string' && isAbsolute(settings.accessCodePath),
    'invalid_arguments', 'Challenged incoming access requires an absolute protected code settings path.');
  requireThat(settings.voiceArchive === null || isAbsolute(settings.voiceArchive.databasePath),
    'invalid_arguments', 'Voice archival requires the exact absolute Desktop state database.');
  if (settings.voiceArchive) {
    const tools = settings.voiceArchive.appTools;
    requireThat((!tools.nodeExecutable || isAbsolute(tools.nodeExecutable)) &&
      (!tools.serverPath || isAbsolute(tools.serverPath)) &&
      (tools.pipePath === 'auto' || tools.pipePath.startsWith('\\\\.\\pipe\\')),
      'invalid_arguments', 'Voice task control requires local App Tools paths and a Desktop pipe selector.');
  }
  const micro = settings.voiceInput['micro'] as { usbipExecutable: string } | null;
  const hotkey = settings.voiceInput['hotkeyFallback'] ?? null;
  requireThat((settings.incomingRoute ?? 'voice') !== 'voice' && (settings.outgoingRoute ?? 'windows') !== 'voice' || micro !== null || hotkey !== null,
    'invalid_arguments', 'Voice call routing requires an explicit Codex Voice input.');
  if (micro) requireThat(win32.isAbsolute(micro.usbipExecutable) && win32.basename(micro.usbipExecutable).toLowerCase() === 'usbip.exe',
    'invalid_arguments', 'Micro requires the exact absolute Windows path to its verified usbip.exe client.');
  requireThat((settings.incomingRoute ?? 'voice') !== 'voice' && (settings.outgoingRoute ?? 'windows') !== 'voice' || settings.voiceArchive !== null,
    'invalid_arguments', 'Codex Voice routing requires direct Codex App Tools task control.');
  requireThat(settings.credentialsPath === null || isAbsolute(settings.credentialsPath), 'invalid_arguments', 'Phone credentials need an explicit absolute protected file path.');
  requireThat(settings.registration === null || settings.credentialsPath !== null, 'invalid_arguments', 'SIP registration requires its protected credential file.');
  requireThat(settings.incomingPrincipalId === null || settings.policy.incoming.length > 0 || (settings.policy.challengedIncoming?.length ?? 0) > 0,
    'invalid_arguments', 'Automatic incoming acceptance requires a configured incoming policy.');
  requireThat(new Set([...settings.policy.incoming, ...(settings.policy.challengedIncoming ?? [])].map(item => item.peerAddress)).size <= 32, 'invalid_arguments', 'Native incoming peer inventory exceeds its bound.');
  const snapshot = structuredClone(settings);
  snapshot.voiceInput['hotkeyFallback'] ??= null;
  return snapshot;
}
export async function phoneConfigurationSection(path: string, section: 'sip'|'access'|'speech'): Promise<unknown> {
  requireThat(isAbsolute(path),'invalid_arguments','Phone configuration requires an absolute protected path.');
  const value=await jsonFile<Record<string,unknown>>(path,128*1024);
  requireThat(value && value['schemaVersion']===1 && value[section] && typeof value[section]==='object' && !Array.isArray(value[section]),'invalid_arguments','Phone service configuration is invalid.');
  return value[section];
}
/** Only the protected native pipe receives the values; public tools cannot supply or read them. */
export async function phoneCredentials(path: string | null): Promise<{ username: string | null; password: string | null }> {
  if (path === null) return { username: null, password: null };
  const value = await phoneConfigurationSection(path,'sip') as {username:string|null;password:string|null};
  validatePhoneService('PhoneCredentials', value); return value;
}
