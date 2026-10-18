// Original service code runs unchanged against caller-owned SQLite files.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync,writeFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {gunzipSync} from 'node:zlib';
import {createHmac} from 'node:crypto';
registerHooks({
  resolve(specifier,context,next){
    if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
      const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
      if(existsSync(fileURLToPath(source)))return next(source.href,context);
    }
    return next(specifier,context);
  },
  load(url,context,next){
    if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
    return next(url,context);
  }
});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {queryObjects}=await import('../../services/hive/src/query.ts');
const {catalogPage}=await import('../../services/hive/src/paging.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.copyTo){
  const bytes=gunzipSync(readFileSync(new URL('./hive-current.sqlite.gz',import.meta.url)));
  for(const target of input.copyTo)writeFileSync(target,bytes);
}
let store=new HiveStore(input.path);
if(!input.retained){
  store.setMetadata('cursor_secret','0123456789abcdef'.repeat(4));
  store.close();store=new HiveStore(input.path);
}
const field=(step,name)=>step[name+'Json']===undefined?step[name]:JSON.parse(step[name+'Json']);
try {
  if(input.mode==='vectors'){
    const identity={method:'fixture',filter:{b:2,a:1}};
    const token=store.cursor(identity,{sort:[{type:2,value:0}],id:'retained'});
    const secret=store.get("SELECT value FROM metadata WHERE key='cursor_secret'").value;
    const signBody=body=>body+'.'+createHmac('sha256',secret).update(body).digest('base64url');
    const sign=data=>signBody(Buffer.from(data).toString('base64url'));
    const steps=[];
    const read=(cursor,other=identity)=>steps.push({op:'readCursor',cursor,identity:other});
    for(const boundary of [null,false,0,'',1,'🙂',[],{},[1,null],{value:'é',nested:{z:true,a:0}}]){
      const index=steps.length;steps.push({op:'cursor',identity,boundary});
      steps.push({op:'readCursor',from:index,identity});
    }
    steps.push({op:'cursor',identityJson:'{"z":"\\ud800","a":"\\udfff"}',boundaryJson:'{"\\ud800":"\\udfff"}'});
    read(token);read(token,{filter:{a:1,b:2},method:'fixture'});
    read(token,{...identity,limit:50});read(token,{...identity,surface:'other'});
    for(const cursor of ['', '.', 'abc', '.a','a.','a.a','a.a.a',token+'.x',token+'=',token+'\n',' '+token,'é.'+token])read(cursor);
    for(const suffix of ['.','..ignored','..ignored.more','...'])read(token+suffix);
    const [body,signature]=token.split('.');
    const alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
    for(const last of alphabet)read(body+'.'+signature.slice(0,-1)+last);
    for(let size=1;size<=66;size++)read(body+'.'+'A'.repeat(size));
    for(const data of ['null','[]','false','1','{}','{"query":"wrong"}','{"query":'+JSON.stringify(hashJson(identity))+'}',
      JSON.stringify({query:hashJson(identity),boundary:{sort:[],id:'ok'}}),
      '{"query":"wrong","query":'+JSON.stringify(hashJson(identity))+',"boundary":"last"}',
      '{"query":'+JSON.stringify(hashJson(identity))+',"boundary":"\\ud800"}',
      '{"query":'+JSON.stringify(hashJson(identity))+',"boundary":1e999}',
      '{','"text"','not json'])read(sign(data));
    read(sign(Buffer.from([0xff,0x00])));
    const b=Buffer.from(JSON.stringify({query:hashJson(identity),boundary:'utf8 replacement'}));
    b[b.indexOf('u'.charCodeAt())]=255;read(sign(b));
    for(let tail=0;tail<4;tail++)read(signBody(Buffer.from(JSON.stringify({query:hashJson(identity),boundary:'x'.repeat(tail)})).toString('base64url')+'A'));
    steps.push({op:'readCursor',cursor:token,identity,at:8_000_000_000_000});
    steps.push({op:'reopen'});read(token);
    steps.push({op:'sql',sql:"UPDATE metadata SET value='replacement' WHERE key='cursor_secret'"});read(token);
    steps.push({op:'reopen'});read(token);
    process.stdout.write(JSON.stringify(steps));
  }else if(input.mode==='retainedVectors'){
    const identity={method:'retained-store',catalog:'fixture'};
    const steps=[{op:'cursor',identity,boundary:{after:'unchanged',sequence:42}},{op:'reopen'},{op:'readCursor',identity,from:0,at:8_000_000_000_000}];
    for(const family of store.all('SELECT key,media_type FROM contract_families ORDER BY key LIMIT 8')){
      steps.push({op:'query',params:{contractKey:family.key,includeArchived:true,limit:2}});
    }
    for(const object of store.all('SELECT id FROM objects ORDER BY id LIMIT 8'))steps.push({op:'read',params:{objectId:object.id}});
    process.stdout.write(JSON.stringify(steps));
  }else{
    const rows=[],values=[];
    for(const step of input.steps){
      let value,error=null;
      const realNow=Date.now;if(step.at!==undefined)Date.now=()=>step.at;
      try{
        switch(step.op){
          case 'sql':store.db.exec(step.sql);break;
          case 'reopen':store.close();store=new HiveStore(input.path);break;
          case 'cursor':value=store.cursor(field(step,'identity'),field(step,'boundary'));break;
          case 'readCursor':{
            const decoded=store.readCursor(field(step,'identity'),step.from===undefined?step.cursor:values[step.from]);
            value={present:decoded!==undefined,value:decoded===undefined?null:decoded};break;
          }
          case 'catalog':{
            const params={...step.params};if(step.from!==undefined)params.cursor=values[step.from].nextCursor;
            if(step.tokenFrom!==undefined)params.cursor=values[step.tokenFrom];
            value=catalogPage(store,field(step,'records'),field(step,'identity'),r=>r.key,params);break;
          }
          case 'query':{
            const params={...step.params};if(step.from!==undefined)params.cursor=values[step.from].nextCursor;
            if(step.tokenFrom!==undefined)params.cursor=values[step.tokenFrom];
            value=store.withBudget(()=>queryObjects(store,params),10000);break;
          }
          case 'read':value=new Objects(store).read(step.params);break;
          case 'stat':value=new Objects(store).stat(step.params);break;
          case 'scalar':value=store.withBudget(()=>store.db.prepare(step.sql).all(...(step.bindings??[])),step.overdue?-1:10000);break;
          default:throw Error('Unknown query trace action');
        }
      }catch(e){error=IvyError.from(e).toWire();}finally{Date.now=realNow;}
      values.push(value);
      rows.push({value:value===undefined?null:JSON.stringify(value),error});
    }
    process.stdout.write(JSON.stringify(rows));
  }
}finally{store.close();}
