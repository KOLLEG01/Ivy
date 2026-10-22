import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load} from './inbox-fixtures.mjs';
import {snapshot,seedTechnicalHistory} from './import-core-fixtures.mjs';
const {MediaSpool}=await load('services/secretary/src/media-spool.js');
const {hashJson,canonical,validateAgent,IvyError}=await load('packages/sdk/src/node.js');
const {validateMedia}=await load('services/secretary/src/media-schema.js');
const {validateTriage}=await load('services/secretary/src/triage-schema.js');

export async function seed(core,location,settings,at){
 const schema={...JSON.parse(readFileSync('specs/schemas/secretary-media.schema.json','utf8')),$id:'https://fixture.invalid/retained-media'};
 const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
 const samples=schemaSamples(ajv,[schema]);
 const sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
 const owner={principalId:settings.identity.principalId,rootObjectId:settings.identity.scope.rootObjectId};
 const admission={sourceId:'mail',producerPrincipalId:owner.principalId,accountId:'account',providerMessageId:'retained-message',itemObjectId:'00000000-0000-4000-8000-000000000040',identityHash:hashJson('retained-identity'),messageHash:hashJson('retained-message')};
 const materialization={attachment:{messageId:admission.providerMessageId,attachmentId:'retained-attachment',attachmentType:'fileAttachment',name:'../original\ud800.txt',mediaType:'text/plain',providerSizeBytes:20,inline:false,contentId:null,modifiedAt:null,payloadFetchSupported:true},fileId:'retained-file',downloadUrl:'https://fixture.oaiusercontent.com/retained?sig=synthetic-capability',nativeResponseHash:hashJson('original-native-response')};
 const intent={operationId:'retained-acquisition',captureOperationId:'fixture:1800000000000:retained-capture',item:{objectId:admission.itemObjectId,revision:2},admission,materializationHash:hashJson(materialization)};
 const plan={...sample('MediaReadPlan'),admission,item:intent.item,captureOperationId:intent.captureOperationId,createdAt:at,epoch:'original-media-epoch'};
 const call={planHash:hashJson(plan),slot:0,method:'account/read',params:{refreshToken:false},operationId:'fixture:1800000000000:original-media-native',definitionHash:hashJson('original-definition'),preparedAt:at};
 const operation={schemaVersion:1,operationId:call.operationId,callerPrincipalId:owner.principalId,serviceNodeId:plan.configuration.target.serviceNodeId,nativeVersion:plan.configuration.target.nativeVersion,nativeExecutableHash:plan.configuration.target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),phase:'accepted',createdAt:at,updatedAt:at,epoch:null,requestId:null,reply:null,code:null};
 validateMedia('MediaReadPlan',plan);validateMedia('MediaReadCall',call);validateAgent('Operation',operation);
 const publishedPlan=structuredClone(plan);publishedPlan.item={objectId:'00000000-0000-4000-8000-000000000041',revision:3};publishedPlan.admission.itemObjectId=publishedPlan.item.objectId;publishedPlan.captureOperationId='fixture:1800000000000:published-capture';
 const publishedIntent={...intent,operationId:'published-acquisition',captureOperationId:publishedPlan.captureOperationId,item:publishedPlan.item,admission:publishedPlan.admission};
 const publishedCall={...call,planHash:hashJson(publishedPlan),slot:38,method:'thread/unsubscribe',params:{threadId:'retained-media-thread'},operationId:'fixture:1800000000000:original-media-unsubscribe'};
 const terminal={...operation,operationId:publishedCall.operationId,method:publishedCall.method,params:publishedCall.params,requestHash:hashJson({method:publishedCall.method,params:publishedCall.params}),phase:'succeeded',epoch:plan.epoch,requestId:7,reply:{result:{status:'unsubscribed'}}};
 const folder=join(location,'outlook-media'),spool=await MediaSpool.open(folder,owner),payload=Buffer.from('Original\0retained\ufffd text\n','utf8');let gets=0;
 try{
  spool.prepare(intent,materialization);
  await spool.acquire(intent.operationId,new AbortController().signal,async()=>{},async()=>{gets++;return new Response(payload);});
  for(const id of ['prepared-acquisition','interrupted-acquisition','failed-acquisition'])spool.prepare({...intent,operationId:id},materialization);
  await spool.acquire('failed-acquisition',new AbortController().signal,async()=>{},async()=>{throw new IvyError('media_download_cancelled','Synthetic retained cancellation.');});
  spool.prepare(publishedIntent,materialization);await spool.acquire(publishedIntent.operationId,new AbortController().signal,async()=>{},async()=>{gets++;return new Response(payload);});
  for(const p of [plan,publishedPlan])spool.prepareRead(p);
  spool.prepareNativeCall(call);spool.observeNativeCall(call,operation);
  spool.prepareNativeCall(publishedCall);spool.observeNativeCall(publishedCall,terminal);
  assert.equal(gets,2);
 }finally{spool.close();}
 const db=new DatabaseSync(join(folder,'media.sqlite'));
 db.prepare("UPDATE attempts SET phase='downloading' WHERE operation_id='interrupted-acquisition'").run();
 db.exec("CREATE TABLE retained_spool_unknown(note TEXT,bytes BLOB);INSERT INTO retained_spool_unknown VALUES('retain exactly',x'00ff00d800');");
 const before=snapshot(db);db.close();
 const work={schemaVersion:1,item:intent.item,plan:{objectId:'retained-triage-plan',revision:1},operationId:'fixture:1800000000000:triage-original',phase:'pending',completion:null,assessment:null,errorCode:null};
 validateTriage('TriageWork',work);seedTechnicalHistory(core,'secretary/triage-work','retained-triage-work',work,'retained-triage-work');
 const thread={schemaVersion:1,plan:work.plan,threadId:'retained-dormant-thread'};validateTriage('TriageThread',thread);seedTechnicalHistory(core,'secretary/triage-thread','retained-triage-thread',thread,'retained-triage-thread');
 const nativeBytes=Buffer.from(canonical({method:'turn/start',params:{input:[{type:'text',text:'retained\ud800draft'}]},operationId:work.operationId}));
 core.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run('retained-triage-params','secretary/triage-native-parameters','retained-triage-params',nativeBytes);
 core.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run('retained-triage-attachment','secretary/triage-read','retained-triage-attachment',Buffer.from([0,255,0,216,0]));
 return{owner,intent,materialization,plan,call,operation,publishedPlan,publishedCall,terminal,publishedIntent,before,payloadBase64:payload.toString('base64'),work,thread,nativeBase64:nativeBytes.toString('base64'),gets};
}
if(process.argv[2]==='verify'){
 const [folder,file]=process.argv.slice(3),saved=JSON.parse(readFileSync(file,'utf8')),spool=await MediaSpool.open(folder,saved.owner);let gets=0,guards=0;
 try{
  assert.deepEqual(spool.payload(saved.intent.operationId).bytes,Buffer.from(saved.payloadBase64,'base64'));
  for(const id of [saved.intent.operationId,'interrupted-acquisition','failed-acquisition'])await spool.acquire(id,new AbortController().signal,async()=>{guards++;},async()=>{gets++;throw Error('A retained terminal attempt cannot dispatch another GET');});
  assert.equal(guards,0);assert.equal(gets,0);assert.equal(spool.get('prepared-acquisition').phase,'prepared');assert.equal(spool.get('interrupted-acquisition').errorCode,'media_download_interrupted');
  assert.deepEqual(spool.readPlan(saved.plan.item),saved.plan);assert.equal(spool.readPlan(saved.publishedPlan.item),null);assert.equal(spool.get(saved.publishedIntent.operationId),null);
  const native=spool.nativeCall(hashJson(saved.plan),0);assert.deepEqual(native.call,saved.call);assert.deepEqual(native.seen,saved.operation);assert.equal(native.observation.phase,'succeeded');assert.equal(native.observation.operationId,saved.call.operationId);
 }finally{spool.close();}
 const db=new DatabaseSync(join(folder,'media.sqlite'));assert.equal(db.prepare('PRAGMA user_version').get().user_version,1);assert.equal(db.prepare('SELECT hex(bytes) AS bytes FROM retained_spool_unknown').get().bytes,'00FF00D800');db.close();
}
