import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {lstatSync,mkdirSync,readFileSync,readdirSync,readlinkSync,symlinkSync,writeFileSync,linkSync,chmodSync} from 'node:fs';
import {lstat,realpath,rm} from 'node:fs/promises';
import {isAbsolute,join,parse,relative,resolve,sep} from 'node:path';
import {stripTypeScriptTypes} from 'node:module';
import {atomicJson,jsonFile} from '../../dist/packages/host-runtime/src/config.js';
import {resolveCodexHome} from '../../dist/packages/host-runtime/src/codex-home.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';

const input=JSON.parse(readFileSync(0,'utf8')), OriginalDate=Date;
const source=readFileSync('services/agent-manager/src/runtime-reset.ts','utf8');
const start=source.indexOf('interface ResetMarker');assert(start>=0);
const body=stripTypeScriptTypes(source.slice(start).replace(/^export /gm,''),{mode:'transform'});
const load=new Function('join','parse','resolve','lstat','realpath','rm','atomicJson','jsonFile','resolveCodexHome','requireThat','Date',body+'\nreturn applyAgentRuntimeReset;');
const now='2026-10-19T12:34:56.789Z';
const marker={schemaVersion:1,resetId:'reset',runtimeEpoch:'epoch',completedAt:now};
const cases=[];
function add(name,value=marker,options={}){
 cases.push({name,markerKind:'file',markerBytes:Buffer.from(JSON.stringify(value)).toString('hex'),appliedKind:'missing',appliedBytes:'',homeKind:'directory',settings:{codexHome:'$home'},repeat:1,...options});
}
for(const [name,value]of [['null',null],['false',false],['zero',0],['empty-string',''],['true',true],['one',1],['array',[]],['object',{}]])add('marker-'+name,value);
add('missing',marker,{markerKind:'missing'});add('directory',marker,{markerKind:'directory'});
add('bad-json',marker,{markerBytes:Buffer.from('{').toString('hex')});
add('hard-link',marker,{markerKind:'hard-link'});
for(const key of ['schemaVersion','resetId','runtimeEpoch','completedAt']){
 const value={...marker};delete value[key];add('missing-'+key,value);
 add('null-'+key,{...marker,[key]:null});
}
for(const key of ['resetId','runtimeEpoch']){
 for(const [name,value]of [['empty',''],['number',1],['128-units','😀'.repeat(64)],['130-units','😀'.repeat(65)],['lone-surrogate','\ud800']])add(key+'-'+name,{...marker,[key]:value});
}
for(const [name,date]of [
 ['canonical',now],['year-zero','0000-02-29T00:00:00.000Z'],['negative-year','-000001-01-01T00:00:00.000Z'],['expanded','+010000-01-01T00:00:00.000Z'],
 ['minimum','-271821-04-20T00:00:00.000Z'],['maximum','+275760-09-13T00:00:00.000Z'],['below-minimum','-271821-04-19T23:59:59.999Z'],['above-maximum','+275760-09-13T00:00:00.001Z'],
 ['invalid','invalid'],['empty',''],['no-ms','2026-10-19T12:34:56Z'],['date-only','2026-10-19'],['offset','2026-10-19T14:34:56.789+02:00'],['rfc','Mon, 19 Oct 2026 12:34:56 GMT'],
 ['normalized-day','2024-02-30T00:00:00.000Z'],['normalized-hour','2024-01-01T24:00:00.000Z'],['expanded-short','+000001-01-01T00:00:00.000Z'],['negative-zero','-000000-01-01T00:00:00.000Z'],
 ['lowercase','2026-10-19t12:34:56.789z'],['unicode','😀'],['nul',now+'\0']
])add('date-'+name,{...marker,completedAt:date});
add('invalid-shape-before-date',{...marker,resetId:1,completedAt:'invalid'});
add('extra-fields',{...marker,extra:{value:'preserved',number:7},appliedAt:'replaced',__proto__:null});
add('duplicate-property',marker,{markerBytes:Buffer.from('{"schemaVersion":0,"schemaVersion":1,"resetId":"reset","runtimeEpoch":"epoch","completedAt":"'+now+'"}').toString('hex')});
add('replacement-utf8',marker,{markerBytes:Buffer.concat([Buffer.from(JSON.stringify(marker).slice(0,-1)+',"extra":"'),Buffer.from([255]),Buffer.from('"}')]).toString('hex')});
const exact=Buffer.from(JSON.stringify(marker));
add('at-marker-bound',marker,{markerBytes:Buffer.concat([exact,Buffer.alloc(1024*1024-exact.length,32)]).toString('hex')});
add('over-marker-bound',marker,{markerBytes:Buffer.concat([exact,Buffer.alloc(1024*1024+1-exact.length,32)]).toString('hex')});
for(const [name,value]of [['same',{schemaVersion:1,resetId:'reset',runtimeEpoch:'epoch',completedAt:'invalid',foreign:1}],['other-reset',{...marker,resetId:'other'}],['other-epoch',{...marker,runtimeEpoch:'other'}],['other-schema',{...marker,schemaVersion:2}],['null',null],['false',false],['array',[]]])add('applied-'+name,marker,{appliedKind:'file',appliedBytes:Buffer.from(JSON.stringify(value)).toString('hex')});
add('applied-malformed',marker,{appliedKind:'file',appliedBytes:Buffer.from('{').toString('hex')});
add('applied-directory',marker,{appliedKind:'directory'});
add('applied-oversize',marker,{appliedKind:'file',appliedBytes:Buffer.alloc(1024*1024+1,32).toString('hex')});
for(const [name,settings]of [['native',{nativeHome:'$home'}],['both',{codexHome:'$home',nativeHome:'$home'}],['conflict',{codexHome:'$home',nativeHome:'$other'}],['absent',{}],['null',null],['primitive',5],['empty',{codexHome:''}],['relative',{codexHome:'relative'}],['null-codex',{codexHome:null}],['null-native',{nativeHome:null}],['null-select-native',{codexHome:null,nativeHome:'$home'}],['root',{codexHome:'$root'}]])add('settings-'+name,marker,{settings});
for(const homeKind of ['missing','file','junction','ancestor-file'])add('home-'+homeKind,marker,{homeKind});
add('inner-junction');add('readonly');add('idempotent',marker,{repeat:2});
add('invalid-settings-acknowledged',marker,{settings:null,appliedKind:'file',appliedBytes:Buffer.from(JSON.stringify(marker)).toString('hex')});
// Retain the known Host atomic-json error-effect mismatch as the last case,
// so it cannot prevent qualification of the other independent reset cases.
add('atomic-unsafe-extra',{...marker,extra:{number:9007199254740992}});
function snapshot(root){
 const result=[];
 function walk(path,relative=''){
  for(const name of readdirSync(path).sort()){
   const full=join(path,name),entry=relative+name.replace(/^runtime-reset-applied\.json\.[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.tmp$/,'runtime-reset-applied.json.<uuid>.tmp'),info=lstatSync(full);
   if(info.isSymbolicLink()){assert(readlinkSync(full));result.push({path:entry,kind:'link'});}
   else if(info.isDirectory()){result.push({path:entry,kind:'directory'});walk(full,entry+'/');}
   else {const bytes=readFileSync(full);result.push({path:entry,kind:'file',bytes:bytes.length,hash:'sha256:'+createHash('sha256').update(bytes).digest('hex')});}
  }
 }walk(root);return result;
}
function settings(value,root,home){
 if(value===null||typeof value!=='object')return value;
 return Object.fromEntries(Object.entries(value).map(([key,value])=>[key,value==='$home'?home:value==='$other'?join(root,'other'):value==='$root'?parse(resolve(root)).root:value]));
}
for(const c of cases){
 const root=join(input.root,c.name),data=join(root,'data'),home=join(root,'home');mkdirSync(data,{recursive:true});
 const markerPath=join(data,'runtime-reset.json'),appliedPath=join(data,'runtime-reset-applied.json');
 if(c.markerKind==='directory')mkdirSync(markerPath);else if(c.markerKind!=='missing'){
  const bytes=Buffer.from(c.markerBytes,'hex');if(c.markerKind==='hard-link'){writeFileSync(join(data,'marker-source'),bytes);linkSync(join(data,'marker-source'),markerPath);}else writeFileSync(markerPath,bytes);
 }
 if(c.appliedKind==='directory')mkdirSync(appliedPath);else if(c.appliedKind==='file')writeFileSync(appliedPath,Buffer.from(c.appliedBytes,'hex'));
 if(c.homeKind==='file'||c.homeKind==='ancestor-file')writeFileSync(home,'not a directory');
 else if(c.homeKind!=='missing'){
  const actual=c.homeKind==='junction'?join(root,'actual'):home;mkdirSync(actual);
  for(const name of ['sessions','archived_sessions','shell_snapshots','tmp']){mkdirSync(join(actual,name));writeFileSync(join(actual,name,'leaf'),'removed '+name);}
  for(const name of ['history.jsonl','state_5.sqlite','state_5.sqlite-wal','state_5.sqlite-shm'])writeFileSync(join(actual,name),'removed '+name);
  for(const name of ['config.toml','AGENTS.md','state_6.sqlite','foreign'])writeFileSync(join(actual,name),'preserved '+name);
  if(c.homeKind==='junction')symlinkSync(actual,home,process.platform==='win32'?'junction':'dir');
 }
 if(c.name==='inner-junction'){
  await rm(join(home,'sessions'),{recursive:true,force:true});mkdirSync(join(root,'external'));writeFileSync(join(root,'external','leaf'),'preserve link target');symlinkSync(join(root,'external'),join(home,'sessions'),process.platform==='win32'?'junction':'dir');
 }
 if(c.name==='readonly')chmodSync(join(home,'sessions','leaf'),0o400);
 const accountHome=join(root,'account');
 if(c.name==='settings-null-codex'){
  mkdirSync(join(accountHome,'.codex','sessions'),{recursive:true});
  writeFileSync(join(accountHome,'.codex','sessions','leaf'),'fixture fallback history');
 }
 let reads=0;
 const FixedDate=class extends OriginalDate{constructor(...args){super(...(args.length?args:[now]));if(!args.length)reads++;}};
 const fixtureRemove=async(path,options)=>{
  const local=relative(resolve(root),resolve(path));
  assert(local&&local!=='..'&&!local.startsWith('..'+sep)&&!isAbsolute(local),'Reset oracle removal leaves its fixture');
  return rm(path,options);
 };
 const fixtureHome=settings=>resolveCodexHome(settings,{environment:{},userHome:accountHome,cwd:root});
 const apply=load(join,parse,resolve,lstat,realpath,fixtureRemove,atomicJson,jsonFile,fixtureHome,requireThat,FixedDate);
 let selected=settings(c.settings,root,c.homeKind==='ancestor-file'?join(home,'nested'):home),reply;
 try{for(let i=0;i<c.repeat;i++)await apply(data,selected);reply={ok:true};}catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
 c.expected={reply,clockReads:reads,files:snapshot(root)};
}
console.log(JSON.stringify({now,cases}));
