import {readFileSync,mkdirSync,writeFileSync,symlinkSync,readdirSync} from 'node:fs';
import {readdir,realpath} from 'node:fs/promises';
import {join,dirname,isAbsolute} from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
if(input.action==='prepare'){
  mkdirSync(input.root,{recursive:true});
  for(const name of ['.hidden','zeta','Alpha','alpha-two','é','e\u0301','Grüße😀','10','2','Å','A\u030A'])mkdirSync(join(input.root,name));
  writeFileSync(join(input.root,'plain.txt'),'plain');symlinkSync(join(input.root,'Alpha'),join(input.root,'directory-link'),process.platform==='win32'?'junction':'dir');
  const names=readdirSync(input.root,{withFileTypes:true}).filter(entry=>entry.isDirectory()).map(entry=>entry.name);
  console.log(JSON.stringify({paths:[input.root,join(input.root,'Alpha'),join(input.root,'missing'),join(input.root,'plain.txt'),'relative',input.root+'-missing\ud800',...(process.platform==='win32'?['\\\\?\\'+input.root.replaceAll('/','\\')]:[])],pairs:names.flatMap(left=>names.map(right=>({left,right,result:Math.sign(left.localeCompare(right))})))}));
}else{
  const source=readFileSync('services/agent-manager/src/directories.ts','utf8'),file=ts.createSourceFile('directories.ts',source,ts.ScriptTarget.Latest,true);let declaration;
  function visit(node){if(ts.isFunctionDeclaration(node)&&node.name?.text==='listDirectories')declaration=node.getText(file).replace(/^export\s+/,'');ts.forEachChild(node,visit);}visit(file);
  const list=vm.runInNewContext(ts.transpile(declaration+'\nlistDirectories;',{target:ts.ScriptTarget.ES2022}),{readdir,realpath,join,dirname,isAbsolute,IvyError,requireThat});
  try{console.log(JSON.stringify({result:await list({path:input.path})}));}catch(error){console.log(JSON.stringify({error:IvyError.from(error).toWire()}));}
}
