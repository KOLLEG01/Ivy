import {readFileSync,writeFileSync,mkdirSync,statSync,linkSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
import {jsonFile,atomicJson} from '../../dist/packages/host-runtime/src/config.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
const input=JSON.parse(readFileSync(0,'utf8')),OriginalDate=Date;
const source=readFileSync('services/agent-manager/src/process-evidence.ts','utf8');
const start=source.indexOf('const evidencePath =');if(start<0)throw Error('Original evidence module boundary changed');
const code=stripTypeScriptTypes(source.slice(start).replace(/^export /gm,''),{mode:'transform'});
// Scope the domain's injected clock lexically. The real shared atomic helper
// keeps its own deadline clock, as it does in the Rust production boundary.
const moduleWithClock=new Function('join','jsonFile','atomicJson','requireThat','Date','process',code+'\nreturn {ownedProcessStops,retainOwnedProcessStop};');
const now=OriginalDate.parse('2026-10-19T12:34:56.789Z'),cutoff=now-14*24*60*60*1000;
const iso=ms=>new OriginalDate(ms).toISOString(),row=(epoch,at=iso(now))=>({epoch,observedAt:at});
const cases=[],dates=new Set();
function add(name,value,options={}){
 const bytes=options.bytes??Buffer.from(JSON.stringify(value));
 const addDate=value=>{if(value&&typeof value==='object'){if(typeof value.observedAt==='string')dates.add(value.observedAt);for(const child of Object.values(value))addDate(child);}};
 try{addDate(JSON.parse(bytes.toString('utf8')));}catch{}
 cases.push({name,bytes:bytes.toString('hex'),kind:options.kind??'file',operation:options.operation??'read',epoch:options.epoch??'new-epoch',now:options.now??[now]});
}
add('missing',null,{kind:'missing'});add('directory',null,{kind:'directory'});
for(const [name,value] of [['null',null],['object',{}],['text','text'],['number',1],['empty',[]],['one',[row('old')]],['missing-epoch',[{observedAt:iso(now)}]],['missing-time',[{epoch:'old'}]],['extra',[{...row('old'),extra:1}]],['null-row',[null]],['array-row',[[]]],['epoch-number',[row(1)]],['empty-epoch',[row('')]],['epoch256',[row('😀'.repeat(128))]],['epoch258',[row('😀'.repeat(129))]],['invalid-date',[row('x','invalid')]],['date-number',[row('x',1)]],['date-only',[row('x','2026-10-19')]],['offset',[row('x','2026-10-19T14:34:56.789+02:00')]],['rfc',[row('x','Mon, 19 Oct 2026 12:34:56 GMT')]],['extended',[row('x','+010000-01-01T00:00:00.000Z')]],['duplicates',[row('same'),row('same')]],['rows256',Array.from({length:256},(_,i)=>row('e'+i))],['rows257',Array.from({length:257},(_,i)=>row('e'+i))]])add(name,value);
add('bad-json',null,{bytes:Buffer.from('{')});
add('duplicate-property',null,{bytes:Buffer.from('[{"epoch":"before","epoch":"after","observedAt":"'+iso(now)+'"}]')});dates.add(iso(now));
add('replaced-utf8-epoch',null,{bytes:Buffer.concat([Buffer.from('[{"epoch":"'),Buffer.from([0xff]),Buffer.from('","observedAt":"'+iso(now)+'"}]')])});
add('at-file-bound',null,{bytes:Buffer.from('[]'+' '.repeat(64*1024-2))});
add('over-file-bound',null,{bytes:Buffer.from('[]'+' '.repeat(64*1024-1))});
add('hard-link',[row('linked')],{kind:'hard-link'});
add('retain-missing',null,{kind:'missing',operation:'retain'});
add('retain-empty',[],{operation:'retain'});
add('retain-threshold',[row('before',iso(cutoff-1)),row('equal',iso(cutoff)),row('after',iso(cutoff+1)),row('future',iso(now+1))],{operation:'retain'});
add('retain-duplicates',[row('same'),row('other'),row('same')],{operation:'retain',epoch:'same'});
add('retain-max',Array.from({length:256},(_,i)=>row('e'+i)),{operation:'retain'});
add('retain-original-date-text',[row('date','2026-10-19'),row('offset','2026-10-19T14:34:56.789+02:00'),row('rfc','Mon, 19 Oct 2026 12:34:56 GMT')],{operation:'retain'});
add('retain-clock-per-row',[row('new-epoch',iso(now)),row('first',iso(cutoff+1)),row('second',iso(cutoff+1)),row('third',iso(cutoff+2))],{operation:'retain',now:[now,now+1,now+2]});
add('retain-invalid',[{epoch:'prior',observedAt:'invalid'}],{operation:'retain'});
add('retain-bad-json',null,{operation:'retain',bytes:Buffer.from('{')});
add('retain-directory',null,{operation:'retain',kind:'directory'});
add('retain-empty-epoch',[],{operation:'retain',epoch:''});
const parsed=[...dates].map(date=>({date,millis:Number.isFinite(OriginalDate.parse(date))?OriginalDate.parse(date):null}));
for(const c of cases){
 const root=join(input.root,c.name);mkdirSync(root,{recursive:true});const path=join(root,'owned-process-stops.json');
 if(c.kind==='directory')mkdirSync(path);else if(c.kind!=='missing'){
  const bytes=Buffer.from(c.bytes,'hex');
  if(c.kind==='hard-link'){const target=join(root,'original');writeFileSync(target,bytes);linkSync(target,path);}else writeFileSync(path,bytes);
 }
 const trace={parse:[],observedAt:[],now:[]};let position=0;
 const EvidenceDate=class extends OriginalDate{
  constructor(...args){super(...(args.length?args:[now]));if(!args.length)trace.observedAt.push(iso(now));}
  static now(){const value=c.now[Math.min(position++,c.now.length-1)];trace.now.push(value);return value;}
  static parse(value){trace.parse.push(value);return OriginalDate.parse(value);}
 };
 const {ownedProcessStops,retainOwnedProcessStop}=moduleWithClock(join,jsonFile,atomicJson,requireThat,EvidenceDate,process);
 let reply;try{reply={ok:true,value:c.operation==='read'?await ownedProcessStops(root):(await retainOwnedProcessStop(root,c.epoch),null)};}catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
 let fileHash=null;try{if(statSync(path).isFile())fileHash='sha256:'+createHash('sha256').update(readFileSync(path)).digest('hex');}catch{}
 c.dateParseCalls=trace.parse;delete trace.parse;
 c.expected={reply,fileHash,trace};
}
process.stdout.write(JSON.stringify({clock:iso(now),parsed,cases}));
