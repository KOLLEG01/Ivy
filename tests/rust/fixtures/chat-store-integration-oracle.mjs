import './chat-store-loader.mjs';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const now=1790000000123,RealDate=Date;
let clock=now;
globalThis.Date=class extends RealDate {constructor(...args){super(...(args.length?args:[clock]));}static now(){return clock;}};
const client={async request(method){if(method==='system.status')return{runtimeEpoch:'chat'};if(method==='objects.write')return{object:{id:'input'},revision:{revision:2}};throw Error('Unexpected '+method);}};
const dates=[];
for(const time of [-8640000000000001,-8640000000000000,-62167219200000,-1,0,now,253402300799999,253402300800000,8640000000000000,8640000000000001]){
 clock=time;const store=new ChatStore(client,'root');let expected;
 try{const pin=await store.write('chat-bridge/main',{sequence:1},'main',{create:{parentId:'root',name:'main'}});const d=await store.read('chat-bridge/main',pin);expected={createdAt:d.metadata.createdAt,updatedAt:d.metadata.updatedAt,revisionAt:d.revision.createdAt,changeVersion:store.changeVersion};}
 catch(error){const e=IvyError.from(error);expected={error:{code:e.code,message:e.message,outcome:e.outcome}};}
 dates.push({time,expected});store.close();
}
const workflows=[];
for(const state of ['completed','failed','cancelled']){
 clock=now;const store=new ChatStore(client,'root');
 const main=await store.write('chat-bridge/main',{queue:[{inputId:'input'}]},'main',{create:{parentId:'root',name:'main'}});
 const child=await store.write('chat-bridge/collection',{},'child',{create:{parentId:'input',name:'child'}});
 const blob=store.saveLocalBlob('chat-bridge/evidence-chunk','blob',child.objectId,Buffer.from([0,1,255]));
 const trace=[{stage:'created',changeVersion:store.changeVersion,blob:[...store.localBlob('chat-bridge/evidence-chunk',blob,child.objectId)]}];
 store.limits.count=2;
 await store.write('chat-bridge/input',{payload:{images:[]},state,finishedAt:new Date(now).toISOString()},'finished',{objectId:'input',expectedRevision:1});
 trace.push({stage:'queued',retained:!!await store.named('chat-bridge/collection','child','input'),changeVersion:store.changeVersion});
 await store.write('chat-bridge/main',{queue:[]},'drained',{objectId:main.objectId,expectedRevision:1});
 await store.requireAvailable(main);
 let blobError;try{store.localBlob('chat-bridge/evidence-chunk',blob,child.objectId);}catch(error){blobError=IvyError.from(error).code;}
 trace.push({stage:'drained',retained:!!await store.named('chat-bridge/collection','child','input'),changeVersion:store.changeVersion,mainRevision:(await store.read('chat-bridge/main',main)).pin.revision,blobError});
 workflows.push({state,expected:trace});store.close();
}
process.stdout.write(JSON.stringify({now,dates,workflows}));
