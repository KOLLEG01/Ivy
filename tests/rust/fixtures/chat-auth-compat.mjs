// All keys/accounts are generated locally for this fixture. No socket is opened.
import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,copyFileSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {WhatsAppAuthStore,whatsappAuth,encode,decode} from '../../../services/chat-bridge/src/whatsapp/auth.ts';
import {Curve,generateSignalPubKey,addTransactionCapability,proto} from 'baileys';
import {makeLibSignalRepository} from 'baileys/lib/Signal/libsignal.js';

const [mode,root]=process.argv.slice(2),aliceJid='15555550101:1@s.whatsapp.net',bobJid='15555550102:1@s.whatsapp.net',group='synthetic-auth@g.us';
const diagnostics={info:0,warn:0,debug:0,error:0};
// libsignal's legacy console diagnostics can include key material. The fixture
// observes counts only; neither successful nor failed reports print arguments.
for(const level of Object.keys(diagnostics))console[level]=()=>{diagnostics[level]++;};
const logger={trace(){},debug(){},info(){},warn(){},error(){},child(){return this;}};
const opened=[];
function peer(file){
 const store=new WhatsAppAuthStore(join(root,file)),auth=whatsappAuth(store);
 auth.state.keys=addTransactionCapability(auth.state.keys,logger,{maxCommitRetries:1,delayBetweenTriesMs:0});
 const value={...auth,store,repo:makeLibSignalRepository(auth.state,logger)};opened.push(value);return value;
}
const bytes=n=>Buffer.alloc(n,17),hash=b=>createHash('sha256').update(b).digest('hex');
const wirePath=join(root,'wire.json'),opaque='{ "future":"\\ud800", "bytes":{"type":"Buffer","data":"AAH/"}, "order":7 }';
async function inject(alice,bob,id){
 const pair=Curve.generateKeyPair();await bob.state.keys.set({'pre-key':{[id]:pair}});
 const c=bob.state.creds;
 await alice.repo.injectE2ESession({jid:bobJid,session:{registrationId:c.registrationId,identityKey:generateSignalPubKey(c.signedIdentityKey.public),signedPreKey:{keyId:c.signedPreKey.keyId,publicKey:generateSignalPubKey(c.signedPreKey.keyPair.public),signature:c.signedPreKey.signature},preKey:{keyId:id,publicKey:generateSignalPubKey(pair.public)}}});
 const first=await alice.repo.encryptMessage({jid:bobJid,data:Buffer.from('establish-'+id)});
 assert((await bob.repo.decryptMessage({jid:aliceJid,...first})).equals(Buffer.from('establish-'+id)),'Initial synthetic decryption');
 const reply=await bob.repo.encryptMessage({jid:aliceJid,data:Buffer.from('ack-'+id)});
 assert((await alice.repo.decryptMessage({jid:bobJid,...reply})).equals(Buffer.from('ack-'+id)),'Synthetic ratchet reply');
}
const pack=v=>({...v,ciphertext:v.ciphertext.toString('base64')}),unpack=v=>({...v,ciphertext:Buffer.from(v.ciphertext,'base64')});
async function pending(alice,bob,label){
 const values=[];for(let i=0;i<3;i++)values.push(await alice.repo.encryptMessage({jid:bobJid,data:Buffer.from(label+'-'+i)}));
 assert((await bob.repo.decryptMessage({jid:aliceJid,...values[2]})).equals(Buffer.from(label+'-2')),'Skipped-key fixture');
 return values.slice(0,2).map(pack);
}
async function groupReopen(bob,ciphertext){
 const key=group+'::15555550101::1',before=bob.store.get('sender-key',key);
 try{await bob.repo.decryptGroupMessage({group,authorJid:aliceJid,msg:ciphertext});}
 catch(error){
  if(error?.name!=='TypeError')throw error;
  assert(bob.store.get('sender-key',key)===before,'Failed legacy group read leaves encoded state untouched');
  return{execution:'completed',conditionMet:false,error:{name:error.name,code:error.code??null},encodedStateUnchanged:true};
 }
 throw Error('Reassess pinned legacy group skipped-key counterexample');
}
function report(peer){
 const rows=peer.store.db.prepare('SELECT category,id,value FROM auth ORDER BY category,id').all();
 const categoryCounts={};for(const row of rows)categoryCounts[row.category]=(categoryCounts[row.category]??0)+1;
 const address=peer.repo.jidToSignalProtocolAddress(aliceJid),raw=peer.store.get('session',address);
 const session=decode(raw),entries=Object.values(session._sessions);
 const current=entries.filter(v=>v.indexInfo.closed===-1).length,archived=entries.length-current;
 const skipped=entries.reduce((n,v)=>n+Object.values(v._chains).reduce((n,c)=>n+Object.keys(c.messageKeys).length,0),0);
 assert.equal(current,1,'One current synthetic session');assert(archived>=1,'Archived synthetic session retained');
 assert.equal(session.version,'v1');assert.equal(peer.store.get('future-category','opaque'),opaque,'Opaque row retained');
 assert.equal(peer.store.get('pre-key','retired'),null,'Deleted key remains absent');
 for(const category of ['creds','pre-key','session','sender-key','sender-key-memory','app-state-sync-key','app-state-sync-version','lid-mapping','device-list','identity-key','tctoken','future-category'])assert(categoryCounts[category]>0,'Missing synthetic category '+category);
 return{synthetic:true,mode,categoryCounts,rows:rows.length,current,archived,skipped,logicalHash:hash(Buffer.from(JSON.stringify(rows))),diagnostics};
}
async function run(){
 if(!['seed','group-control','archived-control','advance','verify'].includes(mode))throw Error('Unknown fixture phase');
 const archivedProbe=mode==='archived-control',sourceHashes=archivedProbe?['alice.sqlite','original.sqlite'].map(f=>hash(readFileSync(join(root,f)))):[];
 if(archivedProbe){copyFileSync(join(root,'alice.sqlite'),join(root,'archive-alice.sqlite'));copyFileSync(join(root,'original.sqlite'),join(root,'archive-bob.sqlite'));}
 const alice=peer(archivedProbe?'archive-alice.sqlite':'alice.sqlite'),bob=peer(archivedProbe?'archive-bob.sqlite':mode==='seed'||mode==='group-control'?'original.sqlite':mode==='advance'?'copied.sqlite':'rollback.sqlite');
 if(mode==='seed'){
  await inject(alice,bob,1);const archived=await pending(alice,bob,'archived');
  await inject(alice,bob,2);const current=await pending(alice,bob,'current');
  const first=await alice.repo.encryptGroupMessage({group,meId:aliceJid,data:Buffer.from('group-establish')});
  await bob.repo.processSenderKeyDistributionMessage({item:{groupId:group,axolotlSenderKeyDistributionMessage:first.senderKeyDistributionMessage},authorJid:aliceJid});
  assert((await bob.repo.decryptGroupMessage({group,authorJid:aliceJid,msg:first.ciphertext})).equals(Buffer.from('group-establish')));
  const groups=[];for(let i=0;i<3;i++)groups.push(await alice.repo.encryptGroupMessage({group,meId:aliceJid,data:Buffer.from('group-'+i)}));
  assert((await bob.repo.decryptGroupMessage({group,authorJid:aliceJid,msg:groups[2].ciphertext})).equals(Buffer.from('group-2')));
  await bob.state.keys.set({
   'pre-key':{'spare':Curve.generateKeyPair(),'retired':Curve.generateKeyPair()},
   'sender-key-memory':{[group]:{[aliceJid]:true}},
   'app-state-sync-key':{'synthetic-sync':proto.Message.AppStateSyncKeyData.fromObject({keyData:bytes(32),fingerprint:{rawId:7,currentIndex:3,deviceIndexes:[1,2]},timestamp:1730000000000})},
   'app-state-sync-version':{'regular':{version:7,hash:bytes(128),indexValueMap:{'synthetic-index':{valueMac:bytes(32)}}}},
   'lid-mapping':{'pn:15555550103':'100000000000003'},
   'device-list':{'15555550101':['1','2']},
   'identity-key':{'synthetic-extra':bytes(33)},
   'tctoken':{'15555550101':{token:bytes(32),timestamp:'1730000000',senderTimestamp:1730000001}}
  });
  await bob.state.keys.set({'pre-key':{'retired':null}});
  Object.assign(bob.state.creds,{registered:true,me:{id:bobJid,name:'Synthetic fixture'},platform:'fixture',myAppStateKeyId:'synthetic-sync',firstUnuploadedPreKeyId:7,nextPreKeyId:19,accountSyncCounter:4,lastAccountSyncTimestamp:1730000000,lastPropHash:'synthetic-property',routingInfo:bytes(8),additionalData:{unknown:'\udfff',retained:bytes(3)},account:proto.ADVSignedDeviceIdentity.fromObject({details:bytes(7),accountSignatureKey:bytes(32),accountSignature:bytes(64),deviceSignature:bytes(64)})});
  bob.store.set('future-category','opaque',opaque);bob.save();alice.save();
  writeFileSync(wirePath,encode({archived,current,group:groups.slice(0,2).map(v=>v.ciphertext)}));
  const state=report(bob);assert(state.skipped>=4,'Both current and archived skipped keys');return state;
 }
 const wire=decode(readFileSync(wirePath,'utf8')),index=mode==='advance'?0:1;
 if(mode==='group-control')return{...report(bob),groupReopen:await groupReopen(bob,wire.group[0])};
 if(archivedProbe){
  const currentRoot=()=>Object.values(decode(bob.store.get('session',bob.repo.jidToSignalProtocolAddress(aliceJid)))._sessions).find(v=>v.indexInfo.closed===-1).currentRatchet.rootKey;
  const before=currentRoot();assert((await bob.repo.decryptMessage({jid:aliceJid,...unpack(wire.archived[0])})).equals(Buffer.from('archived-0')),'Archived message is still decryptable');
  const currentRootChanged=before!==currentRoot();assert(currentRootChanged,'Pinned legacy current-root mutation counterexample');
  const reply=await bob.repo.encryptMessage({jid:aliceJid,data:Buffer.from('archived-probe-reply')});let failure=null;
  try{await alice.repo.decryptMessage({jid:bobJid,...reply});}catch(error){if(error?.name!=='SessionError')throw error;failure={name:error.name,code:error.code??null};}
  assert(failure,'Reassess pinned archived-session reply counterexample');
  assert(['alice.sqlite','original.sqlite'].every((f,i)=>hash(readFileSync(join(root,f)))===sourceHashes[i]),'Archive probe leaves source files untouched');
  return{...report(bob),archivedReply:{execution:'completed',conditionMet:false,archivedDecrypted:true,currentRootChanged,error:failure,sourceFilesUnchanged:true}};
 }
 assert((await bob.repo.decryptMessage({jid:aliceJid,...unpack(wire.current[index])})).equals(Buffer.from('current-'+index)),'Retained current decryption');
 const groupFinding=await groupReopen(bob,wire.group[index]);
 const reply=await bob.repo.encryptMessage({jid:aliceJid,data:Buffer.from(mode+'-reply')});
 assert((await alice.repo.decryptMessage({jid:bobJid,...reply})).equals(Buffer.from(mode+'-reply')),'Synthetic evolved state reply');
 const key=(await bob.state.keys.get('app-state-sync-key',['synthetic-sync']))['synthetic-sync'];
 assert(Buffer.from(key.keyData).equals(bytes(32)),'Protobuf auth key bytes');assert.equal(String(key.timestamp),'1730000000000');
 const state=(await bob.state.keys.get('app-state-sync-version',['regular'])).regular;assert.equal(state.version,mode==='advance'?7:8);state.version++;
 await bob.state.keys.set({'app-state-sync-version':{'regular':state}});
 assert.equal(bob.state.creds.additionalData.unknown,'\udfff','Unknown credential text');
 assert(Buffer.from(bob.state.creds.routingInfo).equals(bytes(8)),'Retained credential bytes');
 bob.state.creds.accountSyncCounter++;bob.state.creds.nextPreKeyId++;bob.save();alice.save();return{...report(bob),groupReopen:groupFinding};
}
try{const result=await run();for(const p of opened)p.store.close();process.stdout.write(JSON.stringify(result));}
catch(error){for(const p of opened)try{p.store.close();}catch{}const line=String(error?.stack).split('\n').find(v=>v.includes('chat-auth-compat.mjs:'))?.match(/chat-auth-compat\.mjs:(\d+):(\d+)/)?.slice(1).join(':');process.stderr.write(JSON.stringify({phase:mode,error:error?.name??'Error',fixtureLine:line??null,message:'Synthetic auth compatibility assertion failed.'}));process.exitCode=1;}
