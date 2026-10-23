import fs from 'node:fs';
const [mode,ready,finish,...args]=process.argv.slice(2);
const ascii=value=>JSON.stringify(value).replace(/[\u007f-\uffff]/g,ch=>'\\u'+ch.charCodeAt(0).toString(16).padStart(4,'0'));
fs.writeFileSync(ready,ascii({pid:process.pid,mode}));
const receipt=ascii({args,cwd:process.cwd(),marker:process.env.IVY_PHONE_CLIENT_MARKER??null});
switch(mode){
 case 'ok':process.stdout.write(receipt);break;
 case 'exit7':process.stdout.write(receipt);process.exitCode=7;break;
 case 'stderr-bound':process.stderr.write('e'.repeat(65536));break;
 case 'stderr-over':process.stderr.write('e'.repeat(65537));break;
 case 'stdout-over':process.stdout.write('x'.repeat(65537));break;
 case 'over-hang':process.stdout.write('x'.repeat(65537));setInterval(()=>{},1000);break;
 case 'hang':setInterval(()=>{},1000);break;
 case 'release':{const timer=setInterval(()=>{if(fs.existsSync(finish)){clearInterval(timer);process.stdout.write(receipt);}},10);break;}
 default:throw Error('Unknown owned client fixture mode');
}
