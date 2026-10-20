import {writeFileSync,readFileSync} from 'node:fs';
const [mode,...args]=process.argv.slice(2);
switch(mode) {
  case 'text':
    for(const byte of [0xef,0xbb,0xbf,0xf0,0x9f,0x98,0x80,0xe2,0x82,0xac,0xff,0xe2,0x82]) {
      process.stdout.write(Buffer.from([byte]));await new Promise(resolve=>setTimeout(resolve,2));
    }
    process.stderr.write('retained stderr');break;
  case 'args':process.stdout.write(JSON.stringify({args,cwd:process.cwd(),env:process.env.IVY_EXEC_FILE_VALUE??null}));break;
  case 'argv0':process.stdout.write(process.argv0);break;
  case 'exit':process.stdout.write('partial stdout');process.stderr.write('partial stderr');process.exitCode=7;break;
  case 'large':process[args[0]].write('x'.repeat(8192));await new Promise(resolve=>setTimeout(resolve,30000));break;
  case 'closed':process.stdout.end();process.stderr.end();await new Promise(resolve=>setTimeout(resolve,30000));break;
  case 'stdin':readFileSync(0);process.stdout.write('got EOF');break;
  case 'sleep':process.stdout.write('ready');if(args[0])writeFileSync(args[0],String(process.pid));await new Promise(resolve=>setTimeout(resolve,30000));break;
  default:throw Error('Unknown exec-file fixture');
}
