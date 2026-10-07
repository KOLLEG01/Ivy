import { isIP } from 'node:net';
import { hashJson } from '../../../../packages/sdk/src/node.js';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import { phoneValidator } from './validation.js';
import { validateShared } from '../../../../packages/sdk/src/node.js';
import definition from '../../../../specs/schemas/phone-admission.schema.json' with { type: 'json' };
import journal from '../../../../specs/schemas/phone-journal.schema.json' with { type: 'json' };
import runtime from '../../../../specs/schemas/phone-runtime.schema.json' with { type: 'json' };

export interface PhoneIncoming { sipCallId: string; fromUri: string; peerAddress: string; peerPort: number; transport: 'udp' | 'tcp' | 'tls'; assertedNumbers?: string[] | null; registrationPeer?: boolean }
export interface PhoneCallAdmission {
  screening?: PhoneScreening;
  route?: 'voice' | 'windows';
  voicePrompt?: string;
  operationId: string; epoch: string; principalId: string; policyHash: string; direction: 'incoming' | 'outgoing';
  callId: string | null; recipientId: string | null; destination: string | null; incoming: PhoneIncoming | null;
}
export interface PhoneScreening { announcement: string; timeoutSeconds: number; repeatCount: number }
export interface PhoneCall extends PhoneCallAdmission { callId: string }
export interface PhonePolicyDefinition {
  allowDirectDial?: boolean;
  dialDomain?: string;
  recipients: { id: string; destination: string }[];
  incoming: { peerAddress: string; transport: PhoneIncoming['transport']; fromUri: string; expectedAssertedNumber?: string }[];
  challengedIncoming?: { peerAddress: string; transport: PhoneIncoming['transport'] }[];
}
const validate = phoneValidator({ ...runtime.$defs, Uuid: journal.$defs.Uuid, ...definition.$defs });
export const phoneCallReservation = 64 * 1024;
export function validatePhoneAdmission(name: string, value: unknown): void {
  validate(name, value, name === 'PhonePolicy' ? 256 * 1024 : phoneCallReservation);
}
function freeze(value: unknown): void {
  if (value && typeof value === 'object') { for (const child of Object.values(value)) freeze(child); Object.freeze(value); }
}

