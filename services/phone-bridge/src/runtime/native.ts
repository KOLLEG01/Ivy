import { randomUUID } from 'node:crypto';
import type { Readable, Writable } from 'node:stream';
import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import { phoneValidator } from './validation.js';
import type { Schema } from '../../../../packages/sdk/src/node.js';
import ipc from '../../../../specs/schemas/phone-ipc.schema.json' with { type: 'json' };
import runtime from '../../../../specs/schemas/phone-runtime.schema.json' with { type: 'json' };
import launch from '../../../../specs/schemas/phone-launch.schema.json' with { type: 'json' };
import hotkey from '../../../../specs/schemas/phone-hotkey.schema.json' with { type: 'json' };
import micro from '../../../../specs/schemas/phone-micro.schema.json' with { type: 'json' };
import capture from '../../../../specs/schemas/phone-capture.schema.json' with { type: 'json' };
import controls from '../../../../specs/schemas/phone-desktop-controls.schema.json' with { type: 'json' };
import voiceLifetime from '../../../../specs/schemas/phone-voice-lifetime.schema.json' with { type: 'json' };
import type { PhoneIncoming } from './admission.js';
import archive from '../../../../specs/schemas/phone-voice-archive.schema.json' with { type: 'json' };
import appTools from '../../../../specs/schemas/codex-app-tools.schema.json' with { type: 'json' };

export const phoneFrameBytes = 65536;
export type PhoneMethod = 'call.realtime.prepare' | 'call.realtime.answer' | 'call.realtime.stop' | 'call.desktop.pauseVoice' | 'call.desktop.resumeVoice' | 'audio.probe' | 'codec.test' | 'inventory' | 'registration.reconnect' | 'configure' | 'heartbeat' | 'status' | 'desktop.observe' | 'desktop.launch' | 'desktop.capture' | 'desktop.captureOwner' | 'desktop.process' | 'desktop.controls' | 'call.desktop.launch' | 'call.desktop.startVoice' | 'call.desktop.stopVoice' |
  'call.screening.prepare' | 'call.screening.bridge' | 'call.screening.synthesize' | 'call.features' | 'call.windows.connect' | 'call.codec.upgrade' | 'call.prepare' | 'call.claim' | 'call.audio.prepare' | 'call.audio.rebind' | 'call.dial' | 'call.answer' | 'call.waiting.end' | 'call.command.feedback' | 'call.hangup' | 'call.release' | 'shutdown';
