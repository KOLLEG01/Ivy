import { fileTypeFromBuffer } from 'file-type';
import { canonical, digest, hashJson, IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import type { Chat } from '../../../packages/sdk/src/node.js';
import type { ChatBridge } from './bridge.js';
import { identity } from './whatsapp/journal.js';
import type { WhatsAppJournal } from './whatsapp/journal.js';
import { phone } from './whatsapp/config.js';
import type { WhatsAppConfig } from './whatsapp/config.js';

interface SavedFile {
  caller: string;
  expectedBridge: Chat.ExpectedBridge;
  requestHash: string;
  transportHash: string;
  delivery: Chat.FileDelivery;
  bytesBase64: string | null;
  caption: string | null;
}
const keyFor = (caller: string, operationId: string) => 'file:' + identity(caller, operationId);
const messageId = (caller: string, operationId: string) => identity('file', caller, operationId).slice(0, 32).toUpperCase();
const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
const maxFileBytes = 8 * 1024 * 1024;
const retentionMs = 7 * 86400_000;
const deliveryStates: Record<string, Chat.FileDelivery['state']> = {
  pending: 'queued', sending: 'sending', sent: 'sent', unknown: 'outcome_unknown', failed: 'failed',
};

/** Content admission, not an assertion that a recipient opened or decoded a document. */
export async function outgoingFileBytes(file: Chat.SendFileRequest['file']): Promise<{ bytes: Buffer; kind: 'image' | 'document' }> {
  const bytes = Buffer.from(file.bytesBase64, 'base64');
  requireThat(bytes.length > 0 && bytes.length <= maxFileBytes && bytes.toString('base64') === file.bytesBase64 &&
    bytes.length === file.byteLength && digest(bytes) === file.contentHash,
  'chat_file_integrity', 'Canonical base64, byte length and SHA-256 must match the selected file (at most 8 MiB).');
  let detected: Awaited<ReturnType<typeof fileTypeFromBuffer>>;
  try { detected = await fileTypeFromBuffer(bytes); }
  catch { throw new IvyError('chat_file_type', 'The file container is incomplete or invalid.'); }
  if (detected) {
    requireThat(file.mediaType === detected.mime, 'chat_file_type', 'Declared MIME does not match the file signature: ' + detected.mime + '.');
  } else {
    let text: string | null = null;
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { /* Binary unknowns require the generic type. */ }
    const readable = text !== null && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(text);
    let valid = file.mediaType === 'application/octet-stream';
    if (readable) {
      valid ||= file.mediaType.startsWith('text/');
      if (file.mediaType === 'application/json') { try { JSON.parse(text!); valid = true; } catch { /* Reject invalid JSON. */ } }
      if (['application/xml', 'image/svg+xml'].includes(file.mediaType))
        valid ||= file.mediaType === 'image/svg+xml' ? /^\s*(?:<\?xml[^>]*>\s*)?<svg[\s>]/i.test(text!) : /^\s*</.test(text!);
    }
    requireThat(valid, 'chat_file_type', 'Unknown binary formats require application/octet-stream; text must be valid UTF-8.');
  }
  const kind = ['image/png', 'image/jpeg'].includes(file.mediaType) ? 'image' : 'document';
  requireThat(kind !== 'image' || bytes.length <= 4 * 1024 * 1024, 'limit_exceeded', 'Native PNG/JPEG chat images are limited to 4 MiB.');
  return { bytes, kind };
}

/** File bytes and intent share the existing durable WhatsApp journal and ordered output queue. */
export class ChatFiles {
  constructor(readonly journal: WhatsAppJournal, readonly config: WhatsAppConfig, readonly bridge: () => ChatBridge | null) {}
  private get transportHash() { return hashJson({ account: this.config.accountId, bot: this.config.botPhoneNumber, sender: this.config.senders[0] }); }
  async target(caller: string): Promise<Chat.FileTarget> {
    const bridge = this.bridge();
    requireThat(bridge, 'service_not_ready', 'The Main file transport is not attached.');
    const workspace = await bridge.workspace(caller);
    requireThat(workspace.binding && workspace.channels.length === 1 && workspace.channels[0]!.channel.adapter === 'whatsapp',
      'chat_main_missing', 'File delivery requires the current Main and its single WhatsApp channel.');
    const channel = workspace.channels[0]!.channel, sender = this.config.senders[0];
    requireThat(sender && this.config.accountId === channel.accountId && sender.channelId === channel.channelId && phone(sender.phoneNumber),
      'chat_file_target_changed', 'The configured WhatsApp transport does not match Main.');
    return { expectedBridge: workspace.expectedBridge, expectedBinding: workspace.binding.object, channel };
  }
  private saved(id: string): SavedFile | null {
    const key = this.journal.get<string>('out-file:' + id);
    return key ? this.journal.get<SavedFile>(key) : null;
  }
  has(caller: string, operationId: string): boolean {
    const id = messageId(caller, operationId);
    return !!this.journal.get(keyFor(caller, operationId)) || !!this.journal.db.prepare('SELECT 1 FROM retired_outgoing WHERE id=?').get(id);
  }
  isFile(id: string): boolean { return this.journal.get('out-file:' + id) !== null; }
  private view(saved: SavedFile): Chat.FileDelivery {
    const row = this.journal.db.prepare('SELECT state FROM outgoing WHERE id=?').get(saved.delivery.messageId);
    requireThat(row, 'storage_unavailable', 'The file delivery lost its original output identity.');
    const state = deliveryStates[String(row['state'])];
    requireThat(state, 'storage_unavailable', 'Unknown file delivery state.');
    return { ...saved.delivery, state, ...this.journal.get<{ errorCode: string | null; updatedAt: string }>('file-state:' + saved.delivery.messageId) };
  }
  async read(caller: string, query: Chat.FileQuery): Promise<Chat.FileReadResult> {
    if (query.fileTarget) return this.target(caller);
    this.prune();
    const saved = this.journal.get<SavedFile>(keyFor(caller, query.fileOperationId!));
    requireThat(saved?.caller === caller, 'not_found', 'This caller has no retained file delivery with that operationId.');
    await this.journal.flush();
    return this.view(saved);
  }
  async send(caller: string, original: Chat.SendFileRequest): Promise<Chat.FileDelivery> {
    const request = structuredClone(original), bridge = this.bridge();
    requireThat(bridge, 'service_not_ready', 'The Main file transport is not attached.');
    bridge.main.admission.authorize(caller, request.expectedBridge);
    requireThat(request.expectedBridge.definitionHash === bridge.main.admission.definitionHash,
      'chat_file_target_changed', 'Read the current Main file target before sending.');
    const key = keyFor(caller, request.operationId), requestHash = hashJson(request), id = messageId(caller, request.operationId);
    const prior = this.journal.get<SavedFile>(key);
    if (prior) {
      requireThat(prior.caller === caller && prior.requestHash === requestHash, 'chat_identity_conflict', 'An existing file operation cannot change its bytes, name, caption or target.');
      await this.journal.flush(); return this.view(prior);
    }
    requireThat(!this.has(caller, request.operationId), 'operation_expired', 'This file operation has expired; its transport identity cannot be reused.');
    const target = await this.target(caller);
    requireThat(same(request.expectedBinding, target.expectedBinding), 'chat_file_target_changed', 'Main changed; a file cannot silently follow another conversation.');
    const { kind } = await outgoingFileBytes(request.file);
    // Recheck after content inspection and before the synchronous journal transaction.
    requireThat(this.bridge() === bridge && same(target, await this.target(caller)), 'chat_file_target_changed', 'Main changed during file admission.');
    const concurrent = this.journal.get<SavedFile>(key);
    if (concurrent) {
      requireThat(concurrent.requestHash === requestHash, 'chat_identity_conflict', 'A concurrent file operation changed its content.');
      await this.journal.flush(); return this.view(concurrent);
    }
    this.prune();
    const rows = this.records(), pending = rows.filter(row => ['queued', 'sending', 'outcome_unknown'].includes(this.view(row).state));
    requireThat(pending.length < 32 && rows.reduce((total, row) => total + (row.bytesBase64 ? row.delivery.file.byteLength : 0), 0) + request.file.byteLength <= 64 * 1024 * 1024,
      'limit_exceeded', 'The file queue is full (32 unresolved deliveries or 64 MiB). Reconcile original deliveries first.');
    const { bytesBase64, ...file } = request.file, at = new Date().toISOString();
    const saved: SavedFile = { caller, expectedBridge: target.expectedBridge, requestHash, transportHash: this.transportHash, bytesBase64, caption: request.caption,
      delivery: { operationId: request.operationId, binding: target.expectedBinding, channel: target.channel, file, kind, messageId: id,
        state: 'queued', errorCode: null, createdAt: at, updatedAt: at } };
    this.journal.transaction(() => {
      this.journal.enqueueAttachment(id, phone(this.config.senders[0]!.phoneNumber)! + '@s.whatsapp.net', request.caption ?? '');
      this.journal.set(key, saved); this.journal.set('out-file:' + id, key);
    });
    await this.journal.flush();
    return this.view(saved);
  }
  async admitted(id: string): Promise<boolean> {
    const saved = this.saved(id);
    requireThat(saved, 'chat_file_integrity', 'The queued file receipt is missing.');
    if (!this.bridge()) return false;
    const target = await this.target(saved.caller);
    requireThat(saved.transportHash === this.transportHash && same(saved.expectedBridge, target.expectedBridge) && same(saved.delivery.binding, target.expectedBinding) &&
      same(saved.delivery.channel, target.channel), 'chat_file_target_changed', 'The original file recipient or Main binding changed.');
    return true;
  }
  async payload(id: string) {
    const saved = this.saved(id);
    requireThat(saved?.bytesBase64, 'chat_file_integrity', 'The queued file bytes are unavailable.');
    const { bytes, kind } = await outgoingFileBytes({ ...saved.delivery.file, bytesBase64: saved.bytesBase64 });
    requireThat(kind === saved.delivery.kind, 'chat_file_integrity', 'The queued file kind changed.');
    const caption = saved.caption === null ? {} : { caption: saved.caption };
    return kind === 'image' ? { image: bytes, mimetype: saved.delivery.file.mediaType, ...caption }
      : { document: bytes, mimetype: saved.delivery.file.mediaType, fileName: saved.delivery.file.filename, ...caption };
  }
  mark(id: string, state: 'sending' | 'sent' | 'unknown' | 'failed', errorCode: string | null = null): void {
    const saved = this.saved(id); if (!saved) return;
    if (this.view(saved).state === 'sent' && state !== 'sent') return;
    this.journal.transaction(() => {
      this.journal.mark(id, state);
      this.journal.set('file-state:' + id, { updatedAt: new Date().toISOString(), errorCode });
      if (state === 'sent' || state === 'failed') {
        saved.bytesBase64 = null; this.journal.set(keyFor(saved.caller, saved.delivery.operationId), saved);
      }
    });
  }
  private records(): SavedFile[] {
    return this.journal.db.prepare("SELECT value FROM kv WHERE key LIKE 'file:%'").all().map(row => JSON.parse(String(row['value'])) as SavedFile);
  }
  prune(now = Date.now()): void {
    const rows = this.records().sort((a, b) => Date.parse(b.delivery.createdAt) - Date.parse(a.delivery.createdAt));
    let completed = 0;
    for (const row of rows) {
      const id = row.delivery.messageId, expired = now - Date.parse(row.delivery.createdAt) >= retentionMs;
      const state = this.view(row).state;
      if ((state === 'sent' || state === 'failed') && row.bytesBase64) this.mark(id, state, this.view(row).errorCode);
      if (expired || (['sent', 'failed'].includes(state) && ++completed > 256)) {
        // Compact replay fences prevent an old operation sending again after its bytes/receipt expire.
        this.journal.transaction(() => {
          this.journal.retireAttachment(id, row.requestHash);
          for (const key of [keyFor(row.caller, row.delivery.operationId), 'out-file:' + id, 'out-file-message:' + id, 'file-submitted:' + id, 'file-state:' + id]) this.journal.set(key, null);
        });
      }
    }
  }
}
