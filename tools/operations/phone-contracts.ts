import { readFileSync } from 'node:fs';
import { bundleLocalSchema, requireThat, SchemaValidators } from '../../packages/sdk/src/node.js';
import type { Schema } from '../../packages/sdk/src/node.js';

// Operator tools consume the public schemas; no Phone service implementation is loaded.
const load = (name: string): Record<string, Schema> => JSON.parse(readFileSync(
  new URL('../../specs/schemas/' + name + '.schema.json', import.meta.url), 'utf8')).$defs;
const native = JSON.parse(JSON.stringify(Object.assign({}, ...[
  'codex-app-tools', 'phone-voice-archive', 'phone-launch', 'phone-hotkey', 'phone-micro',
  'phone-capture', 'phone-desktop-controls', 'phone-voice-lifetime', 'phone-runtime', 'phone-ipc',
].map(load))).replace(/phone-(?:runtime|launch|hotkey|desktop-controls|micro|voice-lifetime)\.schema\.json#\/\$defs\//g, '#/$defs/')) as Record<string, Schema>;
const definitions = Object.assign(native, ...[
  'codex-app-tools', 'phone-voice-archive', 'phone-journal', 'phone-admission', 'phone-service', 'phone-voice-work',
].map(load));
const validators = new SchemaValidators();

export function phoneServiceSchema(name: string): Record<string, unknown> {
  requireThat(Object.hasOwn(definitions, name), 'internal_error', 'Unknown Phone service contract.');
  // An alias includes the root inline and leaves only definitions reached by it.
  const bundled = bundleLocalSchema({ $defs: { ...definitions, $operatorRoot: definitions[name] } }, '$operatorRoot');
  const selected = bundled['$defs'] as Record<string, Schema>;
  delete selected['$operatorRoot'];
  return { ...structuredClone(definitions[name]), $defs: selected };
}

export function validatePhoneService(name: string, value: unknown): void {
  validators.validate(phoneServiceSchema(name), value, 256 * 1024);
}

export function validatePhone(name: string, value: unknown): void {
  validators.validate(phoneServiceSchema(name), value, 65536);
}
