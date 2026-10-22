import assert from 'node:assert/strict';
import {mkdtemp,writeFile,unlink,rmdir} from 'node:fs/promises';
import {join} from 'node:path';import {tmpdir} from 'node:os';
import {load,settings as base} from './inbox-fixtures.mjs';
const projection=await load('services/secretary/src/outlook-projection.js'),media=await load('services/secretary/src/outlook-media.js');
const {microsoftConfiguration,loadMicrosoftConfiguration}=await load('services/secretary/src/microsoft-config.js');
const {teamsConfiguration,loadTeamsConfiguration}=await load('services/secretary/src/teams-config.js');
const {IvyError,hashJson}=await load('packages/sdk/src/node.js');
const clone=structuredClone,profile={id:'original-account',email:'me@example.test'},source={sourceId:'mail',kind:'email',accountId:profile.id,producerPrincipalIds:[base.identity.principalId],allowedSenderIds:null,ownSenderIds:[profile.email]};
const settings={...base,sources:[source]},selected={since:'2026-09-01T00:00:00.000Z',until:'2026-09-08T04:00:00.000Z',skip:0,pageSize:8},observedAt='2026-09-08T04:00:01.000Z';
const collector={sourceId:'mail',target:{serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:'fixture',nativeExecutableHash:hashJson('executable'),catalogHash:hashJson('catalog')},profile,since:selected.since,pageSize:8,pollMs:60000,maximumPages:100,threadCwd:process.platform==='win32'?'C:\\fixture\\collector':'/fixture/collector',expectedAccountHash:hashJson('account')};
const config={schemaVersion:1,collectors:[collector]},teams={schemaVersion:1,collectors:[]};
const message=(changes={})=>({id:'native-message',sender:{emailAddress:{address:'Sender@Example.test'}},subject:'Original title',receivedDateTime:'2026-09-07T23:00:00Z',isRead:false,body:{contentType:'html',content:'<p>Original <b>full</b> body.</p><p>Second paragraph.</p>'},bodyPreview:'Never a complete original body',has_attachments:false,web_link:'https://outlook.office365.com/mail/id/original',...changes});
const page=(entries=[message()])=>({value:entries,has_more:false,next_from_index:null,next_link:null});
const rawAttachment={id:'attachment',attachment_type:'fileAttachment',odata_type:'#microsoft.graph.fileAttachment',name:'inert.html',content_type:'text/html',size_bytes:253,is_inline:false,content_id:null,last_modified_date_time:'2026-09-08T00:00:00Z',payload_fetch_supported:true,payload_status:'not_requested',payload_error:null,file_uri:null};
const metadata=(changes={})=>({message_id:'message',attachments:[{...rawAttachment,...changes}],next_link:null,errors:[]});
const attachment=media.outlookAttachments(metadata(),'message')[0];
const materialization={message_id:'message',attachment_id:'attachment',attachment_type:'fileAttachment',filename:'inert.html',size_bytes:253,mime_type:'text/html',is_inline:false,content_id:null,payload_status:'materialized',file_uri:{file_id:'original-file',file_name:'inert.html',mime_type:'text/html',download_url:'https://fixture.oaiusercontent.com/files/original?sig=private-capability'}};
const rows=[];
const error=e=>{const x=IvyError.from(e);return{error:{code:x.code,message:x.message,outcome:x.outcome,...(x.details===undefined?{}:{details:x.details})}};};
async function add(label,action,value,options={}){
 const input={label,action,value:clone(value),...clone(options)};
 try{let output;const selectedInput=(key,fallback)=>Object.hasOwn(input,key)?input[key]:fallback;
  if(action==='microsoftConfig')output=microsoftConfiguration(value,selectedInput('settings',settings));
  else if(action==='teamsConfig')output=teamsConfiguration(value,settings);
  else if(action==='page')output=projection.outlookPage(value,selectedInput('profile',profile),selectedInput('source',source),selectedInput('selected',selected),selectedInput('observedAt',observedAt));
  else if(action==='metadata')output=media.outlookAttachments(value,selectedInput('messageId','message'));
  else if(action==='materialization')output=media.outlookMaterialization(value,selectedInput('attachment',attachment));
  else output=projection[action](value);
  rows.push({input,expected:{value:output}});
 }catch(e){rows.push({input,expected:error(e)});}
}
for(const value of [profile,{...profile,displayName:'ignored'},null,[],{},0,{id:'',email:profile.email},{id:profile.id,email:'not-an-address'},{id:profile.id,email:'a@b@c'},{id:profile.id,email:'<a>@b'},{id:profile.id,email:' a@b'},{id:profile.id,email:'a@b '},{id:'\ufeff',email:profile.email},{id:'😀',email:'ΣΟΣ@EXAMPLE.TEST'},{id:'unpaired\ud800',email:profile.email},{id:'a'.repeat(257),email:profile.email},{id:'original',email:'a\u0085@b'}])await add('profile '+rows.length,'microsoftProfile',value);
for(const value of [{content:[],structuredContent:{value:[]},isError:false},{content:[],structuredContent:{}},{content:[],structuredContent:{},isError:null},{content:[],structuredContent:null},{structuredContent:{}},{content:'wrong',structuredContent:{}},{content:[],structuredContent:{},isError:0},{content:[],isError:true,structuredContent:{error_code:'FORBIDDEN'}},null,[]])await add('MCP '+rows.length,'microsoftMcp',value);
for(const seconds of [1,62,86400,0,-1,1.5,86401,'62',null])await add('MCP retry '+seconds,'microsoftMcp',{content:[],isError:true,structuredContent:{error_code:'RATE_LIMITED',retry_after_seconds:seconds}});
await add('original selection','outlookArguments',selected);
for(const change of [{until:selected.since},{since:'2026-02-29T00:00:00.000Z'},{since:'2026-09-01T00:00:00.0000000Z'},{skip:-1},{skip:1.5},{pageSize:33},{pageSize:0},{until:'0000-01-01T00:00:00.000Z'}])await add('selection '+JSON.stringify(change),'outlookArguments',{...selected,...change});
for(const [kind,content]of [['text',''],['text',' literal \t\n\r preserved '],['text','😀'.repeat(16384)],['text','😀'.repeat(16385)],['text','\ud800'],['text','\0'],['text','\u007f'],['text','\u0085'],['unknown','body'],['text','x'.repeat(32769)]])await add('body '+kind+' '+rows.length,'outlookBody',{contentType:kind,content});
const htmlCases=[
 '<html><head><style>SECRET</style></head><body><p>One &amp; two</p><div>Next<br>line</div><table><tr><td>A</td><td>B</td></tr></table><p><a href="https://example.test/path">Source</a></p><img src="cid:image" alt="Invoice chart"><script>SECRET</script><template>SECRET</template><a href="javascript:alert(1)">Visible label</a></body></html>',
 '<table>outside<tr><td>first<td><p>second</table>tail','<b>one<i>two</b>three</i>','<!-- ignored --><!DOCTYPE html><p>&#0; &Tab; &#x1f600; &nbsp; &copy;</p>',
 '<p hidden>Visible</p><iframe>Also visible source</iframe><object data="cid:object">Object text</object>','<noscript><img src="cid:inert">hidden parser text</noscript><p>Visible</p>',
 '<svg xmlns="http://www.w3.org/2000/svg"><image xlink:href="cid:x" xmlns:xlink="http://www.w3.org/1999/xlink"/><foreignObject><img src="cid:html" alt="alt"></foreignObject></svg>',
 '<math><annotation-xml encoding="text/html"><img src="cid:math"></annotation-xml></math>','<video src="https://never-fetch.invalid/v"><source src="cid:source"></video>',
 '<img a="1" src="cid:first" a="2"><img src="cid:second" a="1">','<IMG SRC="cid:upper" ALT="same"><image src="cid:alias">',
 '<head><img src="cid:after-head"></head><script><img src="cid:script"></script><style>hidden</style><template><img src="cid:template"></template>',
 '<p>A</p> \n\n\n <p>B</p>','<img alt=" [a] &#10; b "><a href="mailto:Person@Example.test">Mail</a>',
 '<p>'+'x'.repeat(32769)+'</p>','<!--'+'x'.repeat(1048576-7)+'-->','x'.repeat(1048577),
 '<i></i>'.repeat(100001),'<div>'.repeat(1000)+'end'+'</div>'.repeat(1000),'<img src="cid:x">'.repeat(1024),'<img src="cid:x">'.repeat(1025)
];
for(const content of htmlCases){await add('HTML body '+rows.length,'outlookBody',{contentType:'html',content});await add('HTML references '+rows.length,'outlookInlineReferences',{contentType:'html',content});}
for(const href of ['https://EXAMPLE.test:443/a/../b?q=ü','http://example.test','mailto:a@example.test','ftp://example.test','javascript:alert(1)','/relative','https://user:pass@example.test/a','https://@example.test/a','https://example.test/a#',' https://example.test/a ','http:\\example.test\\x','https://example.test/'+'x'.repeat(4096)])await add('HTML link '+href.slice(0,70),'outlookBody',{contentType:'html',content:'<a href="'+href+'">label</a>'});
await add('media flag short circuit','outlookHasMedia',{has_attachments:true,body:null});await add('missing body requires evidence','outlookHasMedia',{has_attachments:false});await add('non-HTML reference body bypass','outlookInlineReferences',{contentType:'text',content:null});
await add('complete page','page',page());await add('complete empty page','page',page([]));
await add('filter before body identity and time','page',page([message({sender:{emailAddress:{address:'ME@example.test'}},body:47,id:null,receivedDateTime:'bad'}),message({sender:{emailAddress:{address:'unknown@example.test'}},body:47,id:null}),message()]),{source:{...source,ownSenderIds:[],allowedSenderIds:['sender@example.test']}});
for(const change of [{accountId:'different'},{kind:'teams'},{ownSenderIds:['ME@example.test']},{allowedSenderIds:['SENDER@example.test']},{ownSenderIds:['invalid']},{producerPrincipalIds:[]}])await add('page source '+JSON.stringify(change),'page',page(),{source:{...source,...change}});
await add('profile schema before account','page',page(),{profile:{...profile,email:null}});await add('profile raw case is retained','page',page(),{profile:{...profile,email:'ME@EXAMPLE.TEST'}});
for(const observedAt of [selected.since,selected.until,'2026-02-30T00:00:00.000Z','2026-09-08T04:00:01Z',null])await add('page observed '+observedAt,'page',page(),{observedAt});
const continued={...page(),has_more:true,next_from_index:13,next_link:'https://graph.microsoft.com/v1.0/me/messages?$skip=13'};
await add('complete continuation','page',continued);
for(const change of [{next_from_index:0},{next_from_index:1.5},{next_from_index:1000001},{next_from_index:null},{next_link:null},{next_link:'https://user:pass@graph.microsoft.com/a'},{next_link:'https://graph.microsoft.com.evil.test/a'},{next_link:'https://GRAPH.MICROSOFT.COM:443/a'},{value:[]},{has_more:false},{has_more:'true'}])await add('continuation '+JSON.stringify(change),'page',{...continued,...change});
await add('absent page completion','page',{value:[]});await add('duplicate original ID','page',page([message(),message()]));await add('page count bound','page',page([message(),message({id:'other'})]),{selected:{...selected,pageSize:1}});
for(const change of [{receivedDateTime:selected.until},{receivedDateTime:'2026-08-31T23:59:59Z'},{receivedDateTime:'2026-02-30T00:00:00Z'},{receivedDateTime:'2026-09-07T23:00:00.1230000Z'},{receivedDateTime:'2026-09-07T23:00:00.1230001Z'},{receivedDateTime:'2026-09-07T23:00:00+00:00'},{isRead:true},{has_attachments:true},{has_attachments:'true'},{body:{contentType:'html',content:'Same<img src="cid:first">'}},{body:{contentType:'html',content:'Same<img src="cid:second">'}},{body:null},{subject:'x'.repeat(1025)},{id:'\ud800'},{id:'😀'},{sender:{emailAddress:{address:'ΣΟΣ@EXAMPLE.TEST'}}},{web_link:'https://EXAMPLE.test:443/a'},{web_link:'https://example.test/a#'},{web_link:'javascript:alert(1)'}])await add('page message '+rows.length,'page',page([message(change)]));
await add('decreasing original order only','page',page([message({receivedDateTime:'2026-09-07T22:00:00Z'}),message({id:'later'})]));
await add('complete page output bound','page',page(Array.from({length:8},(_,i)=>message({id:String(i),body:{contentType:'text',content:'x'.repeat(32768)}}))));
await add('complete metadata','metadata',metadata());await add('empty metadata','metadata',{...metadata(),attachments:[]});
for(const change of [{message_id:'other'},{next_link:'https://graph.microsoft.com/next'},{errors:['failed']},{attachments:[rawAttachment,rawAttachment]},{attachments:Array(33).fill(rawAttachment)},{attachments:null}])await add('metadata page '+rows.length,'metadata',{...metadata(),...change});
for(const change of [{file_uri:materialization.file_uri},{payload_error:'failed'},{size_bytes:-1},{size_bytes:1.5},{size_bytes:2147483647},{size_bytes:2147483648},{is_inline:null},{name:'bad\0name'},{name:'😀'},{name:'\ud800'},{content_type:null},{last_modified_date_time:'not a date'},{last_modified_date_time:'2026-02-30T00:00:00Z'},{last_modified_date_time:'2026-09-08T00:00:00.1234567Z'},{attachment_type:'referenceAttachment',odata_type:'#microsoft.graph.referenceAttachment',payload_fetch_supported:false},{attachment_type:'unknown',odata_type:'opaque'},{attachment_type:['fileAttachment']},{attachment_type:'other'},{content_id:'\u007f'},{payload_status:'materialized'}])await add('attachment '+rows.length,'metadata',metadata(change));
for(const messageId of ['',null,'x'.repeat(4097),'\ud800'])await add('attachment message ID '+rows.length,'metadata',metadata(),{messageId});
const missing=clone(metadata());delete missing.attachments[0].content_type;await add('missing is distinct from null','metadata',missing);
await add('complete materialization','materialization',materialization);
for(const change of [{message_id:'other'},{attachment_id:'other'},{filename:'other'},{size_bytes:1},{mime_type:'image/png'},{content_id:'different'},{is_inline:true},{payload_status:'failed'},{file_uri:null},{file_uri:{...materialization.file_uri,file_name:'other'}}])await add('materialization '+rows.length,'materialization',{...materialization,...change});
for(const url of ['http://fixture.oaiusercontent.com/a','https://oaiusercontent.com/a','https://oaiusercontent.com.evil.test/a','https://a@fixture.oaiusercontent.com/a','https://fixture.oaiusercontent.com:444/a','https://fixture.oaiusercontent.com/a#fragment','file:///tmp/media','not a URL','https://FIXTURE.oaiusercontent.com:443/a#','https://fixture.oaiusercontent.com/a?sig=unchanged',' https://fixture.oaiusercontent.com/a ','https://fixture.oaiusercontent.com./a'])await add('materialization URL '+url,'materialization',{...materialization,file_uri:{...materialization.file_uri,download_url:url}});
for(const change of [{attachmentType:'referenceAttachment'},{payloadFetchSupported:false},{modifiedAt:'2026-02-30T00:00:00Z'}])await add('materialization attachment '+rows.length,'materialization',materialization,{attachment:{...attachment,...change}});
await add('valid Microsoft config','microsoftConfig',config);await add('empty Microsoft config','microsoftConfig',{schemaVersion:1,collectors:[]});await add('duplicate source IDs','microsoftConfig',{...config,collectors:[collector,collector]});
for(const change of [{since:'2026-02-30T00:00:00.000Z'},{since:'invalid'},{threadCwd:'relative'},{threadCwd:'/'},{threadCwd:'\\root-relative'},{threadCwd:'C:drive-relative'},{threadCwd:'C:\\absolute'},{threadCwd:'\\\\server\\share\\directory'},{pageSize:33},{pollMs:59999},{maximumPages:0},{profile:{...profile,id:'another'}},{profile:{...profile,email:'not-email'}},{since:'0000-01-01T00:00:00.000Z'}])await add('Microsoft config collector '+rows.length,'microsoftConfig',{...config,collectors:[{...collector,...change}]});
for(const change of [{kind:'teams'},{accountId:'other'},{producerPrincipalIds:['different']},{sourceId:'other'}])await add('Microsoft config source '+JSON.stringify(change),'microsoftConfig',config,{settings:{...settings,sources:[{...source,...change}]}});
await add('disabled Teams empty config','teamsConfig',teams);
const teamsCollector={...collector,maximumChats:8,maximumMessages:8,excludedChatIds:[]};delete teamsCollector.pageSize;delete teamsCollector.maximumPages;
await add('Teams nonempty remains gated','teamsConfig',{schemaVersion:1,collectors:[teamsCollector]});await add('Teams validates before disabled gate','teamsConfig',{schemaVersion:1,collectors:[{}]});
const fileRows=[],directory=await mkdtemp(join(tmpdir(),'secretary-outlook-projection-')),file=join(directory,'config.json');
try{
 for(const value of [undefined,{schemaVersion:1},{schemaVersion:1,microsoft:config,teams},{schemaVersion:1,microsoft:null,teams:null},{schemaVersion:1,microsoft:{schemaVersion:1,collectors:[]},teams:{schemaVersion:1,collectors:[teamsCollector]}},null,{},[]]){
  if(value===undefined)await unlink(file).catch(e=>{if(e.code!=='ENOENT')throw e;});else await writeFile(file,JSON.stringify(value),{mode:0o600});
  for(const action of ['microsoftFile','teamsFile']){const input={action,...(value===undefined?{missing:true}:{value})};try{fileRows.push({input,expected:{value:await(action==='microsoftFile'?loadMicrosoftConfiguration:loadTeamsConfiguration)(file,settings)}});}catch(e){fileRows.push({input,expected:error(e)});}}
 }
 for(const rawKind of ['syntax','bom','oversized']){
  await writeFile(file,rawKind==='syntax'?'{':rawKind==='bom'?'\ufeff{"schemaVersion":1}':' '.repeat(1048577),{mode:0o600});
  for(const action of ['microsoftFile','teamsFile']){const input={action,rawKind};try{fileRows.push({input,expected:{value:await(action==='microsoftFile'?loadMicrosoftConfiguration:loadTeamsConfiguration)(file,settings)}});}catch(e){fileRows.push({input,expected:error(e)});}}
 }
}finally{await unlink(file).catch(e=>{if(e.code!=='ENOENT')throw e;});await rmdir(directory);}
for(const label of ['complete page','complete empty page','filter before body identity and time','complete continuation','complete metadata','complete materialization','valid Microsoft config','disabled Teams empty config'])assert('value'in rows.find(r=>r.input.label===label).expected,label);
console.log(JSON.stringify({platform:process.platform,settings,profile,source,selected,observedAt,attachment,rows,fileRows}));
