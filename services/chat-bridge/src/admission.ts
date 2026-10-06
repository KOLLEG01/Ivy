import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { requireThat } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';

const freeze = (value: unknown): void => {
  if (value && typeof value === 'object') { for (const item of Object.values(value)) freeze(item); Object.freeze(value); }
};
export const sameChannel = (a: Chat.Channel, b: Chat.Channel): boolean => a.adapter === b.adapter && a.accountId === b.accountId && a.channelId === b.channelId;

/** Pure transport admission. Call before any message, operation, scheduling or content logging I/O. */
export class ChatAdmission {
  readonly definition: Chat.Definition;
  readonly definitionHash: string;
  constructor(definition: Chat.Definition) {
    validateChat('Definition', definition);
    requireThat(Buffer.byteLength(canonical(definition)) <= 128 * 1024, 'limit_exceeded', 'ChatBridge definition exceeds its canonical byte limit.');
    requireThat(definition.project.namespace === 'codex' && definition.project.kind === 'project', 'chat_definition_mismatch', 'ChatBridge needs an explicitly configured native project owner.');
    const identities = definition.channels.map(value => hashJson(value.channel));
    requireThat(new Set(identities).size === identities.length, 'chat_definition_mismatch', 'ChatBridge channel identities must be unique.');
    this.definition = structuredClone(definition); freeze(this.definition); this.definitionHash = hashJson(this.definition);
  }
  channelsFor(_callerPrincipalId: string): Chat.ChannelConfiguration[] {
    return structuredClone(this.definition.channels);
  }
  expected(callerPrincipalId: string, role: 'reader' | 'notice' | 'either' = 'reader'): Chat.ExpectedBridge {
    void role;
    return { principalId: this.definition.principalId, callerPrincipalId, workspaceId: this.definition.workspaceId, definitionHash: this.definitionHash };
  }
  authorize(callerPrincipalId: string, expected: Chat.ExpectedBridge, channel?: Chat.Channel, role: 'reader' | 'notice' | 'either' = 'reader'): void {
    const actual = this.expected(callerPrincipalId, role);
    validateChat('ExpectedBridge', expected);
    requireThat(actual.principalId === expected.principalId && actual.callerPrincipalId === expected.callerPrincipalId &&
      actual.workspaceId === expected.workspaceId, 'chat_definition_mismatch', 'The request belongs to another Main or caller.');
    if (channel) {
      validateChat('Channel', channel);
      requireThat(this.definition.channels.some(value => sameChannel(value.channel, channel)),
        'chat_sender_refused', 'The exact account and channel are not configured.');
    }
  }
  send(callerPrincipalId: string, request: Chat.SendRequest): { identity: Chat.InputIdentity; requestHash: string } {
    // The channel and expected identity are checked before inspecting message/media content.
    this.authorize(callerPrincipalId, request.expectedBridge, request.channel);
    validateChat('SendRequest', request);
    requireThat(request.payload.text.length > 0 || request.payload.images.length > 0, 'invalid_arguments', 'A chat input needs text or an image.');
    requireThat(request.payload.images.reduce((total, image) => total + image.byteLength, 0) <= 4 * 1024 * 1024,
      'limit_exceeded', 'Chat input image bytes exceed the combined limit.');
    const identity = { channel: structuredClone(request.channel), senderPrincipalId: callerPrincipalId, messageId: request.messageId };
    return { identity, requestHash: hashJson({ workspaceId: this.definition.workspaceId, definitionHash: this.definitionHash, identity, binding: request.expectedBinding, payload: request.payload }) };
  }
}
