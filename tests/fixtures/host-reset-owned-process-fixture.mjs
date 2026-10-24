import {readFileSync,writeFileSync,mkdirSync} from 'node:fs';
import {DatabaseSync} from 'node:sqlite';
import path from 'node:path';
const [rootInput,nonce,mode,ownerRoot,controlPath]=process.argv.slice(2),root=path.resolve(rootInput);
if(!path.basename(path.dirname(root)).startsWith('ivy-config-'))throw Error('Disposable process root required');
let lock;
if(ownerRoot){
 mkdirSync(ownerRoot,{recursive:true});
 lock=new DatabaseSync(path.join(ownerRoot,'executor-lock.sqlite'));
 lock.exec('PRAGMA busy_timeout=100; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS owner(pid INTEGER); DELETE FROM owner;');
 lock.prepare('INSERT INTO owner VALUES (?)').run(process.pid);
 lock.exec('COMMIT');
}
writeFileSync(path.join(root,'identity.json'),JSON.stringify({pid:process.pid,nonce,mode}));
if(mode==='exit259')setTimeout(()=>{lock?.close();process.exit(259);},350);
if(mode==='cooperative')setTimeout(()=>{lock?.close();process.exit(86);},15_000).unref();
const timer=setInterval(()=>{
 if(mode!=='cooperative')return;
 try{const c=JSON.parse(readFileSync(controlPath,'utf8'));if(c.instanceId==='fixture'&&c.bootId==='fixture-boot'&&c.launchId==='fixture-launch'&&c.action==='shutdown'){lock?.close();clearInterval(timer);process.exit(0);}}catch{}
},25);
