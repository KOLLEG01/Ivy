// Render native corpus documents in an isolated Chromium-compatible browser.
import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,mkdirSync,realpathSync} from 'node:fs';
import {rm} from 'node:fs/promises';
import {resolve,dirname,sep,basename} from 'node:path';
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {randomUUID,createHash} from 'node:crypto';
import {isDeepStrictEqual} from 'node:util';
const [corpusPath,outputPath]=process.argv.slice(2);
if(!corpusPath||!outputPath)throw Error('Supply corpus and output paths.');
const output=resolve(outputPath),allowed=resolve('outputs');
if(!output.startsWith(allowed+sep))throw Error('Output must be inside repository outputs.');
const profile=resolve(dirname(output),'auth-browser-'+randomUUID());
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const proof={checkedAt:new Date().toISOString(),execution:'not_run',finding:null,issue:null,observations:[],limits:'Local renderer harness only. No native listener, real credential/session/OAuth exchange, provider popup, live account, deployment or platform-wide browser acceptance.'};
proof.harness={path:'tests/acceptance/hive-auth-pages-browser.mjs',sha256:hash(readFileSync(new URL(import.meta.url)))};
let server,child,childClosed,ws,send,session,closed=false,sequence=0;const pending=new Map();
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const until=async fn=>{const end=Date.now()+20000;do{const value=await fn();if(value)return value;await delay(50);}while(Date.now()<end);throw Error('Browser observation deadline exceeded');};
try{
 const executable=process.env.IVY_TEST_BROWSER;
 if(!executable){proof.execution='blocked';proof.issue={code:'browser_unavailable',detail:'IVY_TEST_BROWSER is not configured.'};throw Error(proof.issue.detail);}
 const bytes=readFileSync(corpusPath),corpus=bytes.toString('utf8').trim().split(/\r?\n/).map(JSON.parse);
 assert.equal(corpus.length,6);assert.ok(corpus.every(c=>isDeepStrictEqual(c.original,c.native)));
 const login=corpus.find(c=>c.name==='native-login-pages-and-session-response-binding').native;
 const approval=corpus.find(c=>c.name==='oauth-approval-native-page-and-pending-state').native;
 const body=(data,id)=>data.events.find(e=>e.kind==='end'&&e.id===id).body;
 const headers=Object.fromEntries(login.events.filter(e=>e.kind==='header'&&e.id==='normal').map(e=>[e.name,JSON.parse(e.encoded)]));
 const documents={normal:body(login,'normal'),rejected:body(login,'rejected'),approval:body(approval,'approval')},theme=body(login,'theme');
 proof.corpus={path:corpusPath,sha256:hash(bytes),records:corpus.length};proof.browser={executable,sha256:hash(readFileSync(executable))};proof.documents=Object.fromEntries(Object.entries(documents).map(([k,v])=>[k,hash(v)]));proof.themeSha256=hash(theme);proof.headers=headers;
 const posts=[];
 server=createServer(async(req,res)=>{for(const[k,v]of Object.entries(headers))res.setHeader(k,v);const url=new URL(req.url,'http://fixture.test');
  if(req.method==='POST'){let bytes='';for await(const chunk of req)bytes+=chunk;posts.push({path:url.pathname,origin:req.headers.origin,body:bytes});res.writeHead(200,{'Content-Type':'text/html; charset=utf-8'});res.end('<!doctype html><title>Recorded</title>');return;}
  if(url.pathname==='/ivy/login/theme.js'){res.writeHead(200,{'Content-Type':'text/javascript; charset=utf-8'});res.end(theme);return;}
  if(url.pathname==='/blank'){res.writeHead(200,{'Content-Type':'text/html; charset=utf-8'});res.end('<!doctype html><title>Blank</title>');return;}
  const page=url.searchParams.get('page');if(documents[page]){res.writeHead(200,{'Content-Type':'text/html; charset=utf-8'});res.end(documents[page]);return;}
  res.writeHead(404);res.end();
 });await new Promise((r,j)=>{server.once('error',j);server.listen(0,'127.0.0.1',r);});const origin='http://127.0.0.1:'+server.address().port;
 mkdirSync(profile,{recursive:true});let stderr='';
 child=spawn(executable,['--headless=new','--disable-gpu','--no-first-run','--no-default-browser-check','--disable-background-networking','--disable-sync','--remote-debugging-port=0','--user-data-dir='+profile,'about:blank'],{windowsHide:true,stdio:['ignore','ignore','pipe']});
 childClosed=new Promise(r=>child.once('close',r));
 child.stderr.on('data',chunk=>{stderr=(stderr+chunk.toString()).slice(-65536);});let launchError;child.on('error',error=>{launchError=error;});
 const endpoint=await until(()=>{if(launchError)throw launchError;return stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/)?.[1];});
 ws=new WebSocket(endpoint);await new Promise((r,j)=>{ws.addEventListener('open',r,{once:true});ws.addEventListener('error',j,{once:true});});
 const exceptions=[];
 ws.addEventListener('message',event=>{const m=JSON.parse(event.data);if(m.id){const p=pending.get(m.id);if(p){pending.delete(m.id);clearTimeout(p.timer);m.error?p.reject(Error(JSON.stringify(m.error))):p.resolve(m.result);}}else if(m.method==='Runtime.exceptionThrown')exceptions.push(m.params.exceptionDetails.text);});
 ws.addEventListener('close',()=>{closed=true;for(const p of pending.values()){clearTimeout(p.timer);p.reject(Error('Browser connection closed'));}pending.clear();});
 send=(method,params={},target=session)=>new Promise((r,j)=>{const id=++sequence,timer=setTimeout(()=>{pending.delete(id);j(Error('CDP deadline: '+method));},20000);pending.set(id,{resolve:r,reject:j,timer});ws.send(JSON.stringify({id,method,params,...(target?{sessionId:target}:{})}));});
 proof.browser.version=await send('Browser.getVersion',{},null);
 const target=await send('Target.createTarget',{url:'about:blank'},null);session=(await send('Target.attachToTarget',{targetId:target.targetId,flatten:true},null)).sessionId;
 await send('Page.enable');await send('Runtime.enable');
 const evaluate=async expression=>{const r=await send('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(JSON.stringify(r.exceptionDetails));return r.result.value;};
 const navigate=async url=>{await send('Page.navigate',{url});await until(()=>evaluate('location.href === '+JSON.stringify(url)+' && document.readyState === "complete"'));};
 for(const viewport of [{width:1280,height:900,mobile:false},{width:390,height:844,mobile:true}]){
  await send('Emulation.setDeviceMetricsOverride',{...viewport,deviceScaleFactor:1});
  for(const page of ['normal','rejected','approval'])for(const scheme of page==='approval'?['light']:['light','dark']){
   await send('Emulation.setEmulatedMedia',{features:[{name:'prefers-color-scheme',value:scheme}]});await navigate(origin+'/blank');await evaluate('localStorage.clear()');
   const path=page==='approval'?'/ivy/oauth/authorize':'/ivy/login';const url=origin+path+'?page='+page+'#from-browser';await navigate(url);
   const observed=await evaluate(`(()=>{const form=document.querySelector('form'),credential=document.querySelector('input[type=password]'),button=form.querySelector('button');const bounds=el=>{const r=el.getBoundingClientRect();return{x:r.x,y:r.y,width:r.width,height:r.height};};return{width:innerWidth,scrollWidth:document.documentElement.scrollWidth,form:bounds(form),button:bounds(button),method:form.method,action:new URL(form.action).pathname,required:credential.required,maximum:credential.maxLength,autocomplete:credential.autocomplete,returnTo:form.elements.returnTo?.value??null,transaction:form.elements.transaction?.value??null,invalid:credential.getAttribute('aria-invalid'),error:document.querySelector('[role=alert]')?.textContent??null,theme:document.documentElement.dataset.theme??null,themeHidden:document.getElementById('theme-toggle')?.hidden??null,themeLabel:document.getElementById('theme-toggle')?.getAttribute('aria-label')??null,scripts:document.scripts.length,client:document.querySelector('strong')?.textContent??null};})()`);
   assert.equal(observed.width,viewport.width);assert.ok(observed.scrollWidth<=viewport.width+1);assert.ok(observed.form.x>=0&&observed.form.x+observed.form.width<=viewport.width+1);assert.ok(observed.button.height>0);assert.equal(observed.method,'post');assert.equal(observed.action,path);assert.equal(observed.required,true);assert.equal(observed.maximum,4096);assert.equal(observed.autocomplete,'current-password');
   if(page==='approval'){assert.equal(observed.scripts,0);assert.equal(observed.client,'Example <script>&"\'');assert.ok(observed.transaction);}
   else{assert.equal(observed.themeHidden,false);assert.equal(observed.theme,scheme);assert.equal(observed.invalid,page==='rejected'?'true':null);assert.equal(observed.error!==null,page==='rejected');assert.equal(observed.returnTo,page==='normal'?'/ivy/ui/item?x=1#from-browser':'/ivy/ui/item?x=1#target');await evaluate('document.getElementById("theme-toggle").click()');const changed=await evaluate('({theme:document.documentElement.dataset.theme,saved:localStorage.getItem("ivy.theme")})');assert.deepEqual(changed,{theme:scheme==='light'?'dark':'light',saved:scheme==='light'?'dark':'light'});observed.toggled=changed;}
   const before=posts.length;await evaluate('document.querySelector("input[type=password]").value="fixture-only";document.querySelector("form").requestSubmit()');await until(()=>posts.length>before);const post=posts.at(-1),parameters=new URLSearchParams(post.body);assert.equal(post.origin,origin);assert.equal(post.path,path);assert.equal(parameters.get(page==='approval'?'credential':'token'),'fixture-only');if(page==='approval')assert.equal(parameters.get('transaction'),observed.transaction);else assert.equal(parameters.get('returnTo'),observed.returnTo);
   proof.observations.push({viewport,scheme,page,observed,submission:{path:post.path,originMatches:true,fields:[...parameters.keys()]}});
  }
 }
 assert.deepEqual(exceptions,[]);proof.execution='completed';proof.finding={conditionMet:true,detail:'Ten desktop/mobile renderer observations passed, including escaping, form controls/submission, fragment retention, theme switching and no horizontal overflow.'};
}catch(error){if(proof.execution!=='blocked'){proof.execution=error instanceof assert.AssertionError?'completed':'failed';proof.finding=proof.execution==='completed'?{conditionMet:false,detail:String(error)}:null;proof.issue=proof.execution==='failed'?{code:'browser_check_failed',detail:String(error)}:null;}proof.error=String(error);process.exitCode=1;}
finally{
 if(send&&!closed){try{await send('Browser.close',{},null);}catch{}}
 if(ws&&!closed)ws.close();if(server)await new Promise(r=>server.close(r));
 if(child){await Promise.race([childClosed,delay(10000)]);if(child.exitCode===null){child.kill();await Promise.race([childClosed,delay(10000)]);}}
 try{if(profile.startsWith(allowed+sep)&&basename(profile).startsWith('auth-browser-')){let actual;try{actual=realpathSync(profile);}catch{}if(actual){if(!actual.startsWith(allowed+sep))throw Error('Browser profile escaped outputs');await rm(profile,{recursive:true,force:true,maxRetries:20,retryDelay:100});}}proof.profileRemoved=true;}catch(error){proof.cleanupIssue=String(error);proof.execution='failed';proof.finding=null;proof.issue={code:'browser_cleanup_failed',detail:String(error)};process.exitCode=1;}
 proof.finishedAt=new Date().toISOString();mkdirSync(dirname(output),{recursive:true});writeFileSync(output,JSON.stringify(proof,null,2)+'\n');console.log(JSON.stringify({execution:proof.execution,finding:proof.finding,issue:proof.issue,observations:proof.observations.length}));
}
