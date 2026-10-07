import { randomUUID } from 'node:crypto';
import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { PhoneAdmission } from './admission.js';
import type { PhoneCall, PhoneScreening } from './admission.js';
import { PhoneJournal } from './journal.js';
import type { PhoneOperation, PhoneJournalIntent } from './journal.js';
import { PhoneNativeClient, phoneFrameBytes, validatePhone } from './native.js';
import type { PhoneMethod } from './native.js';

/** Original call command boundary. Voice preparation/readiness is coordinated by the service. */
export class PhoneCallCommands {
  private readonly pending = new Map<string, Promise<PhoneOperation>>();
  constructor(readonly admission: PhoneAdmission, readonly journal: PhoneJournal, private readonly native: PhoneNativeClient) {}
  async admitOutgoing(principalId: string, operationId: string, recipientId: string | null, route?: 'voice' | 'windows', screening?: PhoneScreening, destination?: string,
    voicePrompt?: string): Promise<PhoneCall> {
    const prior = this.journal.callForOperation(operationId);
    const admission = this.admission.outgoing(prior?.epoch ?? this.native.epoch, operationId, principalId, recipientId, route, screening, destination, voicePrompt);
    if (prior) return this.journal.admitCall(admission);
    const observed = await this.native.observe();
    if (this.journal.callForOperation(operationId)) return this.admitOutgoing(principalId, operationId, recipientId, route, screening, destination, voicePrompt);
    this.currentEpoch();
    requireThat(observed.configured && observed.endpoint !== null && (observed.callIds?.length ?? (observed.call ? 1 : 0)) < this.journal.maxConcurrentCalls, 'phone_call_busy', 'Native Phone must have a configured free call slot.');
    return this.journal.admitCall(admission);
  }
  async admitIncoming(principalId: string, operationId: string, callId: string, route?: 'voice' | 'windows'): Promise<PhoneCall> {
    const prior = this.journal.callForOperation(operationId);
    if (prior) {
      requireThat(prior.direction === 'incoming' && prior.callId === callId && prior.principalId === principalId && prior.policyHash === this.admission.hash && prior.route === route,
        'mutation_conflict', 'Original incoming admission arguments or policy changed.');
      this.admission.authorize(principalId, prior, 'read'); return prior;
    }
    const observed = await this.native.observe(callId);
    if (this.journal.callForOperation(operationId)) return this.admitIncoming(principalId, operationId, callId, route);
    this.currentEpoch();
    requireThat(observed.configured && observed.call?.id === callId, 'phone_call_conflict', 'The requested incoming call is no longer the original native call.');
    return this.journal.admitCall({ ...this.admission.incoming(this.native.epoch, operationId, principalId, observed.call), ...(route ? { route } : {}) });
  }
  private currentEpoch(): void {
    requireThat(this.native.epoch === this.journal.epoch, 'phone_epoch_mismatch', 'The observed native Phone epoch has retired.');
  }
  read(principalId: string, callId: string, method: PhoneJournalIntent['method'], generation = 0): PhoneOperation | null {
    this.call(principalId, callId, 'read'); return this.journal.callCommand(callId, method, generation);
  }
  private call(principalId: string, callId: string, action: 'read' | 'connect' | 'cleanup'): PhoneCall {
    const call = this.journal.getCall(callId);
    requireThat(call, 'phone_call_missing', 'Original call admission is unavailable.');
    this.admission.authorize(principalId, call, action); return call;
  }
  prepare(principalId: string, callId: string): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.prepare', { callId });
  }
  features(principalId: string, callId: string, challenge: Record<string, unknown> | null): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.features', { callId, challenge });
  }
  prepareScreening(principalId: string, callId: string, settings: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.screening.prepare', { callId, settings });
  }
  bridgeScreening(principalId: string, callId: string, settings: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.screening.bridge', { callId, settings });
  }
  connectWindows(principalId: string, callId: string, settings: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.windows.connect', { callId, settings });
  }
  upgradeCodec(principalId: string, callId: string): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.codec.upgrade', { callId });
  }
  dial(principalId: string, callId: string, credentials: { username: string | null; password: string | null }, ringSeconds: number, waiting = false): Promise<PhoneOperation> {
    const call = this.call(principalId, callId, 'connect');
    requireThat(call.direction === 'outgoing', 'phone_call_conflict', 'Incoming calls cannot be dialled.');
    return this.invoke(principalId, callId, 'call.dial', { callId, destination: call.destination, username: credentials.username, password: credentials.password, ringSeconds, waiting },
      Math.min(150000, (ringSeconds + 20) * 1000));
  }
  prepareAudio(principalId: string, callId: string, settings: Record<string, unknown>, desktop: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.audio.prepare', { callId, settings, desktop });
  }
  rebindAudio(principalId: string, callId: string, desktop: Record<string, unknown>, threadId: string): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.audio.rebind', { callId, desktop, threadId }, 20000);
  }
  answer(principalId: string, callId: string, waiting = false): Promise<PhoneOperation> { return this.invoke(principalId, callId, 'call.answer', { callId, waiting }, 20000); }
  endWaiting(principalId: string, callId: string): Promise<PhoneOperation> { return this.invoke(principalId, callId, 'call.waiting.end', { callId }); }
  feedback(principalId: string, callId: string, commandSequence: number, success: boolean): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.command.feedback', { callId, commandSequence, success });
  }
  claim(principalId: string, callId: string): Promise<PhoneOperation> { return this.invoke(principalId, callId, 'call.claim', { callId }); }
  launchDesktop(principalId: string, callId: string, application: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.desktop.launch', { callId, application });
  }
  startVoice(principalId: string, callId: string, desktop: Record<string, unknown>, voiceInput: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.desktop.startVoice', { callId, desktop, voiceInput }, 60000);
  }
  transitionVoice(principalId: string, callId: string, action: 'pauseVoice' | 'resumeVoice', generation: number,
    desktop: Record<string, unknown>, voiceInput: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.desktop.' + action as PhoneMethod, { callId, desktop, voiceInput, generation }, 40000);
  }
  stopVoice(principalId: string, callId: string, desktop: Record<string, unknown>, voiceInput: Record<string, unknown>): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.desktop.stopVoice', { callId, desktop, voiceInput }, 25000);
  }
  prepareRealtime(principalId: string, callId: string, generation: number, queueMs = 80): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.realtime.prepare', { callId, generation, queueMs }, 15000);
  }
  answerRealtime(principalId: string, callId: string, generation: number, sdp: string): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.realtime.answer', { callId, generation, sdp }, 20000);
  }
  stopRealtime(principalId: string, callId: string, generation: number): Promise<PhoneOperation> {
    return this.invoke(principalId, callId, 'call.realtime.stop', { callId, generation }, 20000);
  }
  hangup(principalId: string, callId: string): Promise<PhoneOperation> { return this.invoke(principalId, callId, 'call.hangup', { callId }); }
  release(principalId: string, callId: string): Promise<PhoneOperation> { return this.invoke(principalId, callId, 'call.release', { callId }, 30000); }
  cancelUnprepared(principalId: string, callId: string): void {
    this.call(principalId, callId, 'cleanup'); this.journal.discardUnpreparedCall(callId);
  }
  private invoke(principalId: string, callId: string, method: PhoneMethod, params: Record<string, unknown>, timeoutMs = 15000): Promise<PhoneOperation> {
    const cleanup = method === 'call.hangup' || method === 'call.release' || method === 'call.desktop.stopVoice' || method === 'call.realtime.stop';
    const call = this.call(principalId, callId, cleanup ? 'cleanup' : 'connect');
    const requestHash = digest(canonical({ method, params }, phoneFrameBytes));
    const generation = ['call.desktop.pauseVoice', 'call.desktop.resumeVoice'].includes(method) || method.startsWith('call.realtime.') ? Number(params['generation']) : 0;
    const prior = method === 'call.command.feedback'
      ? this.journal.feedbackCommand(callId, Number(params['commandSequence']))
      : this.journal.callCommand(callId, method, generation);
    const settle = (operation: PhoneOperation): PhoneOperation => {
      if (method.startsWith('call.desktop.') && operation.phase === 'result' && operation.receipt?.ok) {
        const result = operation.receipt.result; validatePhone('CallDesktopResult', result);
        requireThat(result !== null && typeof result === 'object' && 'callId' in result && result.callId === callId &&
          'action' in result && result.action === method.slice('call.desktop.'.length), 'phone_receipt_conflict', 'Desktop receipt belongs to another original call or action.');
      }
      if (method === 'call.release' && operation.phase === 'result' && operation.receipt?.ok) this.journal.releaseCall(callId, operation.intent.operationId);
      if (method === 'call.command.feedback' && operation.phase === 'result' && operation.receipt?.ok) {
        const value = operation.receipt.result;
        requireThat(value !== null && typeof value === 'object' && 'callId' in value && value.callId === callId &&
          'commandSequence' in value && value.commandSequence === params['commandSequence'] &&
          'success' in value && value.success === params['success'],
          'phone_receipt_conflict', 'Keypad feedback receipt differs from the original command.');
      }
      if (method === 'call.prepare' && operation.phase === 'result' && operation.receipt?.ok === false &&
          ['operation_conflict', 'runtime_not_ready'].includes(operation.receipt.error!)) this.journal.discardUnpreparedCall(callId);
      return operation;
    };
    if (prior) {
      requireThat(prior.intent.epoch === call.epoch && prior.intent.requestHash === requestHash, 'mutation_conflict', 'Original native call arguments changed.');
      if (prior.phase === 'result') return Promise.resolve(settle(prior));
      const pending = this.pending.get(prior.intent.operationId); if (pending) return pending;
      throw new IvyError('phone_operation_retained', 'Original native call outcome requires reconciliation; it cannot be dispatched again.', 'unknown');
    }
    requireThat(this.native.epoch === call.epoch && this.journal.epoch === call.epoch && this.journal.currentCall(callId),
      'phone_call_conflict', 'Only the original current call can dispatch another native command.');
    const operationId = randomUUID();
    const pending = this.native.request(method, params, operationId, timeoutMs).then(() => {
      const result = this.journal.get(operationId);
      requireThat(result, 'phone_receipt_missing', 'Original native receipt was not retained.'); return settle(result);
    }).finally(() => { this.pending.delete(operationId); });
    this.pending.set(operationId, pending); return pending;
  }
}