export interface PhoneReply { version: 1; epoch: string; requestId: number; ok: boolean; result: unknown; error: string | null }
export interface PhoneIntent { epoch: string; operationId: string; callId: string | null; method: PhoneMethod; requestHash: string; voiceGeneration?: number; commandSequence?: number }
export interface PhoneDesktopObservation {
  state: 'absent' | 'waiting' | 'ready' | 'ambiguous' | 'unavailable';
  identity: { pid: number; startTimeUtcTicks: string; imagePath: string; appUserModelId: string } | null;
}
export interface PhoneDesktopLaunch {
  phase: 'ready' | 'waiting' | 'not_submitted' | 'submitted' | 'outcome_unknown';
  identity: PhoneDesktopObservation['identity']; activationPid: number | null; errorCode: string | null;
}
export interface PhoneCaptureObservation {
  endpointId: string; state: 'observed' | 'unavailable';
  sessions: { instanceId: string; state: 'active' | 'inactive' | 'expired'; pid: number; singleProcess: boolean }[];
}
export interface PhoneProcessObservation { pid: number; state: 'owned' | 'foreign' | 'unavailable' }
export interface PhoneDesktopCapture {
  endpointId: string; state: 'matched' | 'none' | 'ambiguous' | 'unavailable';
  identity: { instanceId: string; pid: number; processStartTimeUtcTicks: string } | null;
}
export interface PhoneDesktopControls {
  state: 'observed' | 'unavailable';
  windows: { handle: string; minimized: boolean;
    controls: { id: number[]; parentId: number[]; name: string; enabled: boolean; offscreen: boolean; toggle: number | null }[] }[];
}
export interface PhoneStatus {
  micro?: { state: string; generation: number | null; errorCode: string | null } | null;
  callIds?: string[];
  configured: boolean; endpoint: string | null; registration: Record<string, unknown> | null;
  call: { id: string; direction: 'incoming' | 'outgoing'; state: string; sipCallId: string | null; error: string | null; incoming: PhoneIncoming | null;
    features?: { access: string; disableCodecUpgrade: boolean; screening: string; command: string | null; commandSequence: number;
      commandState?: 'pending' | null; model?: string | null; reasoningEffort?: string | null;
      commands: { command: 'new_voice' | 'select_voice'; sequence: number; model: string | null; reasoningEffort: string | null }[];
      commandOverflow: boolean } | null } | null;
  media: { closed: boolean; failed: boolean; sentPackets: number; receivedPackets: number;
    receive: Record<string, number>; audio: { callId: string; state: string; route: Record<string, unknown> | null } | null } | null;
}
export interface PhoneReceiptHooks {
  beforeSend: (intent: PhoneIntent) => Promise<void>;
  beforeResolve: (intent: PhoneIntent, reply: PhoneReply) => Promise<void>;
}
interface Pending { resolve: (value: PhoneReply) => void; reject: (error: IvyError) => void; timer: NodeJS.Timeout; intent: PhoneIntent | null; receiving: boolean }
const readonlyMethods = new Set<PhoneMethod>(['audio.probe', 'codec.test', 'inventory', 'heartbeat', 'status', 'desktop.observe', 'desktop.capture', 'desktop.captureOwner', 'desktop.process', 'desktop.controls']);
const controlMethods = new Set<string>(ipc.$defs.ControlMethod.enum);
const identifiers = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const definitions = JSON.parse(JSON.stringify({ ...appTools.$defs, ...archive.$defs, ...launch.$defs, ...hotkey.$defs, ...micro.$defs, ...capture.$defs, ...controls.$defs, ...voiceLifetime.$defs, ...runtime.$defs, ...ipc.$defs })
  .replaceAll('phone-runtime.schema.json#/$defs/', '#/$defs/').replaceAll('phone-launch.schema.json#/$defs/', '#/$defs/')
  .replaceAll('phone-hotkey.schema.json#/$defs/', '#/$defs/').replaceAll('phone-desktop-controls.schema.json#/$defs/', '#/$defs/')
  .replaceAll('phone-micro.schema.json#/$defs/', '#/$defs/').replaceAll('phone-voice-lifetime.schema.json#/$defs/', '#/$defs/')) as Record<string, Schema>;
const validate = phoneValidator(definitions);
export function phoneSchema(name: string): Record<string, unknown> {
  requireThat(Object.hasOwn(definitions, name), 'internal_error', 'Unknown Phone contract.');
  return { $defs: structuredClone(definitions), $ref: '#/$defs/' + name };
}
export function validatePhone(name: string, value: unknown): void {
  validate(name, value, phoneFrameBytes);
}

