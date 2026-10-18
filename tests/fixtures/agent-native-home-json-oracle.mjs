import {homedir} from 'node:os';
import {posix,win32} from 'node:path';
import {resolveCodexHome,expectedServerHome,usesExternalAppServer,instanceOwnedCodexHome} from '../../dist/packages/host-runtime/src/codex-home.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const capture=fn=>{try{return {result:fn()};}catch(e){return {error:IvyError.from(e).toWire()};}};
const environment=Object.fromEntries(['CODEX_HOME','USERPROFILE','HOME'].filter(k=>process.env[k]!==undefined).map(k=>[k,process.env[k]]));
const context={nativePlatform:process.platform,processCwd:process.cwd(),lexicalCwds:{win32:win32.resolve(),linux:posix.resolve()},accountHome:homedir(),environment,driveCwds:Object.fromEntries(Object.entries(process.env).filter(([k])=>/^=[a-z]:$/i.test(k)).map(([k,v])=>[k.slice(1),v]))};
const values=[undefined,null,false,0,1,true,'',' ','/fixture/data','/fixture/data/home','C:\\fixture\\data','C:\\fixture\\data\\home',[],{},['x']];
const settings=[null,false,0,1,true,'settings',[],{}];
for(const codexHome of values)for(const nativeHome of values)settings.push({...codexHome!==undefined?{codexHome}:{},...nativeHome!==undefined?{nativeHome}:{}});
const servers=[null,false,0,'owned-stdio',[],{},...['owned-stdio','claude-adapter','unknown',null].map(mode=>({mode})),{mode:'external-proxy'},...[null,false,0,1,'','relative',[],{},'C:\\fixture\\data'].map(expectedCodexHome=>({mode:'external-proxy',expectedCodexHome}))];
for(const appServer of servers)for(const home of [{},{codexHome:null},{codexHome:'C:\\fixture\\data\\home'},{nativeHome:'/fixture/data/home'}])settings.push({...home,appServer});
const rows=[];
for(const platform of ['win32','linux']){
  const paths=platform==='win32'?win32:posix,options={platform,environment:{},userHome:platform==='win32'?'C:\\fixture\\user':'/fixture/user',cwd:platform==='win32'?'C:\\fixture\\work':'/fixture/work'};
  for(const value of settings){
    const ambient=capture(()=>resolveCodexHome(value,{platform}));
    const roots=platform==='win32'?['C:\\fixture\\data','C:\\fixture\\data\\home','C:\\fixture','D:\\other']:['/fixture/data','/fixture/data/home','/fixture','/other'];
    if(ambient.result)roots.push(paths.dirname(ambient.result.path),ambient.result.path);
    rows.push({platform,settings:value,options,roots,expected:{resolution:capture(()=>resolveCodexHome(value,options)),external:capture(()=>usesExternalAppServer(value)),expectedHome:capture(()=>expectedServerHome(value)),owned:roots.map(root=>capture(()=>instanceOwnedCodexHome(value,root,platform)))}});
  }
}
process.stdout.write(JSON.stringify({context,rows}));
