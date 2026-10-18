import {registerHooks} from 'node:module';
import {existsSync,mkdtempSync,mkdirSync,readFileSync,writeFileSync,rmSync,symlinkSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
if(process.argv[2]==='command'){
  process.argv=[process.execPath,fileURLToPath(new URL('../../packages/cli/src/publish-ui.ts',import.meta.url)),...process.argv.slice(3)];
  await import('../../packages/cli/src/publish-ui.ts');
}else{
const {readUiBundle,publishUi}=await import('../../packages/cli/src/publish-ui.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const root=mkdtempSync(join(tmpdir(),'ivy-rust-publication-'));
const definition={metadata:{uiId:'fixture-ui',displayName:'Fixture',description:'Publication fixture \ud800',iconKey:'ui'},entryPath:'index.html',requirements:{hiveProtocol:1,contracts:[],services:[]},dataContracts:[]};
const contract=key=>({key,version:'1.0.0',owner:{kind:'agent'},mediaType:'text/plain',retention:{objects:{mode:'retain'},revisions:{mode:'all'}},specMarkdown:'Fixture contract.'});
const mutationId='fixture-epoch:1792290000000:original-\ud800';
const failure=e=>{e=IvyError.from(e);return {ok:false,error:e.toWire()};};
const capture=async fn=>{try{return {ok:true,value:await fn()};}catch(e){return failure(e);}};
const rows=[];let sequence=0;
function fixture(files={'index.html':'<h1>Fixture</h1>'}){
  const path=join(root,String(++sequence));mkdirSync(path);
  for(const [name,bytes]of Object.entries(files)){mkdirSync(join(path,name,'..'),{recursive:true});writeFileSync(join(path,name),bytes);}
  return path;
}
async function scan(name,files,custom=definition){const directory=fixture(files);rows.push({kind:'scan',name,directory,definition:JSON.stringify(custom),expected:JSON.stringify(await capture(()=>readUiBundle(directory,custom)))});return directory;}
try{
 const basic={'index.html':'<h1>Fixture</h1>','main.js':'\ufeffdocument.body.textContent="fixture";','data.json':' {"z":-0,"a":"\\ud800","x":1e-7} ','ivy-ui.json':'ignored','LICENSE':'notice','sub/é.txt':'é','\ue000.txt':'bmp','𐀀.txt':'astral'};
 for(const extension of ['css','txt','md','wasm','png','jpg','jpeg','gif','webp','svg','ico','woff','woff2'])basic['asset.'+extension]=extension==='css'?'body{}':Buffer.from([0,1,2]);
 await scan('all-media-and-order',basic);
 for(const name of ['unsupported.exe','case.HTML','bad%.txt','bad#.txt','e\u0301.txt','.hidden','nested/LICENSE'])await scan(name,{'index.html':'', [name]:'x'});
 for(const [name,bytes]of [['text-bad-utf8',Buffer.from([0xc0,0x80])],['text-bom',Buffer.from('\ufeffx')],['text-exact',Buffer.alloc(1024*1024,120)],['text-over',Buffer.alloc(1024*1024+1,120)]])await scan(name,{'index.html':'','sample.txt':bytes});
 for(const [name,bytes]of [['json-bom',Buffer.from('\ufeff{"x":1}')],['json-bad-utf8',Buffer.from([0xff])],['json-bad-syntax',Buffer.from('{')],['json-unsafe',Buffer.from('9007199254740992')],['json-duplicate',Buffer.from('{"x":1,"x":2}')],['json-over',Buffer.from(JSON.stringify('x'.repeat(1024*1024)))],['json-depth','['.repeat(130)+'0'+']'.repeat(130)]])await scan(name,{'index.html':'','sample.json':bytes});
 await scan('binary-exact',{'index.html':'','sample.png':Buffer.alloc(8*1024*1024)});
 await scan('binary-over',{'index.html':'','sample.png':Buffer.alloc(8*1024*1024+1)});
 await scan('bundle-exact',{'index.html':'',...Object.fromEntries([1,2,3,4].map(i=>[i+'.png',Buffer.alloc(8*1024*1024)]))});
 await scan('bundle-over',{'index.html':'x',...Object.fromEntries([1,2,3,4].map(i=>[i+'.png',Buffer.alloc(8*1024*1024)]))});
 for(const count of [2000,2001])await scan('file-count-'+count,{'index.html':'',...Object.fromEntries(Array.from({length:count-1},(_,i)=>[i+'.txt','']))});
 await scan('missing-entry',{'other.html':''});await scan('invalid-definition',{'index.html':''},{});
 const linked=fixture(),target=fixture();symlinkSync(target,join(linked,'linked'),process.platform==='win32'?'junction':'dir');
 rows.push({kind:'scan',name:'linked-entry',directory:linked,definition:JSON.stringify(definition),expected:JSON.stringify(await capture(()=>readUiBundle(linked,definition)))});
 const linkedRoot=join(root,'root-link');symlinkSync(target,linkedRoot,process.platform==='win32'?'junction':'dir');
 for(const [name,directory]of [['linked-root',linkedRoot],['absent-root',join(root,'missing')],['file-root',join(target,'index.html')],['dot-root',join(target,'..',String(sequence))]])rows.push({kind:'scan',name,directory,definition:JSON.stringify(definition),expected:JSON.stringify(await capture(()=>readUiBundle(directory,definition)))});
 for(const mode of ['success','expected-release','changed','equivalent-json','json-bom','invalid-id','foreign-owner','fail-register','fail-asset','fail-deploy']){
  const directory=fixture({'index.html':'<h1>Fixture</h1>','data.json':mode==='json-bom'?'\ufeff{"x":1}':'{"x":1}','main.js':'\ufefffixture();'});
  const selected={...definition,dataContracts:[contract('fixture/first'),contract('fixture/second')]};
  if(mode==='foreign-owner')selected.dataContracts[1].owner={kind:'service',serviceName:'fixture'};
  const checkedBundle=await readUiBundle(directory,selected);
  if(mode==='changed')writeFileSync(join(directory,'main.js'),'changed();');
  if(mode==='equivalent-json')writeFileSync(join(directory,'data.json'),' {"x": 1} ');
  const trace=[];
  const client={async request(method,params){trace.push({method,params});if((mode==='fail-register'&&method==='contracts.register')||(mode==='fail-asset'&&method==='uis.stageAsset')||(mode==='fail-deploy'&&method==='uis.deploy'))throw new IvyError('outcome_unknown','Synthetic lost reply.','unknown');return {fixture:true,method};}};
  const input={directory,definition:selected,mutationId:mode==='invalid-id'?'bad':mutationId,expectedReleaseId:mode==='expected-release'?'previous-release':null,checkedBundle};
  rows.push({kind:'publish',name:mode,directory,definition:JSON.stringify(selected),mutationId:JSON.stringify(input.mutationId),expectedReleaseId:JSON.stringify(input.expectedReleaseId),checkedBundle,expected:JSON.stringify(await capture(()=>publishUi(client,input))),trace:JSON.stringify(trace)});
 }
 const nfc=[];for(let cp=0;cp<=0x10ffff;cp++){if(cp>=0xd800&&cp<=0xdfff)continue;const text=String.fromCodePoint(cp),value=text.normalize('NFC');if(text!==value)nfc.push([cp,value]);}
 const commands=[];
 if(process.env.IVY_PUBLISH_TEST_BINARY){
  const directory=fixture({'index.html':'<h1>Command</h1>','main.js':'\ufeffcommand();'});writeFileSync(join(directory,'ivy-ui.json'),JSON.stringify({...definition,metadata:{...definition.metadata,description:'Command fixture'}}));
  let trace=[];const server=createServer(async(req,res)=>{let text='';for await(const part of req)text+=part;const frame=JSON.parse(text);trace.push({path:req.url,authorization:req.headers.authorization,method:frame.method,params:frame.params});res.setHeader('content-type','application/json');res.end(JSON.stringify({jsonrpc:'2.0',id:frame.id,result:{fixture:true,method:frame.method}}));});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const base=`http://127.0.0.1:${server.address().port}/fixture`,valid=['--directory',directory,'--base',base,'--mutation-id','fixture:1:command','--initial'];
  const spawnCapture=(native,args,credential=true)=>new Promise((resolve,reject)=>{const env={...process.env};delete env.IVY_PUBLISH_CREDENTIAL;if(credential)env.IVY_PUBLISH_CREDENTIAL='synthetic-fixture';const child=spawn(native?process.env.IVY_PUBLISH_TEST_BINARY:process.execPath,native?args:['--experimental-transform-types','--disable-warning=ExperimentalWarning',fileURLToPath(import.meta.url),'command',...args],{env,windowsHide:true,stdio:['ignore','pipe','pipe']});let out='',err='';child.stdout.on('data',b=>out+=b);child.stderr.on('data',b=>err+=b);child.on('error',reject);child.on('close',code=>resolve({code,out,err}));});
  try{for(const [name,args,credential]of [
   ['success',valid,true],['release',[...valid.slice(0,-1),'--expected-release=old'],true],['equals',['--directory='+directory,'--base='+base,'--mutation-id=fixture:1:command','--initial','--'],true],
   ['empty',[],true],['unknown',['--unknown'],true],['positional',['x'],true],['initial-value',[...valid,'--initial=false'],true],['missing-value',['--directory'],true],['hyphen-value',['--directory','-x'],true],['literal-hyphen',[...valid,'--credential-env','-'],true],['both',[...valid,'--expected-release=x'],true],['no-credential',valid,false],['empty-environment',[...valid,'--credential-env='],true],['invalid-id',[...valid,'--mutation-id=bad'],true],['repeat',[...valid,'--initial'],true],
  ]){trace=[];const expected=await spawnCapture(false,args,credential),expectedTrace=JSON.stringify(trace);trace=[];const actual=await spawnCapture(true,args,credential);commands.push({name,expected,actual,expectedTrace,actualTrace:JSON.stringify(trace)});}}
  finally{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
 }
 process.stdout.write(JSON.stringify({root,rows,nfc,unicode:process.versions.unicode,commands}));
}catch(error){rmSync(root,{recursive:true,force:true});throw error;}
}

