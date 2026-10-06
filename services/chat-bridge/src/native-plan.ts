import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { checkedNativeContract } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import { readNativePlan, validateNativeInvocation } from '../../../packages/sdk/src/native-plan.js';
import { nativeRequestFrameBytes } from '../../agent-manager/src/limits.js';
import type { ChatStore } from './store.js';
import { hashJson } from '../../../packages/sdk/src/node.js';
import { mainThreadOptions } from './main-context.js';

const plans = new WeakMap<ChatStore, Map<string, ReturnType<typeof readNativePlan>>>();

export function validateChatNativeDraft(value: unknown): asserts value is Chat.NativeRequest {
  validateChat('NativeRequest', value); const request = value as Chat.NativeRequest;
  validateNativeInvocation(checkedNativeContract(request.nativeVersion, { sourceHash: request.catalogSourceHash }).contract, request, nativeRequestFrameBytes);
}
export async function resolveChatPlan(store: ChatStore, plan: Chat.NativePlan) {
  // A store belongs to one authenticated connection generation. Exact revision and
  // content/contract pins are immutable; current Main, owner and queue guards remain live.
  let cache = plans.get(store); if (!cache) { cache = new Map(); plans.set(store, cache); }
  const key = hashJson({ plan, parentId: store.rootObjectId });
  let read = cache.get(key);
  if (!read) {
    if (cache.size >= 4) cache.delete(cache.keys().next().value!);
    read = readNativePlan(store.client, checkedNativeContract(plan.nativeVersion, { sourceHash: plan.catalogSourceHash }).contract,
      { plan, kind: 'chat', parentId: store.rootObjectId });
    cache.set(key, read);
    void read.catch(() => { if (cache.get(key) === read) cache.delete(key); });
  }
  const resolved = structuredClone(await read);
  return { ...resolved, threadStart: mainThreadOptions(resolved.threadStart), threadResume: mainThreadOptions(resolved.threadResume) };
}

/** Native collaboration settings take precedence over top-level model/effort. */
export function chatTurnOptions(template:Record<string,Wire.Json>, options:Chat.Payload['turnOptions']):Record<string,Wire.Json>{
  const result:Record<string,Wire.Json>={...template,...options},mode=template['collaborationMode'];
  if(mode && typeof mode==='object' && !Array.isArray(mode)){
    const settings=mode['settings'];
    if(settings && typeof settings==='object' && !Array.isArray(settings))result['collaborationMode']={...mode,settings:{...settings,
      ...(options?.model?{model:options.model}:{}),...(options?.effort?{reasoning_effort:options.effort}:{})}};
  }
  return result;
}
