import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { existsSync, readFileSync } from 'node:fs';
import { registerHooks, stripTypeScriptTypes } from 'node:module';
export const oracleRoot=resolve(process.env.IVY_SECRETARY_ORACLE??fileURLToPath(new URL('../../',import.meta.url)));
registerHooks({
 resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(source))return next(source.href,context);}
  return next(specifier,context);
 },
 load(url,context,next){
  if(url.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
 }
});
export const load=p=>{let path=resolve(oracleRoot,p);if(!existsSync(path)&&path.endsWith('.js')&&existsSync(path.slice(0,-3)+'.ts'))path=path.slice(0,-3)+'.ts';return import(pathToFileURL(path));};
const {identityHash,messageHash}=await load('services/secretary/src/engine.js');
const {hashJson}=await load('packages/sdk/src/node.js');
export const rootId='00000000-0000-4000-8000-000000000001',itemId='00000000-0000-4000-8000-000000000002',sourceId='00000000-0000-4000-8000-000000000003',operationId='00000000-0000-4000-8000-000000000004';
export const at='2026-01-01T00:00:00.000Z',id='fixture-epoch:1800000000000:inbox',caller='producer';
export const scope={secretaryId:'secretary',rootObjectId:rootId};
export const definition={sourceId:'mail',accountId:'account',kind:'email',producerPrincipalIds:[caller],allowedSenderIds:null,ownSenderIds:['self']};
export const source={schemaVersion:1,definition,definitionHash:hashJson(definition),cursor:null,checkpointOperation:null,updatedAt:null};
export const message={accountId:'account',conversationId:'conversation',messageId:'message',nativeRevision:'revision',senderId:'sender',outgoing:false,attachments:'none',occurredAt:at,observedAt:at,title:'Subject',text:'Full text',url:null};
export const policy={silentTime:null,urgentBypass:false,whatsappQuestionsOnly:false,minimumMainUrgency:'normal',researchMaxMinutes:0,voiceEscalation:null};
export const settings={identity:{scope,principalId:'secretary-principal',hostId:'fixture-host'},sources:[definition],policy,pollMs:100,recordsPerTick:10};
export const owner={connected:true,synced:true,principalId:'secretary-principal',hostId:'fixture-host',serviceName:'secretary'};
export const assessment={urgency:'normal',disposition:'notify',notification:'main',summary:'Summary',question:false,usefulIntel:true,task:null};
export const item={schemaVersion:1,sourceId:'mail',identityHash:identityHash('mail',message),messageHash:messageHash(message),message,media:[],decision:null,notice:null};
export const request={action:'capture',operationId:id,expectedScope:scope,sourceId:'mail',message};
export const accepted={schemaVersion:1,action:'capture',operationId:id,callerPrincipalId:caller,requestHash:hashJson(request),request,sourceId:'mail',source,createdAt:at,prepared:null,existingEffect:null,ignoredReason:null,captureIdentity:item.identityHash,checkpointCaptures:[],phase:'accepted',effect:null,errorCode:null};
