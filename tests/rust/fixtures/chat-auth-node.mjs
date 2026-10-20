// Original implementation as an independent persisted peer/rollback authority.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import './chat-auth-node-loader.mjs';
import {WhatsAppAuthStore,whatsappAuth,encode,decode} from '../../../services/chat-bridge/src/whatsapp/auth.ts';
import {addTransactionCapability} from 'baileys';
import {makeLibSignalRepository} from 'baileys/lib/Signal/libsignal.js';
const [mode,root]=process.argv.slice(2), aliceJid='15555550101:1@s.whatsapp.net',bobJid='15555550102:1@s.whatsapp.net';
for(const level of ['log','trace','info','warn','debug','error'])console[level]=()=>{};
const logger={trace(){},debug(){},info(){},warn(){},error(){},child(){return this;}},opened=[];
function peer(file) {
  const store=new WhatsAppAuthStore(join(root,file)),auth=whatsappAuth(store);
  auth.state.keys=addTransactionCapability(auth.state.keys,logger,{maxCommitRetries:1,delayBetweenTriesMs:0});
  const p={store,...auth,repo:makeLibSignalRepository(auth.state,logger)};opened.push(p);return p;
}
async function run() {
  if(mode==='lock') {
    const {ExecutorLock}=await import('../../../packages/host-runtime/src/journal.ts');
    try{const lock=new ExecutorLock(join(root,'whatsapp-owner'));lock.close();return{acquired:true};}
    catch(error){assert.equal(error.code,'executor_already_running');return{acquired:false};}
  }
  if(mode==='ordered-batch') {
    const store=new WhatsAppAuthStore(join(root,'auth.sqlite'));
    const entries=JSON.parse(readFileSync(join(root,'entries.json')));
    store.transaction(()=>{for(const [category,id,value] of entries)store.set(category,id,value);});
    const rows=store.db.prepare('SELECT * FROM auth ORDER BY category,id').all(),changes=store.db.prepare('SELECT COUNT(*) AS n FROM observed').get().n;
    store.close();return{rows,changes};
  }
  const alice=peer('alice.sqlite');
  if(mode==='peer-reply') {
    const reply=decode(readFileSync(join(root,'reply.json'),'utf8'));
    assert((await alice.repo.decryptMessage({jid:bobJid,...reply})).equals(Buffer.from('boundary-reply')));
    const followup=await alice.repo.encryptMessage({jid:bobJid,data:Buffer.from('boundary-followup')});
    alice.save();return{peerDecrypted:true,followup:encode(followup)};
  }
  if(mode==='archived-peer') {
    const reply=decode(readFileSync(join(root,'reply.json'),'utf8'));
    let failure;try{await alice.repo.decryptMessage({jid:bobJid,...reply});}catch(error){failure=error.name;}
    assert.equal(failure,'SessionError');return{execution:'completed',conditionMet:false,error:failure};
  }
  if(mode==='reopen') {
    const bob=peer('auth.sqlite'),wire=decode(readFileSync(join(root,'wire.json'),'utf8'));
    const before=bob.store.get('session','15555550101.1');
    let replay=false;try{await bob.repo.decryptMessage({jid:aliceJid,...{...wire.current[0],ciphertext:Buffer.from(wire.current[0].ciphertext,'base64')}});}catch{replay=true;}
    assert(replay);assert.equal(bob.store.get('session','15555550101.1'),before);
    assert((await bob.repo.decryptMessage({jid:aliceJid,...{...wire.current[1],ciphertext:Buffer.from(wire.current[1].ciphertext,'base64')}})).equals(Buffer.from('current-1')));
    const reply=await bob.repo.encryptMessage({jid:aliceJid,data:Buffer.from('reopened-current-db')});
    assert((await alice.repo.decryptMessage({jid:bobJid,...reply})).equals(Buffer.from('reopened-current-db')));
    assert.equal(bob.state.creds.additionalData.unknown,'\udfff');
    assert.equal(bob.state.creds.accountSyncCounter,5);
    const key=(await bob.state.keys.get('app-state-sync-key',['synthetic-sync']))['synthetic-sync'];
    assert(Buffer.from(key.keyData).equals(Buffer.alloc(32,17)));
    const version=(await bob.state.keys.get('app-state-sync-version',['regular'])).regular;
    assert.equal(version.version,8);
    return{sameEvolvedDatabase:true,replayRejected:true,remainingSkippedKeyDecrypted:true,peerDecrypted:true,unknownCreds:true,protobufKey:true,categories:bob.store.db.prepare('SELECT DISTINCT category FROM auth').all().length};
  }
  throw Error('Unknown fixture phase.');
}
try{const result=await run();for(const p of opened){p.repo.close();p.store.close();}process.stdout.write(JSON.stringify(result));}
catch{for(const p of opened)try{p.repo.close();p.store.close();}catch{}process.stderr.write('Original auth boundary assertion failed.');process.exitCode=1;}
