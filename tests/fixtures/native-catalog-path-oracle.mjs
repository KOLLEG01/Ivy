import {existsSync,mkdirSync,writeFileSync} from 'node:fs';
import {join,dirname} from 'node:path';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {nativeCatalogPath}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {nativeVersions}=await import('../../packages/contracts/src/native-versions.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const root=process.argv[2],version=nativeVersions.at(-1),rows=[];
function collect(root,version,platform,architecture){
 let expected;try{expected={ok:true,value:nativeCatalogPath(root,version,platform,architecture)};}catch(e){expected={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({args:[root,version,platform,architecture].map(v=>v===undefined?null:JSON.stringify(v)),expected:JSON.stringify(expected)});
}
const prefixes=['','.', '..','./','../','/','//','///','\\','\\\\','\\\\\\','C:','c:/','C:\\','C:relative','\\\\server\\share','//server/share','\\\\?\\C:\\','\\\\.\\C:\\','\\\\?\\UNC\\server\\share','\\\\?\\COM1:','COM1:','NUL:','LPT²:','foo:','foo:/'];
const suffixes=['','a','a/../b','a\\..\\b','../..','./a//b/','a:b','a:/b','a\\NUL:x','a/COM1:x','a/../../C:/b','\\share\\..','é','e\u0301','日本語','😀','\ud800','\udfff','a\0b'];
for(const prefix of prefixes)for(const suffix of suffixes)collect(prefix+suffix,version,'win32','x64');
for(const rootValue of [root,root+'/',root+'/../'+root.split(/[\\/]/).at(-1)])for(const v of nativeVersions)collect(rootValue,v);
for(const value of ['','UPPER','x-y','x_y','x.y','x/y','x\\y','x\n','x\r','x\r\n','x\u2028','x\u2029',' x','x ','é','\ud800','a0','0','__proto__']){
 collect(root,version,value,'x64');collect(root,version,'win32',value);collect(root,value,'win32','x64');
}
for(const [name,kind] of [['selected','file'],['directory','directory'],['fallback','absent']]){
 const fixture=join(root,name),file=join(fixture,'specs','native','codex-'+version,'catalog.'+process.platform+'-'+process.arch+'.json');
 if(kind==='file'){mkdirSync(dirname(file),{recursive:true});writeFileSync(file,'irrelevant bytes');}
 if(kind==='directory')mkdirSync(file,{recursive:true});
 collect(fixture,version);collect(fixture,version,process.platform,process.arch);collect(fixture,version,'other','cpu');
}
// Node converts filesystem strings through UTF-8, replacing lone surrogates.
const replacement=join(root,'replacement\ufffd','specs','native','codex-'+version,'catalog.'+process.platform+'-'+process.arch+'.json');
mkdirSync(dirname(replacement),{recursive:true});writeFileSync(replacement,'irrelevant bytes');
for(const suffix of ['\ufffd','\ud800','\udfff'])collect(join(root,'replacement'+suffix),version);
process.stdout.write(JSON.stringify(rows));
