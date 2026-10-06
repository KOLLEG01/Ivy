import { publishBrowserNotice } from '../../../packages/sdk/src/node.js';
import type { SecretaryEngine } from './engine.js';

/** Contact policy is checked by the caller before any browser notice is published. */
export async function secretaryBrowserNotice(engine: SecretaryEngine, tag: string, text: string): Promise<void> {
  const key = 'secretary/browser-notice';
  if (engine.store.technicalNamed(key, tag)) return;
  await publishBrowserNotice(engine.client, 'secretary', {
    title: 'Ivy · Secretary', body: text, tag,
    target: { uiId: 'secretary-ui', fragment: '#/journal?' + new URLSearchParams({ node: engine.owner.serviceNodeId }) },
  });
  engine.store.technicalCreate(key, tag, { expiresAt: engine.now().getTime() + 8 * 86_400_000 });
}
