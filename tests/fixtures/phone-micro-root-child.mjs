import fs from 'node:fs';
import {spawn} from 'node:child_process';
const [mode,ready,release,...args]=process.argv.slice(2);
const ascii=value=>JSON.stringify(value).replace(/[\u007f-\uffff]/g,c=>'\\u'+c.charCodeAt(0).toString(16).padStart(4,'0'));
const signal=name=>{const file=ready+name;fs.writeFileSync(file+'.tmp',ascii({pid:process.pid,mode}));fs.renameSync(file+'.tmp',file);};
function linger(){const until=Date.now()+30000;const timer=setInterval(()=>{if(fs.existsSync(release)||Date.now()>until)clearInterval(timer);},10);}
if(mode==='leaf'){signal('.leaf');linger();}
else if(mode==='branch'){
 const child=spawn(process.execPath,[import.meta.filename,'leaf',ready,release],{stdio:'ignore',windowsHide:true,detached:true});
 child.unref();signal('.branch');linger();
}else{
 fs.appendFileSync(ready+'.starts',process.pid+'\n');
 const receipt=ascii({args,cwd:process.cwd(),inherited:!!process.env.IVY_PHONE_MICRO_CLIENT_NODE});
 const launchTree=()=>{
  const child=spawn(process.execPath,[import.meta.filename,mode==='live-tree'?'branch':'leaf',ready,release],{stdio:mode==='held-desc'?['ignore',process.stdout,process.stderr]:'ignore',windowsHide:true,detached:true});
  child.unref();
  const leaf=ready+(mode==='live-tree'?'.branch':'.leaf');
  const timer=setInterval(()=>{
   if(!fs.existsSync(leaf)||(mode==='live-tree'&&!fs.existsSync(ready+'.leaf')))return;
   clearInterval(timer);signal('');
   if(mode==='live-tree')linger();else {
    // Let the test pin the root/descendant identities before natural exit.
    const exiting=setInterval(()=>{if(fs.existsSync(ready+'.exit')){clearInterval(exiting);if(mode!=='closed-desc')process.stdout.write(receipt);process.exitCode=7;}},5);
   }
  },5);
 };
 if(mode==='closed-desc'){
  // Close the actual original pipe handles before spawn, so the descendant
  // cannot retain unrelated inheritable handles even with stdio:'ignore'.
  process.stdout.write(receipt,()=>{fs.closeSync(1);fs.closeSync(2);launchTree();});
 }else if(mode==='held-desc'||mode==='live-tree')launchTree();
 else{
  signal('');
  switch(mode){
   case 'ok':process.stdout.write(receipt);break;
   case 'exit7':process.stdout.write(receipt);process.exitCode=7;break;
   case 'raw':process.stdout.write(Buffer.from([0x72,0x61,0x77,0x40,0]));break;
   case 'utf8':process.stdout.write(Buffer.from([0xef,0xbb,0xbf,0xc3,0xa9,0xf0,0x90,0x90,0xb7]));break;
   case 'utf16':process.stdout.write(Buffer.from([0xff,0xfe,0xe9,0,1,0xd8,0x37,0xdc]));break;
   case 'stderr-bound':process.stderr.write('e'.repeat(65536));break;
   case 'stderr-over':process.stderr.write('e'.repeat(65537));break;
   case 'stdout-over':process.stdout.write('x'.repeat(65537));break;
   case 'release':{const timer=setInterval(()=>{if(fs.existsSync(release)){clearInterval(timer);process.stdout.write(receipt);}},10);break;}
   default:throw Error('Unknown owned root fixture');
  }
 }
}
