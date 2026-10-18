import {homedir} from 'node:os';
import {resolveCodexHome,instanceOwnedCodexHome,expectedServerHome,usesExternalAppServer} from '../../dist/packages/host-runtime/src/codex-home.js';
const capture=fn=>{try{return {result:fn()};}catch(e){return {error:{code:e.code??'internal_error',message:e.message}};}};
const driveCwds=Object.fromEntries(Object.entries(process.env).filter(([key])=>/^=[a-z]:$/i.test(key)).map(([key,value])=>[key.slice(1),value]));
const rows=[];
for(const platform of ['win32','linux']){
  const cwd=platform==='win32'?'C:\\fixture\\work':'/fixture/work';
  const paths=['', ' ', '\ufeff\t', '.', '..', 'relative', '/', '/fixture/data', '/fixture/data/', '/fixture/data/home', '/fixture/database', '/fixture/data/../other', 'C:\\fixture\\data','c:/FIXTURE/data', 'C:\\fixture\\data\\', 'C:\\fixture\\data\\home', '\\fixture\\data', 'D:relative', '\\\\server\\share\\home', 'C:\\ΣΟΣ\\İ\\HOME', 'C:\\σος\\i\u0307\\home', 'C:\\fixture\\\ud800', '/fixture/\ud800'];
  const settings=[{},...paths.map(codexHome=>({codexHome})),...paths.map(nativeHome=>({nativeHome}))];
  for(const codexHome of paths)for(const nativeHome of paths)settings.push({codexHome,nativeHome});
  const profiles=[{environment:{},cwd},{environment:{CODEX_HOME:'  child/../native\ufeff'},cwd},{environment:{CODEX_HOME:'\u0085'},cwd},{environment:{CODEX_HOME:'  '},cwd},
    {environment:{CODEX_HOME:'C:child'},cwd},{environment:{CODEX_HOME:'D:child'},cwd},{environment:{CODEX_HOME:'\\root'},cwd},
    {environment:{HOME:'/env-home',USERPROFILE:'C:\\env-home'},cwd},
    {environment:{HOME:'',USERPROFILE:''},cwd},{environment:{HOME:'relative',USERPROFILE:'relative'},cwd},
    {environment:{HOME:'/ignored',USERPROFILE:'C:\\ignored'},userHome:platform==='win32'?'C:\\override':'/override',cwd},
    {environment:{},userHome:'',cwd},{environment:{},userHome:'relative',cwd},
    {environment:{codex_home:'ignored',home:'/ignored'},cwd},
    {environment:{CODEX_HOME:'child'}},
  ];
  const inputs=[...settings.map(settings=>({settings,options:{platform,environment:{HOME:'/env-home',USERPROFILE:'C:\\env-home'},cwd}})),...profiles.map(profile=>({settings:{},options:{platform,...profile}}))];
  const roots=platform==='win32'?['C:\\fixture\\data','c:\\FIXTURE\\data\\','C:\\fixture','D:\\other','\\\\server\\share'] : ['/fixture/data','/fixture/data/','/fixture','/other','/'];
  for(const {settings,options} of inputs){
    const modes=[undefined,{mode:'owned-stdio'},{mode:'claude-adapter'},{mode:'external-proxy'},{mode:'external-proxy',expectedCodexHome:''},{mode:'external-proxy',expectedCodexHome:'raw/../external'}];
    // expectedServerHome uses live defaults, so pin those separate ambient inputs.
    const servers=modes.map(appServer=>{const value={...settings,...(appServer?{appServer}:{})};return {mode:appServer??null,external:usesExternalAppServer(value),expected:capture(()=>expectedServerHome(value))};});
    rows.push({platform,settings,options,processCwd:process.cwd(),driveCwds,accountHome:homedir(),ambientEnvironment:Object.fromEntries(['CODEX_HOME','USERPROFILE','HOME'].filter(k=>process.env[k]!==undefined).map(k=>[k,process.env[k]])),roots,
      expected:{resolution:capture(()=>resolveCodexHome(settings,options)),owned:roots.map(root=>capture(()=>instanceOwnedCodexHome(settings,root,platform))),servers}});
  }
}
process.stdout.write(JSON.stringify(rows));
