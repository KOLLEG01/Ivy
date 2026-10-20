// Child process: the original reads these TLS/proxy inputs at process startup.
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import tls from 'node:tls';
const require=createRequire(process.env.IVY_HIVE_TEST_WEB_PUSH);
const webPush=require(process.env.IVY_HIVE_TEST_WEB_PUSH);
const input=JSON.parse(readFileSync(0,'utf8'));
let status;
try{status=(await webPush.sendNotification(input.subscription,'notification',{TTL:3600,timeout:10000,vapidDetails:input.details})).statusCode;}
catch(error){status=error.statusCode??0;}
console.log(JSON.stringify({status,roots:Object.fromEntries(['bundled','system','extra','default'].map(kind=>[kind,tls.getCACertificates(kind).length]))}));
