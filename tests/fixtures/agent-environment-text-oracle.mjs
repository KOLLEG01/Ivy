// Expose original private text helpers; do not replace their implementation.
import{registerHooks}from'node:module';
import{readFileSync}from'node:fs';
import{digest}from'../../dist/packages/contracts/src/canonical.js';
import{IvyError}from'../../dist/packages/contracts/src/errors.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return{format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {managedBlock,codexBlock,renderCodex,withBlock,beginCodex,endCodex,beginMcp,endMcp,beginPermissions,endPermissions};\n',shortCircuit:true};
  return next(url,context);
}});
const original=await import('../../dist/services/agent-manager/src/environment.js');
hook.deregister();
const rows=[];
function add(op,input){let expected;try{expected={result:op==='managed'?original.managedBlock(input.source,input.begin,input.end):op==='codex'?original.codexBlock(input.source,new Set(input.hashes)):op==='render'?original.renderCodex(input.sections):original.withBlock(input.source,input.block)};}catch(error){expected={error:IvyError.from(error).toWire()};}rows.push({op,...input,expected});}
for(const [begin,end]of[[original.beginCodex,original.endCodex],[original.beginMcp,original.endMcp],[original.beginPermissions,original.endPermissions],['😀begin','äend']]){
  for(const source of ['', 'unmanaged\r\n',begin,end,end+begin,begin+end,begin+'\nbody 😀\r\n'+end,'prefix'+begin+'\n'+end+'suffix',begin+begin+end,begin+end+end,begin+'\n'+end+'\n'+begin+'\n'+end,'"'+begin+'"\n"'+end+'"'])add('managed',{source,begin,end});
}
for(const sections of [null,[],['[mcp_servers."fixture"]\nurl = "https://example.invalid/mcp"'],['[apps."fixture"]','enabled = false','','[mcp_servers."fixture"]\nurl = "https://example.invalid/mcp"'],[''],['α😀\r\n','x']])add('render',{sections});
const roots=['','\ufeff','\ufeff\ufeff','\u0085x\u0085','\u200b','\ufeff  # keep\r\n\r\n\r\n\r\n[other]\r\nx=1\r\n','x=1\r[other]\nx=2','x=1\u2028\t[other]\u2029x=2',' [single]\nx=1','\t[[array]]\nx=1','# comment\nroot=1','\n\r\n\n\r\n\n','x\r\r\ny','x\n\n\ny'];
for(const source of roots)for(const block of [null,'','BLOCK\r\n','\n'])add('with',{source,block});
for(const unit of [9,10,11,12,13,32,0x85,0xa0,0x1680,0x180e,0x2000,0x200b,0x2028,0x2029,0x202f,0x205f,0x3000,0xfeff])add('with',{source:String.fromCodePoint(unit)+'x=1'+String.fromCodePoint(unit),block:'BLOCK'});
const {beginCodex:b,endCodex:e}=original;
const sections=[[],['[apps."fixture"]\nenabled = false',''],['[mcp_servers."fixture"]\nurl = "https://example.invalid/mcp"'],['[apps."fixture"]\nenabled = false','','[mcp_servers."fixture"]\nurl = "https://example.invalid/mcp"']];
for(const parts of sections){
  const block=original.renderCodex(parts),hashes=[digest(block)];
  const variants=[block,'prefix\r\n'+block+'\ntrailing',block.replaceAll('\n','\r\n'),block.replace(b,b+'\nuser_key = "preserved"'),block.replace(e,'[mcp_servers."other"]\nurl = "https://other.invalid"\n'+e),block.slice(0,-e.length),block.replace('approval_policy = "never"','approval_policy = "ask"'),block.replace('approval_policy = "never"','approval_policy = "never"\napproval_policy = "never"'),block.replace('default_permissions = ":danger-full-access"','default_permissions = ":danger-full-access" # edit'),block.replace('\n','\u2028'),block+e,b+block,block.replace(e,'\t[extra]\nvalue=1\n'+e)];
  for(const source of variants)add('codex',{source,hashes});
  add('codex',{source:block,hashes:[]});
  add('codex',{source:block,hashes:[digest(block),digest('other')]});
}
for(const source of ['', 'unmanaged',e,e+b,b,b+'\napproval_policy = "never"\ndefault_permissions = ":danger-full-access"\r',b+'\napproval_policy = "never"\rdefault_permissions = ":danger-full-access"\n'+e])add('codex',{source,hashes:[digest(original.renderCodex([]))]});
process.stdout.write(JSON.stringify(rows));
