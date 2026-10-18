import {mkdtemp,mkdir,writeFile,readFile,rm,symlink,readdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {McpConfigurationManager} from '../../dist/services/agent-manager/src/environment.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest} from '../../dist/packages/contracts/src/canonical.js';
const defaults={schemaVersion:1,hostId:null,enabled:true,servers:[]};
const connection={async request(method){if(method==='objects.stat')throw new IvyError('not_found','missing');return {revision:0,configuration:{schemaVersion:1,appId:null}};}};
const root=await mkdtemp(join(tmpdir(),'ivy-mcp-files-')),rows=[];
try{
  for(const setup of ['missing','bom','invalid-utf8','config-directory','oversized-config','state-directory','invalid-state','bom-state','oversized-state','state-root-file','linked-state-root']){
    const home=join(root,setup);await mkdir(home);
    if(setup==='bom')await writeFile(join(home,'config.toml'),'\ufeffmodel="fixture"\r\n');
    if(setup==='invalid-utf8')await writeFile(join(home,'config.toml'),Buffer.from([0xc3,0x28]));
    if(setup==='config-directory')await mkdir(join(home,'config.toml'));
    if(setup==='oversized-config')await writeFile(join(home,'config.toml'),Buffer.alloc(4*1024*1024+1,32));
    if(['state-directory','invalid-state','bom-state','oversized-state'].includes(setup)){
      await mkdir(join(home,'.ivy-agent-mcp'));
      if(setup==='state-directory')await mkdir(join(home,'.ivy-agent-mcp/state.json'));
      else await writeFile(join(home,'.ivy-agent-mcp/state.json'),setup==='invalid-state'?'{':setup==='bom-state'?'\ufeffnull':Buffer.alloc(4*1024*1024+1,32));
    }
    if(setup==='state-root-file')await writeFile(join(home,'.ivy-agent-mcp'),'x');
    if(setup==='linked-state-root'){const dest=join(root,'link-destination');await mkdir(dest);await symlink(dest,join(home,'.ivy-agent-mcp'),process.platform==='win32'?'junction':'dir');}
    const manager=new McpConfigurationManager(home,'fixture','https://fixture.example','fixture-only',defaults);
    await manager.synchronize(connection);await manager.close();
    const fileHash=async path=>readFile(path).then(digest,e=>e.code);
    rows.push({setup,expected:{code:manager.status.code,applied:manager.status.state==='applied',config:await fileHash(join(home,'config.toml')),state:await fileHash(join(home,'.ivy-agent-mcp/state.json')),temporary:(await readdir(home)).filter(s=>s.startsWith('.ivy-environment-'))}});
  }
}finally{await rm(root,{recursive:true,force:true});}
process.stdout.write(JSON.stringify({defaults,rows}));
