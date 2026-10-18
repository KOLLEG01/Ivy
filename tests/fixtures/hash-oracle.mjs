import {createHmac, timingSafeEqual} from 'node:crypto';
const rows=[];
function add(key, message){
  const signature=createHmac('sha256',key).update(message).digest();
  const bytes=value=>[...value];
  const supplied=[signature,signature.subarray(0,31),Buffer.concat([signature,Buffer.of(0)]),Buffer.alloc(0)];
  for(let i=0;i<signature.length;i++){
    const changed=Buffer.from(signature);changed[i]^=1;supplied.push(changed);
  }
  rows.push({key:bytes(key),message:bytes(message),signature:bytes(signature),verification:supplied.map(value=>({signature:bytes(value),valid:value.length===signature.length&&timingSafeEqual(signature,value)}))});
}
const patterned=(size,seed)=>Buffer.from(Array.from({length:size},(_,i)=>(i*73+seed)&255));
for(const k of [0,1,16,31,32,63,64,65,127,128,129,256])
  for(const n of [0,1,55,56,63,64,65,127,128,255,1024,4096])add(patterned(k,17),patterned(n,131));
add(Buffer.alloc(20,0x0b),Buffer.from('Hi There'));
add(Buffer.from('Jefe'),Buffer.from('what do ya want for nothing?'));
// Cursor-shaped bodies use exactly the string key/update semantics from Store.cursor.
// In particular a persisted 64-character hex-looking secret is not decoded to 32 bytes.
for(const secret of ['0123456789abcdef'.repeat(4),'','é\ud800😀']){
  const body=Buffer.from(JSON.stringify({query:'sha256:'+'a'.repeat(64),boundary:{id:'object',value:'é😀'}})).toString('base64url');
  const key=Buffer.from(secret,'utf8'),message=Buffer.from(body,'utf8');
  const expected=createHmac('sha256',secret).update(body).digest();
  if(!expected.equals(createHmac('sha256',key).update(message).digest()))throw Error('String/byte boundary mismatch');
  add(key,message);
}
process.stdout.write(JSON.stringify(rows));
