import { requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import { ChatOperations } from './operations.js';

/** Channel filtering precedes reading selected message content, including cancellation targets. */
export class ChatQueries {
  constructor(readonly operations: ChatOperations) {}
  async input(caller: string, request: Chat.InputQuery): Promise<Chat.InputView> {
    const { admission, store } = this.operations;
    admission.authorize(caller, request.expectedBridge); validateChat('InputQuery', request);
    const channels: Wire.QueryPredicate[] = admission.channelsFor(caller).map(value => ({ op: 'and', args:
      Object.entries(value.channel).map(([key, item]) => ({ op: 'eq', field: 'data:/identity/channel/' + key, value: item })) }));
    const root: Wire.QueryPredicate = store.rootObjectId === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: store.rootObjectId };
    let reference: Chat.ObjectPin | null = null;
    // The definition supports32channels; Hive admits64predicate nodes per query. Keep each
    // admitted channel conjunction intact in at most four bounded, content-free lookups.
    for (let offset = 0; offset < channels.length; offset += 8) {
      const page = await store.client.request('objects.query', { contractKey: 'chat-bridge/input', limit: 1, where: { op: 'and', args: [root,
        { op: 'eq', field: 'object.id', value: request.inputId }, { op: 'eq', field: 'data:/workspaceId', value: admission.definition.workspaceId },
        { op: 'eq', field: 'data:/definitionHash', value: admission.definitionHash }, { op: 'or', args: channels.slice(offset, offset + 8) }] } });
      requireThat(!page.nextCursor, 'chat_identity_conflict', 'An exact input identity must resolve to one Object.');
      const item = page.items[0]; if (item) { reference = { objectId: item.objectId, revision: item.revision }; break; }
    }
    requireThat(reference, 'chat_input_unavailable', 'The selected input is unavailable in the caller\'s admitted channels.');
    const input = await store.read('chat-bridge/input', reference);
    admission.authorize(caller, request.expectedBridge, input.value.identity.channel);
    requireThat(input.value.workspaceId === admission.definition.workspaceId && input.value.definitionHash === admission.definitionHash,
      'chat_scope_mismatch', 'Input queries require the original configured workspace.');
    return { object: input.pin, data: input.value };
  }
}
