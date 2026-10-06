import makeWASocket, { DisconnectReason, fetchLatestWaWebVersion, jidNormalizedUser } from 'baileys';
import type { WAMessage, WASocket } from 'baileys';
import { join } from 'node:path';
import { atomicJson } from '../../../../packages/host-runtime/src/config.js';
import { encode, decode, whatsappAuth } from './auth.js';
import type { WhatsAppAuthStore } from './auth.js';
import { identity, WhatsAppJournal } from './journal.js';
import { jidPhone, phone } from './config.js';
import type { WhatsAppConfig, WhatsAppSender } from './config.js';

// Never forward protocol logs: they may include session keys, QR codes or message bodies.
const quiet = { level:'silent', child:()=>quiet, trace:()=>{}, debug:()=>{}, info:()=>{}, warn:()=>{}, error:()=>{}, fatal:()=>{} };
export class WhatsAppSocket {
  socket: WASocket|null=null;
  connected=false;
  lastOnlinePresenceAt:string|null=null;
  online:boolean|null=null;
  privacy:{online:string|null;lastSeen:string|null}|null=null;
  state='stopped';
  errorCode:string|null=null;
  lastSendMs:number|null=null;
  private stopped=false;
  private generation=0;
  private retry:NodeJS.Timeout|null=null;
  private heartbeat:NodeJS.Timeout|null=null;
  private backoff=1000;
  private busy=new Set<string>();
  private busyAt=0;
  private composing=new Set<string>();
  private composingAt=0;
  private presenceActive=false;
  private reading=false;
  private flushing=false;
  private events:Promise<void>=Promise.resolve();
  constructor(readonly config:WhatsAppConfig,readonly journal:WhatsAppJournal,readonly authStore:WhatsAppAuthStore,readonly dataRoot:string,readonly wake:()=>void){}
  start():void { this.stopped=false; void this.connect().catch(()=>this.reconnect()); }
  private reconnect():void {
    this.connected=false; this.busy.clear();this.composing.clear();this.composingAt=0; if(this.heartbeat)clearInterval(this.heartbeat);this.heartbeat=null;
    if(this.stopped || this.state==='logged_out' || this.state==='account_mismatch')return;
    if(this.retry)clearTimeout(this.retry); this.state='reconnecting';
    this.retry=setTimeout(()=>{this.retry=null;void this.connect().catch(()=>this.reconnect());},this.backoff);
    this.backoff=Math.min(60000,this.backoff*2);
  }
  async connect():Promise<void>{
    if(this.stopped)return; const generation=++this.generation;this.socket?.end(undefined);this.socket=null;
    this.state='connecting';const auth=whatsappAuth(this.authStore);
    const version=await fetchLatestWaWebVersion({signal:AbortSignal.timeout(15000)});
    if(this.stopped || generation!==this.generation)return;
    const socket=makeWASocket({auth:auth.state,version:version.version,logger:quiet,browser:['Ivy ChatBridge','Desktop','0.1.0'],
      markOnlineOnConnect:true,syncFullHistory:false,shouldSyncHistoryMessage:()=>false,connectTimeoutMs:30000,defaultQueryTimeoutMs:30000,keepAliveIntervalMs:20000,
      getMessage:async key=>{
        if(!key.id||!key.remoteJid)return undefined;
        const row=this.journal.db.prepare('SELECT jid,text FROM outgoing WHERE id=?').get(key.id);
        return row&&jidNormalizedUser(String(row['jid']))===jidNormalizedUser(key.remoteJid)?{conversation:String(row['text'])}:undefined;
      }});
    this.socket=socket;
    socket.ev.on('creds.update',()=>{if(generation===this.generation)auth.save();});
    socket.ev.on('connection.update',update=>{
      if(generation!==this.generation || this.stopped)return;
      if(typeof update.isOnline==='boolean')this.online=update.isOnline;
      if(update.qr){this.state='awaiting_qr';void atomicJson(join(this.dataRoot,'whatsapp-pairing.json'),{qr:update.qr,expiresAt:new Date(Date.now()+60000).toISOString()});}
      if(update.connection==='open'){
        if(jidPhone(socket.user?.id??'')!==phone(this.config.botPhoneNumber)){this.state='account_mismatch';this.errorCode='whatsapp_account_mismatch';socket.end(undefined);return;}
        this.connected=true;this.state='connected';this.errorCode=null;this.backoff=1000;this.lastOnlinePresenceAt=null;
        void socket.fetchPrivacySettings(true).then(settings=>{if(this.socket===socket)this.privacy={online:settings['online']??null,lastSeen:settings['last']??null};}).catch(()=>{});
        void atomicJson(join(this.dataRoot,'whatsapp-pairing.json'),{qr:null,expiresAt:null});
        if(this.heartbeat)clearInterval(this.heartbeat);void this.presence();this.heartbeat=setInterval(()=>{void this.presence();},5000);this.wake();
      }
      if(update.connection==='close'){
        this.online=false;
        const code=(update.lastDisconnect?.error as {output?:{statusCode?:number}}|undefined)?.output?.statusCode;
        this.errorCode=code===DisconnectReason.loggedOut?'whatsapp_logged_out':'whatsapp_disconnected';
        if(code===DisconnectReason.loggedOut)this.state='logged_out';this.reconnect();
      }
    });
    socket.ev.on('messages.upsert',event=>{
      if(generation!==this.generation || this.stopped || !['notify','append'].includes(event.type))return;
      for(const message of event.messages){this.events=this.events.then(()=>this.inbound(message,socket,generation)).catch(()=>{this.errorCode='whatsapp_inbound_failed';});}
    });
    socket.ev.on('messages.update',updates=>{
      if(generation!==this.generation || this.stopped)return;
      for(const {key,update} of updates)if(key.fromMe && key.id && key.remoteJid && typeof update.status==='number' && update.status>=2){
        const row=this.journal.db.prepare('SELECT jid FROM outgoing WHERE id=?').get(key.id);
        if(row && jidNormalizedUser(String(row['jid']))===jidNormalizedUser(key.remoteJid)){this.journal.mark(key.id,'sent');this.wake();}
      }
    });
  }
  async admitted(message:WAMessage,socket:WASocket):Promise<WhatsAppSender|null>{
    const key=message.key,jid=key.remoteJid??'';
    if(key.fromMe || !key.id || !/@(?:s\.whatsapp\.net|lid)$/.test(jid) || key.participant)return null;
    let number=jidPhone(jid);
    if(!number && jid.endsWith('@lid')){
      const mapped=await socket.signalRepository.lidMapping.getPNForLID(jid);
      const alternate=jidPhone(key.remoteJidAlt??''); number=jidPhone(mapped??'')??alternate;
      if(mapped && alternate && jidPhone(mapped)!==alternate)return null;
    }
    return number?this.config.senders.find(sender=>phone(sender.phoneNumber)===number)??null:null;
  }
  private async inbound(message:WAMessage,socket:WASocket,generation:number):Promise<void>{
    const sender=await this.admitted(message,socket);if(!sender || generation!==this.generation || this.stopped)return;
    const at=Number(message.messageTimestamp??0)*1000;
    if(!Number.isFinite(at) || at<Math.max(Date.parse(this.config.since),Date.now()-7*86400_000) || at>Date.now()+300000 || !message.message)return;
    const raw=encode(message);if(Buffer.byteLength(raw)>1024*1024){this.errorCode='whatsapp_message_too_large';return;}
    const id=identity(this.config.accountId,sender.channelId,message.key.id),prior=this.journal.db.prepare('SELECT sender,message FROM inbox WHERE id=?').get(id);
    if(prior){
      // PN/LID envelopes and receipt fields can change while the transport message stays identical.
      const fingerprint=this.journal.get<string>('inbox-content:'+id)??identity(encode(decode<WAMessage>(String(prior['message'])).message));
      if(prior['sender']!==sender.principalId || fingerprint!==identity(encode(message.message)))this.errorCode='whatsui_identity_conflict';
      return;
    }
    this.journal.transaction(()=>{
      this.journal.accept({id,sender:sender.principalId,jid:message.key.remoteJid!,message:raw,received:at});
      this.journal.set('inbox-content:'+id,identity(encode(message.message)));
      this.journal.set('read-receipt:'+id,{key:message.key,done:false});
    });
    this.wake();
  }
  async flushReadReceipts():Promise<void>{
    const socket=this.socket;if(!this.connected || !socket || this.reading)return;this.reading=true;
    try{
      await this.journal.flush();
      const rows=this.journal.db.prepare("SELECT key,value FROM kv WHERE key LIKE 'read-receipt:%' AND json_extract(value,'$.done')=0 LIMIT 32").all();
      for(const row of rows){const value=decode<{key:WAMessage['key'];done:boolean}>(String(row['value']));
        await socket.readMessages([value.key]);this.journal.set(String(row['key']),null);
      }
      if(this.errorCode==='whatsapp_read_receipt_failed')this.errorCode=null;
    }catch{this.errorCode='whatsapp_read_receipt_failed';}finally{this.reading=false;}
  }
  setBusy(jids:string[]):void {this.busy=new Set(jids);this.busyAt=Date.now();}
  async presence():Promise<void>{
    const socket=this.socket;if(!this.connected || !socket || this.presenceActive)return;this.presenceActive=true;
    try{
      const active=Date.now()-this.busyAt<20000?this.busy:new Set<string>();
      for(const jid of this.composing)if(!active.has(jid)){await socket.sendPresenceUpdate('paused',jid);this.composing.delete(jid);}
      const refresh=Date.now()-this.composingAt>=4000;
      for(const jid of active)if(refresh||!this.composing.has(jid)){await socket.sendPresenceUpdate('composing',jid);this.composing.add(jid);}
      if(active.size&&refresh)this.composingAt=Date.now();
      if(!this.lastOnlinePresenceAt || Date.now()-Date.parse(this.lastOnlinePresenceAt)>=(this.config.heartbeatMs??120000)){
        await socket.sendPresenceUpdate('available');if(this.socket===socket && this.connected)this.lastOnlinePresenceAt=new Date().toISOString();
      }
    }catch{this.errorCode='whatsapp_presence_failed';}finally{this.presenceActive=false;}
  }
  async flush():Promise<void>{
    const socket=this.socket;if(!this.connected || !socket || this.flushing)return;this.flushing=true;
    const blocked=new Set<string>();
    try{
      // Drain chunks in one pass. Retry with the SAME WhatsApp message ID; unresolved failures
      // remain visible but cannot permanently hold all later messages for this recipient.
      const rows=this.journal.db.prepare("SELECT * FROM outgoing WHERE state IN ('pending','sending') ORDER BY sequence LIMIT 64").all() as unknown as ReturnType<WhatsAppJournal['outgoing']>;
      for(const item of rows){
        if(blocked.has(item.jid)||this.socket!==socket||!this.connected)continue;
        const key='send-attempt:'+item.id,attempt=this.journal.get<{count:number;at:number}>(key)??{count:0,at:0};
        if(attempt.count>=3){this.journal.mark(item.id,'unknown');this.errorCode='whatsapp_delivery_unknown';continue;}
        if(attempt.at&&Date.now()-attempt.at<2000*2**attempt.count){blocked.add(item.jid);continue;}
        this.journal.transaction(()=>{this.journal.mark(item.id,'sending');this.journal.set(key,{count:attempt.count+1,at:Date.now()});});
        await this.journal.flush();
        try{const started=performance.now();await socket.sendMessage(item.jid,{text:item.text},{messageId:item.id});this.lastSendMs=Math.round(performance.now()-started);this.journal.mark(item.id,'sent');}
        catch{blocked.add(item.jid);this.errorCode='whatsapp_delivery_unknown';}
      }
      if(this.errorCode==='whatsapp_delivery_unknown'&&!this.journal.db.prepare("SELECT 1 FROM outgoing WHERE state IN ('sending','unknown') LIMIT 1").get())this.errorCode=null;
    }finally{this.flushing=false;}
  }
  async close():Promise<void>{
    this.stopped=true;this.generation++;this.connected=false;this.state='stopped';
    if(this.retry)clearTimeout(this.retry);if(this.heartbeat)clearInterval(this.heartbeat);
    this.socket?.end(undefined);this.socket=null;await this.events;
  }
}
