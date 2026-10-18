import {WebSocketServer} from 'ws';
import {load,settings as base,owner,rootId,at} from './inbox-fixtures.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {canonical,hashJson}=await load('packages/sdk/src/node.js');
const settings={...base,pollMs:60000,identity:{...base.identity,serviceNodeId:'fixture.secretary'}};
const id='00000000-0000-4000-8000-000000000099';
const value=defaultConfiguration(settings,at,{serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'});
const store=new SecretaryStore({},rootId);
let document;
try{
 const write=store.objectWriteRequest('secretary/configuration',value,{object:{objectId:id,revision:1}},'fixture:1800000000000:configuration');
 document={object:{id,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/configuration'},revision:{revision:1,contractVersion:contractVersion('secretary/configuration'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:write.references,createdAt:at},content:{encoding:'json',value}};
}finally{store.close();}
// This socket supplies only the real SDK connection lifetime. Domain RPCs are
// independently gated by the Rust test; there is no live Hive or provider.
const server=new WebSocketServer({host:'127.0.0.1',port:0});
await new Promise((resolve,reject)=>{server.once('listening',resolve);server.once('error',reject);});
process.stdout.write(JSON.stringify({port:server.address().port,settings,owner,rootId,document})+'\n');
process.stdin.resume();
process.stdin.on('end',()=>{for(const socket of server.clients)socket.terminate();server.close();});
