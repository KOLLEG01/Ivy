import {writeFileSync,existsSync} from 'node:fs';
const [mode,...args]=process.argv.slice(2);
switch(mode){
 case 'empty':break;
 case 'text':process.stdout.write('\ufeffOriginal é😀\n');process.stderr.write(' diagnostics ');break;
 case 'args':process.stdout.write(JSON.stringify(args));break;
 case 'cwd':process.stdout.write(process.cwd());break;
 case 'exe':process.stdout.write(process.execPath);break;
 case 'env':process.stdout.write(JSON.stringify(process.env.RUST_TEST_THREADS??null));break;
 case 'exit':process.stdout.write(' output ');process.stderr.write(' error ');process.exitCode=7;break;
 case 'whitespace':process.stdout.write(' \t\n');process.stderr.write('\ufeff');process.exitCode=7;break;
 case 'stdout':process.stdout.write('x'.repeat(70000));break;
 case 'stderr':process.stderr.write('e'.repeat(70000));break;
 case 'combined':process.stdout.write('o'.repeat(32768));process.stderr.write('e'.repeat(32768));break;
 case 'sleep':writeFileSync(args[0],String(process.pid));process.stdout.write('ready');setInterval(()=>{},1000);break;
 case 'gate':{
  writeFileSync(args[0],String(process.pid));
  const timer=setInterval(()=>{if(existsSync(args[1])){clearInterval(timer);process.stdout.write('owned output');process.exitCode=Number(args[2]);}},10);
  break;
 }
 default:throw Error('Unknown fixture mode');
}