/** One authenticated inherited pipe epoch, with durable hooks before effects/results. Never retries. */
export class PhoneNativeClient {
  readonly closed: Promise<string>;
  private finishClosed!: (code: string) => void;
  private ended = false;
  private sequence = 0;
  private fragments: Buffer[] = [];
  private buffered = 0;
  private readonly pending = new Map<number, Pending>();
  private readonly expiredReads = new Set<number>();
  private readonly expiredEffects = new Map<number, Pending>();
  private heartbeat: NodeJS.Timeout | null = null;
  private heartbeating = false;
  private heartbeatFailures = 0;
  constructor(readonly epoch: string, private readonly input: Writable, output: Readable, private readonly hooks: PhoneReceiptHooks) {
    requireThat(identifiers.test(epoch), 'invalid_arguments', 'Phone native owner requires an exact epoch UUID.');
    this.closed = new Promise(resolve => { this.finishClosed = resolve; });
    input.once('error', () => this.close('phone_write_failed'));
    output.on('data', (chunk: Buffer) => this.receive(chunk));
    output.once('error', () => this.close('phone_read_failed'));
    output.once('end', () => this.close(this.buffered ? 'phone_truncated_frame' : 'phone_pipe_closed'));
  }
  startHeartbeat(): void {
    requireThat(!this.ended, 'phone_unavailable', 'Phone native owner is closed.');
    if (this.heartbeat) return;
    const tick = () => {
      if (this.heartbeating || this.ended) return; this.heartbeating = true;
      void this.request('heartbeat', {}, null, 5000).then(reply => {
        this.heartbeatFailures = 0;
        if (!reply.ok) this.close('phone_heartbeat_rejected');
      }, () => {
        if (++this.heartbeatFailures >= 3) this.close('phone_heartbeat_lost');
      }).finally(() => { this.heartbeating = false; });
    };
    this.heartbeat = setInterval(tick, 1000); tick();
  }
  async observe(callId?: string): Promise<PhoneStatus> {
    const reply = await this.request('status', callId ? { callId } : {}, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Original native Phone status is unavailable.');
    validatePhone('Status', reply.result); const status = reply.result as PhoneStatus;
    requireThat((status.call === null) === (status.media === null) &&
      (!status.media?.audio || status.media.audio.callId === status.call?.id) &&
      (status.configured || status.call === null && status.registration === null && status.endpoint === null),
    'phone_invalid_status', 'Native status has inconsistent call or configuration ownership.');
    if (status.call) requireThat(status.call.direction === 'incoming' ?
      status.call.incoming !== null && status.call.incoming.sipCallId === status.call.sipCallId : status.call.incoming === null,
    'phone_invalid_status', 'Native status has inconsistent original caller context.');
    return status;
  }
  async observeDesktop(appUserModelId: string): Promise<PhoneDesktopObservation> {
    const reply = await this.request('desktop.observe', { appUserModelId }, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Original Desktop observation is unavailable.');
    validatePhone('DesktopObservation', reply.result); const observed = reply.result as PhoneDesktopObservation;
    requireThat(!observed.identity || observed.identity.appUserModelId === appUserModelId, 'phone_invalid_status', 'Observed Desktop belongs to another configured application.');
    return observed;
  }
  async launchDesktop(application: { appUserModelId: string; startIfMissing: boolean }, operationId: string): Promise<PhoneDesktopLaunch> {
    const reply = await this.request('desktop.launch', application, operationId, 15000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Desktop activation did not return a confirmed result.');
    validatePhone('LaunchResult', reply.result); const launched = reply.result as PhoneDesktopLaunch;
    requireThat(!launched.identity || launched.identity.appUserModelId === application.appUserModelId,
      'phone_invalid_status', 'Desktop activation returned another configured application.');
    return launched;
  }
  async observeControls(desktop: NonNullable<PhoneDesktopObservation['identity']>, labels: string[]): Promise<PhoneDesktopControls> {
    const reply = await this.request('desktop.controls', { desktop, labels }, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Original Desktop controls are unavailable.');
    validatePhone('Observation', reply.result); const observed = reply.result as PhoneDesktopControls;
    const windows = new Set<string>(), ids = new Set<string>();
    for (const window of observed.windows) {
      requireThat(!windows.has(window.handle), 'phone_invalid_status', 'Duplicate Desktop window observation.'); windows.add(window.handle);
      for (const control of window.controls) {
        const id = JSON.stringify(control.id);
        requireThat(!ids.has(id) && id !== JSON.stringify(control.parentId) && labels.includes(control.name),
          'phone_invalid_status', 'Desktop control identity or label differs from the original scoped observation.'); ids.add(id);
      }
    }
    return observed;
  }
  async observeCapture(endpointId: string): Promise<PhoneCaptureObservation> {
    const reply = await this.request('desktop.capture', { endpointId }, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Original capture observation is unavailable.');
    validatePhone('CaptureObservation', reply.result); const observed = reply.result as PhoneCaptureObservation;
    requireThat(observed.endpointId === endpointId && new Set(observed.sessions.map(session => session.instanceId)).size === observed.sessions.length &&
      observed.sessions.every(session => !session.singleProcess || session.pid > 0),
      'phone_invalid_status', 'Capture observation belongs to another endpoint or repeats a session.');
    return observed;
  }
  async observeAudioProcess(desktop: NonNullable<PhoneDesktopObservation['identity']>, pid: number): Promise<PhoneProcessObservation> {
    const reply = await this.request('desktop.process', { desktop, pid }, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Original audio-process observation is unavailable.');
    validatePhone('ProcessOwnership', reply.result); const observed = reply.result as PhoneProcessObservation;
    requireThat(observed.pid === pid, 'phone_invalid_status', 'Audio-process observation belongs to another process.'); return observed;
  }
  async observeDesktopCapture(desktop: NonNullable<PhoneDesktopObservation['identity']>, endpointId: string): Promise<PhoneDesktopCapture> {
    const reply = await this.request('desktop.captureOwner', { desktop, endpointId }, null, 5000);
    requireThat(reply.ok && !this.ended, 'phone_unavailable', 'Current Desktop capture correlation is unavailable.');
    validatePhone('DesktopCaptureCorrelation', reply.result); const observed = reply.result as PhoneDesktopCapture;
    requireThat(observed.endpointId === endpointId, 'phone_invalid_status', 'Capture correlation belongs to another endpoint.'); return observed;
  }
  private writable(): void {
    requireThat(!this.ended && !this.input.destroyed && !this.input.writableEnded && this.input.writable, 'phone_unavailable', 'Phone pipe is unavailable.');
    requireThat(this.input.writableLength < phoneFrameBytes * 2, 'phone_capacity', 'Phone command pipe is full.');
  }
  async request(method: PhoneMethod, args: Record<string, unknown>, operationId: string | null, timeoutMs = 15000): Promise<PhoneReply> {
    this.writable();
    requireThat(Number.isSafeInteger(timeoutMs) && timeoutMs > 0 && timeoutMs <= 150000, 'invalid_arguments', 'Phone request timeout must be bounded.');
    requireThat(readonlyMethods.has(method) ? operationId === null : operationId !== null && identifiers.test(operationId), 'invalid_arguments', 'Phone effects require the original operation UUID.');
    const reservedControl = controlMethods.has(method);
    requireThat(this.pending.size < (reservedControl ? 48 : 32), 'phone_capacity', 'Phone request capacity is full.');
    const requestId = ++this.sequence, params = JSON.parse(canonical(args, phoneFrameBytes)) as Record<string, unknown>;
    const request = { version: 1, epoch: this.epoch, requestId, operationId, method, params };
    validatePhone('Request', request);
    const shape = method === 'call.realtime.prepare' ? 'PrepareRealtime' : method === 'call.realtime.answer' ? 'AnswerRealtime' : method === 'call.realtime.stop' ? 'StopRealtime' :
      method === 'call.desktop.pauseVoice' || method === 'call.desktop.resumeVoice' ? 'CallDesktopVoiceTransition' :
      method === 'status' && params['callId'] ? 'CallIdentity' : method === 'call.screening.prepare' ? 'PrepareScreening' : method === 'call.features' ? 'ConfigureFeatures' :
      method === 'call.windows.connect' || method === 'call.screening.bridge' ? 'ConnectWindows' :
      method === 'configure' ? 'Configure' : method === 'call.dial' ? 'Dial' : method === 'call.answer' ? 'Answer' : method === 'call.command.feedback' ? 'CommandFeedback' : method === 'call.audio.prepare' ? 'PrepareAudio' : method === 'call.audio.rebind' ? 'RebindAudio' :
      method === 'desktop.observe' ? 'ObserveDesktop' : method === 'desktop.launch' ? 'DesktopApplication' : method === 'desktop.capture' ? 'ObserveCapture' : method === 'desktop.captureOwner' ? 'ObserveCaptureOwner' : method === 'desktop.controls' ? 'ObserveControls' : method === 'desktop.process' ? 'ObserveProcess' :
      method === 'call.desktop.launch' ? 'CallDesktopLaunch' :
      method === 'call.desktop.startVoice' || method === 'call.desktop.stopVoice' ? 'CallDesktopVoiceInput' : method.startsWith('call.') ? 'CallIdentity' : null;
    if (shape) validatePhone(shape, params); else requireThat(Object.keys(params).length === 0, 'invalid_arguments', 'Phone control commands have no arguments.');
    if (method.startsWith('call.')) requireThat(typeof params['callId'] === 'string' && identifiers.test(params['callId']), 'invalid_arguments', 'Phone commands require the exact call UUID.');
    const encoded = canonical(request, phoneFrameBytes), intent: PhoneIntent | null = operationId === null ? null :
      { epoch: this.epoch, operationId, callId: method.startsWith('call.') ? params['callId'] as string : null, method,
        ...(['call.desktop.pauseVoice', 'call.desktop.resumeVoice'].includes(method) || method.startsWith('call.realtime.') ? { voiceGeneration: Number(params['generation']) } : {}),
        ...(method === 'call.command.feedback' ? { commandSequence: Number(params['commandSequence']) } : {}),
        requestHash: digest(canonical({ method, params }, phoneFrameBytes)) };
    if (intent) await this.hooks.beforeSend(intent);
    this.writable(); // Durable intent may have taken time while the original process exited.
    requireThat(this.pending.size < (reservedControl ? 48 : 32), 'phone_capacity', 'Phone request capacity changed before dispatch.');
    return new Promise<PhoneReply>((resolve, reject) => {
      const timer = setTimeout(() => {
        if (intent) {
          if (this.expiredEffects.size >= 64) { this.close('phone_request_timeout'); return; }
          const original = this.pending.get(requestId);
          if (!original) return;
          this.pending.delete(requestId);
          this.expiredEffects.set(requestId, original);
          reject(new IvyError('phone_request_timeout', 'The original Phone command is still unconfirmed.', 'unknown'));
          return;
        }
        if (this.expiredReads.size >= 64) { this.close('phone_request_timeout'); return; }
        this.pending.delete(requestId); this.expiredReads.add(requestId);
        reject(new IvyError('phone_request_timeout', 'Phone observation timed out; active calls remain owned.'));
      }, timeoutMs);
      this.pending.set(requestId, { resolve, reject, timer, intent, receiving: false });
      try { this.input.write(encoded + '\n', error => { if (error) this.close('phone_write_failed'); }); }
      catch { this.close('phone_write_failed'); }
    });
  }
  private receive(chunk: Buffer): void {
    if (this.ended) return;
    let offset = 0;
    try {
      while (offset < chunk.length) {
        const newline = chunk.indexOf(10, offset), end = newline < 0 ? chunk.length : newline;
        const part = chunk.subarray(offset, end); this.buffered += part.length;
        requireThat(this.buffered <= phoneFrameBytes, 'phone_invalid_frame', 'Native Phone response exceeds its frame limit.');
        this.fragments.push(part); offset = end + 1;
        if (newline < 0) break;
        const bytes = Buffer.concat(this.fragments, this.buffered); this.fragments = []; this.buffered = 0;
        const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
        const value = JSON.parse(text) as PhoneReply; validatePhone('Response', value);
        requireThat(value.epoch === this.epoch && value.ok === (value.error === null) && (value.ok || value.result === null), 'phone_invalid_frame', 'Native Phone response ownership/outcome mismatch.');
        if (this.expiredReads.delete(value.requestId)) continue;
        const expired = this.expiredEffects.get(value.requestId);
        if (expired) {
          requireThat(!expired.receiving, 'phone_invalid_frame', 'Native Phone response repeated its original late receipt.');
          expired.receiving = true;
          void this.resolveLate(value, expired);
          continue;
        }
        const pending = this.pending.get(value.requestId);
        requireThat(pending && !pending.receiving, 'phone_invalid_frame', 'Native Phone response has no unconsumed original request.');
        pending.receiving = true;
        // Remove only after durable receipt storage; a concurrent pipe loss still reports unknown.
        void this.resolve(value, pending);
      }
    } catch { this.close('phone_invalid_frame'); }
  }
  private async resolve(value: PhoneReply, pending: Pending): Promise<void> {
    try {
      if (pending.intent) await this.hooks.beforeResolve(pending.intent, value);
      if (this.pending.get(value.requestId) !== pending) {
        if (this.expiredEffects.get(value.requestId) === pending) this.expiredEffects.delete(value.requestId);
        return;
      }
      this.pending.delete(value.requestId); clearTimeout(pending.timer); pending.resolve(value);
    } catch { this.close('phone_receipt_save_failed'); }
  }
  private async resolveLate(value: PhoneReply, pending: Pending): Promise<void> {
    try {
      if (pending.intent) await this.hooks.beforeResolve(pending.intent, value);
      if (this.expiredEffects.get(value.requestId) === pending) this.expiredEffects.delete(value.requestId);
    } catch { this.close('phone_receipt_save_failed'); }
  }
  close(code = 'phone_owner_stopping'): void {
    if (this.ended) return; this.ended = true;
    if (this.heartbeat) clearInterval(this.heartbeat); this.heartbeat = null;
    this.fragments = []; this.buffered = 0;
    for (const item of this.pending.values()) { clearTimeout(item.timer); item.reject(new IvyError(code, 'The original Phone command outcome requires reconciliation.', 'unknown')); }
    this.pending.clear(); this.expiredReads.clear(); this.expiredEffects.clear(); this.finishClosed(code);
  }
}

export const newPhoneEpoch = (): string => randomUUID();
