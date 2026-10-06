import { bundledSchema } from '../../../packages/sdk/src/node.js';
import { validateChat } from '../../../packages/sdk/src/node.js';
import type { Chat, Wire } from '../../../packages/sdk/src/node.js';
import catalog from '../../../specs/schemas/chat.operations.json' with { type: 'json' };

export const version = '1.3.0' as const;
export const readableVersions: readonly string[] = ['1.2.0', version];
export const contractNames = { 'chat-bridge/conversation': 'Conversation', 'chat-bridge/main': 'Main', 'chat-bridge/binding': 'Binding', 'chat-bridge/native-call': 'NativeCall',
  'chat-bridge/input': 'Input', 'chat-bridge/evidence': 'Evidence', 'chat-bridge/collection': 'Collection', 'chat-bridge/result': 'Result', 'chat-bridge/reply': 'Reply',
  'chat-bridge/offer': 'Offer', 'chat-bridge/acknowledgement': 'Acknowledgement', 'chat-bridge/operation': 'Operation' } as const;
export interface ContractValues {
  'chat-bridge/conversation': Chat.Conversation; 'chat-bridge/main': Chat.Main; 'chat-bridge/binding': Chat.Binding; 'chat-bridge/native-call': Chat.NativeCall;
  'chat-bridge/input': Chat.Input; 'chat-bridge/evidence': Chat.Evidence; 'chat-bridge/collection': Chat.Collection; 'chat-bridge/result': Chat.Result;
  'chat-bridge/reply': Chat.Reply; 'chat-bridge/offer': Chat.Offer; 'chat-bridge/acknowledgement': Chat.Acknowledgement;
  'chat-bridge/operation': Chat.Operation;
}
export type ContractKey = keyof ContractValues;
export const localContractKeys = new Set<ContractKey>(['chat-bridge/main', 'chat-bridge/binding', 'chat-bridge/native-call', 'chat-bridge/evidence',
  'chat-bridge/collection', 'chat-bridge/offer', 'chat-bridge/acknowledgement', 'chat-bridge/operation']);
export function validateDomain<K extends ContractKey>(key: K, value: unknown): asserts value is ContractValues[K] { validateChat(contractNames[key], value); }
export const chatSchema = (name: string) => bundledSchema('#/$defs/' + name, catalog.schema);
export function chatContracts(): Wire.DataContract[] {
  const retention: Wire.RetentionPolicy = { objects: { mode: 'expire', maximumAgeDays: 90 }, revisions: { mode: 'current' } };
  const conversationRetention: Wire.RetentionPolicy = { objects: { mode: 'retain' }, revisions: { mode: 'current' } };
  const artifactRetention: Wire.RetentionPolicy = { objects: { mode: 'owned' }, revisions: { mode: 'current' } };
  const finalKeys = new Set(['chat-bridge/conversation', 'chat-bridge/input', 'chat-bridge/result', 'chat-bridge/reply']);
  return [...catalog.contracts.filter(value => finalKeys.has(value.key)).map(value => ({ key: value.key, version, owner: { kind: 'service' as const, serviceName: 'chat-bridge' },
    mediaType: 'application/json', retention: value.key === 'chat-bridge/conversation' ? conversationRetention : retention, jsonSchema: bundledSchema(value.definition, catalog.schema),
    specMarkdown: `# ${value.key} ${version}\n\nChatBridge owns this final user-visible conversation record. Conversation identity is retained; completed turn records expire after 90 days. Original caller, input, binding and result identities remain mandatory across configuration changes.` })),
    ...(catalog.mediaContracts as Omit<Wire.DataContract, 'retention'>[]).filter(value => value.key !== 'chat-bridge/evidence-chunk').map(value => ({ ...value, retention: artifactRetention }))];
}
