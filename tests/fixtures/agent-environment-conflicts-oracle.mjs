import{registerHooks}from'node:module';
import{readFileSync}from'node:fs';
import{IvyError}from'../../dist/packages/contracts/src/errors.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return{format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {serverSections,hasAppSection,rootConfigurationKeys,adoptMatchingPermissions,hasLegacySandboxTable};\n',shortCircuit:true};
  return next(url,context);
}});
const original=await import('../../dist/services/agent-manager/src/environment.js');hook.deregister();
const rows=[];
function add(source){let adopted;try{adopted={result:original.adoptMatchingPermissions(source)};}catch(error){adopted={error:IvyError.from(error).toWire()};}rows.push({source,servers:[...original.serverSections(source)],keys:[...original.rootConfigurationKeys(source)],legacy:original.hasLegacySandboxTable(source),adopted});}
const sources=['','# comment\n','approval_policy = "never"\ndefault_permissions = ":danger-full-access"\n','"approval_policy" = \'never\'\r\n\'default_permissions\'=\":danger-full-access\"\r\n','approval_policy = "ask"','approval_policy = "never" # comment','approval_policy = "never"\napproval_policy = "never"','default_permissions = ":danger-full-access"\n\'default_permissions\' = \':danger-full-access\'','approval_policy = "never"\rdefault_permissions = ":danger-full-access"\r','approval_policy = "never"\u2028default_permissions = ":danger-full-access"','approval_policy\n= "never"','approval_policyx = "never"','APPROVAL_POLICY = "never"','[project]\napproval_policy = "ask"','\u00a0[project]\napproval_policy = "never"','sandbox_mode="danger-full-access"\n"sandbox_workspace_write"={}'];
for(const source of sources)add(source);
for(const key of ['approval_policy','default_permissions','sandbox_mode','sandbox_workspace_write'])for(const quote of ['',"'",'"'])for(const delimiter of [' = ', '\t=\t', '\n=\n'])add(`${quote}${key}${quote}${delimiter}${key==='approval_policy'?'"never"':key==='default_permissions'?'":danger-full-access"':'{}'}\n`);
for(const name of ['a','A','a_b','a-b','0a','a'.repeat(64),'a'.repeat(65),'ä','ſ','K','a.b',''])for(const quote of ['',"'",'"'])add(`\n \t[mcp_servers.${quote}${name}${quote}]\nx=1\n[MCP_SERVERS.${quote}${name}${quote}.extra]\n`);
for(const prefix of ['', ' ', '\t', '\n\r\n', '\u00a0', '\ufeff', '\u0085', '\u200b'])for(const end of ['.extra]',']','_wrong]'])add(prefix+'[sandbox_workspace_write'+end+'\n');
for(const unit of [10,13,0x2028,0x2029,0xa0,0x85,0xfeff])add(String.fromCodePoint(unit)+'approval_policy = "never"'+String.fromCodePoint(unit)+'[mcp_servers.fixture]'+String.fromCodePoint(unit)+'[sandbox_workspace_write]');
add('[mcp_servers.Fixture]\n[mcp_servers.fixture]\n[mcp_servers.Fixture]\n');
const apps=[];
for(const appId of ['app','app.with+regex[chars]','a"b',"a'b",'😀','', 'app\nline'])for(const quote of ['',"'",'"'])for(const suffix of [']','.tools]','_other]']){
  const source='\n \t[apps.'+quote+appId+quote+suffix+'\n[APPS.app]\n';
  apps.push({source,appId,expected:original.hasAppSection(source,appId)});
}
for(const source of ['[APPS.app]','\u0085[apps.app]','\u00a0[apps.app]','[apps.application]','[apps. app]'])apps.push({source,appId:'app',expected:original.hasAppSection(source,'app')});
process.stdout.write(JSON.stringify({rows,apps}));
