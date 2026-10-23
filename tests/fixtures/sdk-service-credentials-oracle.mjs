import {registerHooks,createRequire} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createServer} from 'node:net';
import {once} from 'node:events';
import {validateHeaderValue} from 'node:http';
const require=createRequire(import.meta.url);
const external=new Map(['ajv/dist/2020.js','re2js'].map(name=>[name,pathToFileURL(require.resolve(name)).href]));
external.set('ws',new URL('./wrapper.mjs',pathToFileURL(require.resolve('ws'))).href);
registerHooks({resolve(specifier, context, next) {
  if(external.has(specifier))return next(external.get(specifier),context);
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {ServiceClient, ServiceConnection} = await import('../../packages/sdk/src/service.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const identity = {serviceNodeId:'fixture-node', serviceName:'fixture', hostId:'fixture-host', version:'1.0.0', buildId:'sha256:'+'a'.repeat(64), hiveProtocol:1};
const mode = process.argv[2];
let result;
if (mode === 'admission') {
  const accepted = [], rejections = new Map();
  for (let unit = 0; unit <= 65535; unit++) {
    try { validateHeaderValue('Authorization', 'Bearer '+String.fromCharCode(unit)); accepted.push(unit); }
    catch (error) { rejections.set(error.code, {name:error.name, code:error.code, message:error.message}); }
  }
  result = {accepted, rejections:[...rejections.values()]};
} else if (mode === 'cases') {
  result = [
    ['', 'empty'], ['fixture-token', 'ascii'], ['éÿ\u0080\u009f', 'latin1'],
    [String.fromCharCode(9,...Array.from({length:95},(_,i)=>32+i),...Array.from({length:128},(_,i)=>128+i)), 'all-admitted-units'],
    ['\tinside\t', 'tabs'], [' token \t', 'spaces'], ['a:b', 'colon'],
    ['Ā', 'bmp-256'], ['€', 'bmp-euro'], ['💡', 'astral'],
    ['\ud800', 'high-unit'], ['\udfff', 'low-unit'], ['a\ud800b', 'embedded-unit'],
    ['x\r\ny', 'crlf'], ['x\r', 'cr'], ['x\ny', 'lf'], ['x\x7fy', 'del'],
    ...Array.from({length:32},(_,i)=>[String.fromCharCode(i),'control-'+i]),
  ].map(([credential,name])=>({credential,name}));
  for (const outcome of ['not_executed','unknown','completed']) result.push({name:'callback-'+outcome, failure:outcome});
} else if (mode === 'run') {
  const rows = [], rawHeaderErrors = [];
  for (const row of JSON.parse(readFileSync(0, 'utf8'))) {
    const headers = [], sockets = new Set();
    const server = createServer(socket => {
      sockets.add(socket);socket.once('close',()=>sockets.delete(socket));
      let bytes = Buffer.alloc(0), done = false;
      socket.on('data', chunk => {
        if (done) return;bytes = Buffer.concat([bytes,chunk]);
        if (!bytes.includes('\r\n\r\n')) return;done = true;
        const line = bytes.toString('latin1').split('\r\n').find(line=>line.toLowerCase().startsWith('authorization:'));
        headers.push(Buffer.from(line.slice(line.indexOf(':')+2),'latin1').toString('hex'));
        socket.end('HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n');
      });
    });
    server.listen(0,'127.0.0.1');await once(server,'listening');
    const url = `http://127.0.0.1:${server.address().port}/ivy/`;
    let calls = 0, finish;
    const states = [], offline = new Promise(resolve=>{finish=resolve;});
    const client = new ServiceClient({publicBaseUrl:url,identity,handlers:{},registry:()=>({}),reconcile:async()=>{},
      reconnectMinMs:60000,reconnectMaxMs:60000,
      credential:()=>{calls++;if(row.failure)throw new IvyError('credential_failed','failure-\ud800'+'x'.repeat(270),row.failure);return row.credential;},
      onState:state=>{states.push(state);if(state.status==='offline')finish();}});
    const afterConstructor = calls;
    client.start();
    let timer;
    try { await Promise.race([offline,new Promise((_,reject)=>{timer=setTimeout(()=>reject(Error('No bounded offline state: '+row.name)),3000);})]); }
    finally { clearTimeout(timer);await client.stop(); }
    let direct = null;
    if (!row.failure) {
      let connection;
      try { connection = new ServiceConnection(new URL('ws',url.replace('http:','ws:')),row.credential,client);await connection.opened;direct={ok:true}; }
      catch (error) {
        direct = {ok:false,error:IvyError.from(error).toWire()};
        if (!(error instanceof IvyError)) rawHeaderErrors.push({name:row.name,type:error.name,code:error.code,message:error.message});
      } finally { connection?.close(); }
    }
    for (const socket of sockets) socket.destroy();
    server.close();await once(server,'close');
    rows.push({name:row.name,afterConstructor,calls,states,headers,direct});
  }
  result = {rows,rawHeaderErrors};
} else throw Error('Unknown credential oracle mode');
process.stdout.write(JSON.stringify(result));
