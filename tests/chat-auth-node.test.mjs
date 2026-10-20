import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createAuthAdapter,encode,decode,AuthFenced} from '../services/chat-bridge/rust/node/auth-adapter.mjs';
import {WhatsAppAuthStore} from '../services/chat-bridge/src/whatsapp/auth.ts';
import {Curve,generateSignalPubKey} from 'baileys';

const logger={};
for(const key of ['log','trace','info','warn','error','debug']) { logger[key]=console[key];console[key]=()=>{}; }
test.after(()=>{for(const [key,value]of Object.entries(logger))console[key]=value;});

function store(root,name) {
  const db=new WhatsAppAuthStore(join(root,name));
  let writes=0,gate=null,failure=false;
  const raw={
    async get(entries){return entries.map(([type,id])=>db.get(type,id));},
    async set(entries){
      writes++;
      db.transaction(()=>{for(const [type,id,value]of entries)db.set(type,id,value);});
      if(failure)throw Error('Synthetic lost commit acknowledgement.');
      if(gate)await gate;
    },
  };
  return{db,raw,get writes(){return writes;},hold(promise){gate=promise;},loseAck(){failure=true;}};
}

test('original auth adapter awaits commits, preserves the codec, and exercises its six fixed repository methods',async()=>{
  const root=mkdtempSync(join(tmpdir(),'ivy-auth-library-')),a=store(root,'a.sqlite'),b=store(root,'b.sqlite');
  let alice,bob;
  try {
    let release; a.hold(new Promise(resolve=>{release=resolve;}));let ready=false;
    const starting=createAuthAdapter(null,a.raw).then(value=>{ready=true;return value;});
    await new Promise(resolve=>setTimeout(resolve,20));
    assert(a.db.get('creds','default'));assert.equal(ready,false,'READY must await the original complete credential commit');release();alice=await starting;a.hold(null);
    bob=await createAuthAdapter(null,b.raw);
    const ac=decode(await alice.call('credentials','null')),bc=decode(await bob.call('credentials','null'));
    const aliceJid='15555550101:1@s.whatsapp.net',bobJid='15555550102:1@s.whatsapp.net',pre=Curve.generateKeyPair();
    await bob.call('setKeys',encode({'pre-key':{'1':pre}}));
    await alice.call('injectE2ESession',encode({jid:bobJid,session:{registrationId:bc.registrationId,identityKey:generateSignalPubKey(bc.signedIdentityKey.public),signedPreKey:{keyId:bc.signedPreKey.keyId,publicKey:generateSignalPubKey(bc.signedPreKey.keyPair.public),signature:bc.signedPreKey.signature},preKey:{keyId:1,publicKey:generateSignalPubKey(pre.public)}}}));
    const wire=decode(await alice.call('encryptMessage',encode({jid:bobJid,data:Buffer.from('auth-adapter-established')})));
    assert((decode(await bob.call('decryptMessage',encode({jid:aliceJid,...wire})))).equals(Buffer.from('auth-adapter-established')));
    const group='synthetic-auth-boundary@g.us';
    const first=decode(await alice.call('encryptGroupMessage',encode({group,meId:aliceJid,data:Buffer.from('group-first')})));
    await bob.call('processSenderKeyDistributionMessage',encode({item:{groupId:group,axolotlSenderKeyDistributionMessage:first.senderKeyDistributionMessage},authorJid:aliceJid}));
    assert((decode(await bob.call('decryptGroupMessage',encode({group,authorJid:aliceJid,msg:first.ciphertext})))).equals(Buffer.from('group-first')));
    const opaque='{ "unknown":"\\ud800", "order":7 }';b.db.set('future-category','untouched',opaque);
    await bob.call('saveCredentials',encode({...bc,removedByNextSave:'old',nullable:null,additionalData:{unknown:'\udfff',bytes:Buffer.from([0,255])}}));
    const next=decode(await bob.call('credentials','null'));delete next.removedByNextSave;
    Object.defineProperty(next,'__proto__',{value:{unknown:'retained'},enumerable:true,writable:true,configurable:true});
    await bob.call('saveCredentials',encode(next));
    const saved=decode(b.db.get('creds','default'));assert.equal(saved.additionalData.unknown,'\udfff');assert(saved.additionalData.bytes.equals(Buffer.from([0,255])));
    assert.equal(Object.hasOwn(saved,'removedByNextSave'),false);assert.equal(saved.nullable,null);
    assert.equal(Object.hasOwn(saved,'__proto__'),true);assert.equal(saved.__proto__.unknown,'retained');
    assert.equal(b.db.get('future-category','untouched'),opaque);
    assert.equal(ac.registered,false);
    await assert.rejects(alice.call('connect','{}'),TypeError);
  } finally {alice?.close();bob?.close();a.db.close();b.db.close();rmSync(root,{recursive:true});}
});

test('an uncertain original transaction commit fences all subsequent callbacks, including upstream retries', {timeout:40000},async()=>{
  const root=mkdtempSync(join(tmpdir(),'ivy-auth-uncertain-')),owned=store(root,'auth.sqlite');let adapter;
  try {
    adapter=await createAuthAdapter(null,owned.raw);
    const initialized=owned.writes;owned.loseAck();
    // This repository method uses the actual default addTransactionCapability
    // commitWithRetry loop. One committed callback is followed by lost ACK.
    // Retrying inside the library cannot issue another raw state.set.
    await assert.rejects(adapter.call('encryptGroupMessage',encode({group:'synthetic-uncertain@g.us',meId:'15555550101:1@s.whatsapp.net',data:Buffer.from('not-replayed')})),AuthFenced);
    assert.equal(owned.writes,initialized+1);
    assert(owned.db.get('sender-key','synthetic-uncertain@g.us::15555550101::1'));
    await assert.rejects(adapter.call('credentials','null'),AuthFenced);
    await assert.rejects(adapter.call('encryptGroupMessage',encode({})),AuthFenced);
    assert.equal(owned.writes,initialized+1);
  } finally {adapter?.close();owned.db.close();rmSync(root,{recursive:true});}
});
