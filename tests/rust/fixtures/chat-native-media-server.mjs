// Local TLS fixture. The committed key is public test data, never a credential.
import {createServer} from 'node:https';
import {readFileSync} from 'node:fs';
import {createInterface} from 'node:readline';
import {fileURLToPath} from 'node:url';
const fixture=name=>readFileSync(fileURLToPath(new URL(name,import.meta.url)));
const requests=[];
const server=createServer({key:fixture('./chat-media-test-key.pem'),cert:fixture('./chat-media-test-cert.pem')},async(req,res)=>{
 const chunks=[];for await(const chunk of req)chunks.push(chunk);
 const body=Buffer.concat(chunks);
 requests.push({method:req.method,path:req.url,authorization:req.headers.authorization,contentType:req.headers['content-type'],body:body.toString('base64')});
 if(req.url==='/redirect'){res.writeHead(307,{location:'/sink'});res.end();return;}
 if(req.url==='/slow'){await new Promise(r=>setTimeout(r,500));res.end('{"text":"late"}');return;}
 if(req.url==='/slow-body'){res.writeHead(200,{'content-type':'application/json'});res.flushHeaders();await new Promise(r=>setTimeout(r,500));res.end('{"text":"late"}');return;}
 if(req.url==='/oversized'){res.end(' '.repeat(1024*1024+1));return;}
 if(req.url==='/invalid-json'){res.end('not JSON');return;}
 if(req.url==='/unavailable'){res.writeHead(503);res.end('fixture-private-error');return;}
 res.setHeader('content-type','application/json');res.end('{"text":"Local transcript"}');
});
server.listen(0,'127.0.0.1',()=>process.stdout.write(JSON.stringify({port:server.address().port})+'\n'));
createInterface({input:process.stdin}).on('line',line=>{
 if(line==='stats')process.stdout.write(JSON.stringify(requests)+'\n');
 if(line==='close'){server.closeAllConnections();server.close(()=>process.exit(0));}
});
