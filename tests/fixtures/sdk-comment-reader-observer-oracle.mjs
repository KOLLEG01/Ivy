import fs from 'node:fs';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
const source=fs.readFileSync(new URL('sdk-comment-reader.ts.txt',import.meta.url));
const sha=bytes=>createHash('sha256').update(bytes).digest('hex');
assert.equal(sha(source),'eda30de9d2741fcf4cc19088861a57e2fbc8cd4d5294e1afa3e27e2da6aa47be');
const {CommentReader}=await import('data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(source.toString(),{mode:'transform'})).toString('base64'));
const wrapper=fs.readFileSync(new URL('sdk-comment-reader-observer.ts.txt',import.meta.url),'utf8');
// Exact original constructor wrapper. Hash/remember are caller ports here; no hash policy is qualified.
const install=new Function('hashJson','return function(){'+stripTypeScriptTypes(wrapper,{mode:'transform'})+'}')(node=>node);
const pin=id=>({objectId:id,revision:1});
const leaf=sequence=>({schemaVersion:1,taskId:'task',kind:'comment',entry:{comment:{commentId:`c${sequence}`,sequence,body:'value'},receipt:null}});
const index=id=>({object:pin(id),length:1});
const tree={kind:'tree',root:{object:pin('root'),length:2},length:2};
const rows=[];
for(const name of ['success','hit','clone hit','remember','failure','remember failure','range','range remember','range failure','range identity']){
 let reads=0,failed=false;const events=[],raw='observer raw';
 const client={request:async(method,p)=>{assert.equal(method,'objects.read');reads++;const value=p.objectId==='root'?{schemaVersion:1,taskId:'task',kind:'page',children:[index('c1'),index('c2')]}:leaf(p.objectId==='c2'?2:1);if(name==='range identity'&&p.objectId==='c1')value.taskId='other';return{object:{contractKey:'task-board/comment-node',parentId:'task',ownerObjectId:'task'},revision:{revision:p.revision,contractVersion:'1.0.0'},content:{encoding:'json',value}};}};
 const owner={reader:new CommentReader(client),remember(node,p){events.push({pin:p,kind:node.kind});if(name.includes('failure')&&!failed&&(!name.includes('range')||p.objectId==='c1')){failed=true;throw raw;}}};
 install.call(owner);const reader=owner.reader;
 if(name.includes('remember')){reader.remember(pin('c1'),leaf(1));assert.equal(events.length,0);}
 if(name.includes('range')){
  if(name.includes('failure')){await assert.rejects(reader.range(tree,'task'),error=>error===raw);await reader.node(pin('c1'),'task');await reader.node(pin('c2'),'task');}
  else if(name.includes('identity')){await assert.rejects(reader.range(tree,'task'),{name:'Error',message:'The conversation snapshot belongs to another ticket.'});await reader.node(pin('c2'),'task');}
  else await reader.range(tree,'task');
 }else{
  if(name.includes('failure'))await assert.rejects(reader.node(pin('c1'),'task'),error=>error===raw);else await reader.node(pin('c1'),'task');
  if(name.includes('hit')||name.includes('failure')||name.includes('clone')){const sharedHandle=reader;await sharedHandle.node(pin('c1'),'task');}
 }
 rows.push({name,events,reads});
}
process.stdout.write(JSON.stringify({sourceHash:sha(source),wrapperHash:sha(wrapper),scope:'Exact original wrapper with supplied hash/remember ports; no publication hash or JS clone API claim.',cases:rows}));