/** Pure target and incoming-source policy. Native observations are supplied by the owner, never tool args. */
export class PhoneAdmission {
  readonly definition: PhonePolicyDefinition;
  readonly hash: string;
  constructor(value: PhonePolicyDefinition) {
    validatePhoneAdmission('PhonePolicy', value);
    for (const recipient of value.recipients) validateShared('Identifier', recipient.id);
    requireThat(new Set(value.recipients.map(item => item.id)).size === value.recipients.length, 'invalid_arguments', 'Phone recipient IDs must be unique.');
    requireThat([...value.incoming, ...(value.challengedIncoming ?? [])].every(item => item.peerAddress === 'registered' || isIP(item.peerAddress) !== 0),
      'invalid_arguments', 'Phone incoming policy needs an IP address or the registered SIP peer.');
    const incoming = value.incoming.map(({ peerAddress, transport, fromUri }) => hashJson({ peerAddress, transport, fromUri }));
    requireThat(new Set(incoming).size === incoming.length, 'invalid_arguments', 'Phone incoming rules must be unique.');
    this.definition = structuredClone(value); freeze(this.definition); this.hash = hashJson(this.definition);
  }
  outgoing(epoch: string, operationId: string, principalId: string, recipientId: string | null, route?: 'voice' | 'windows', screening?: PhoneScreening, destination?: string,
    voicePrompt?: string): PhoneCallAdmission {
    const recipient = this.definition.recipients.find(item => item.id === recipientId);
    const direct = recipientId === null && this.definition.allowDirectDial === true && !screening && route === 'windows';
    requireThat(recipient && destination === undefined || direct && destination, 'phone_destination_refused', 'The Phone destination or route is not configured.');
    requireThat(voicePrompt === undefined || route === 'voice' && !screening && recipient !== undefined,
      'phone_caller_refused', 'A Voice prompt requires an admitted configured Voice recipient.');
    requireThat(voicePrompt === undefined || voicePrompt.length <= 30000 && Buffer.byteLength(voicePrompt) <= 32000,
      'invalid_arguments', 'A Voice prompt must fit the native session startup text bound.');
    const resolved = this.resolveDestination(recipient?.destination ?? destination!);
    const value: PhoneCallAdmission = { epoch, operationId, principalId, policyHash: this.hash, direction: 'outgoing', callId: null,
      recipientId, destination: resolved, incoming: null, ...(route ? { route } : {}), ...(screening ? { screening } : {}),
      ...(voicePrompt ? { voicePrompt } : {}) };
    validatePhoneAdmission('PhoneCallAdmission', value); return value;
  }
  private resolveDestination(value: string): string {
    if (/^\+[1-9]\d{6,14}$/.test(value)) {
      requireThat(this.definition.dialDomain && /^[A-Za-z0-9.-]+(?::[0-9]{1,5})?$/.test(this.definition.dialDomain),
        'invalid_arguments', 'An E.164 destination requires a configured SIP dial domain.');
      return 'sip:' + value + '@' + this.definition.dialDomain;
    }
    requireThat(/^sips?:[^\s\x00-\x20]+$/.test(value) && value.length <= 2048, 'invalid_arguments', 'Expected E.164 or SIP destination.');
    return value;
  }
  matchesRecipient(call: PhoneCall, recipientId: string): boolean {
    const recipient = this.definition.recipients.find(item => item.id === recipientId);
    return !!recipient && (call.direction === 'outgoing'
      ? call.recipientId === recipientId
      : sameCaller(this.resolveDestination(recipient.destination), call.incoming!.fromUri));
  }
  incoming(epoch: string, operationId: string, principalId: string, observation: {
    id: string; direction: string; state: string; sipCallId: string | null; error: string | null; incoming: PhoneIncoming | null;
  }): PhoneCallAdmission {
    validatePhoneAdmission('SipCallObservation', observation);
    const context = observation.incoming;
    requireThat(observation.direction === 'incoming' && observation.state === 'ringing' && context && context.sipCallId === observation.sipCallId &&
      (this.allowsIncoming(context) || this.challengesIncoming(context)), 'phone_caller_refused', 'This original incoming call is not admitted by the Phone policy.');
    const value: PhoneCallAdmission = { epoch, operationId, principalId, policyHash: this.hash, direction: 'incoming', callId: observation.id,
      recipientId: null, destination: null, incoming: structuredClone(context) };
    validatePhoneAdmission('PhoneCallAdmission', value); return value;
  }
  private allowsIncoming(context: PhoneIncoming): boolean {
    return this.definition.incoming.some(item => matchesPeer(item.peerAddress, context) &&
      item.transport === context.transport && sameCaller(item.fromUri, context.fromUri) &&
      (item.expectedAssertedNumber === undefined || context.assertedNumbers?.includes(item.expectedAssertedNumber) === true));
  }
  challengesIncoming(context: PhoneIncoming): boolean {
    return !this.allowsIncoming(context) && (this.definition.challengedIncoming ?? []).some(item =>
      matchesPeer(item.peerAddress, context) && item.transport === context.transport);
  }
  authorize(_principalId: string, call: PhoneCall, action: 'read' | 'cleanup' | 'connect'): void {
    validatePhoneAdmission('PhoneCall', call);
    if (action !== 'connect') return;
    requireThat(call.policyHash === this.hash, 'phone_policy_changed', 'Original call policy changed; cleanup remains available.');
    if (call.direction === 'outgoing') {
      const current = this.outgoing(call.epoch, call.operationId, call.principalId, call.recipientId, call.route, call.screening,
        call.recipientId === null ? call.destination! : undefined, call.voicePrompt);
      requireThat(current.destination === call.destination, 'phone_policy_changed', 'Original recipient destination changed.');
    } else {
      requireThat(this.allowsIncoming(call.incoming!) || this.challengesIncoming(call.incoming!), 'phone_caller_refused', 'Original incoming caller policy is no longer admitted.');
    }
  }
}

/** Match the E.164 identity independently of SIP host/URI parameters, as IvySIP does.
 * Non-number SIP identities retain exact matching. Peer and asserted-identity checks remain. */
function sameCaller(expected: string, actual: string): boolean {
  return phoneCallerIdentity(expected) === phoneCallerIdentity(actual);
}
export function phoneCallerIdentity(uri: string): string {
  return /^(?:sips?:|tel:)(\+[1-9]\d{6,14})(?=@|;|$)/i.exec(uri)?.[1] ?? uri;
}
function matchesPeer(rule: string, context: PhoneIncoming): boolean {
  return rule === 'registered' ? context.registrationPeer === true : rule === context.peerAddress;
}
