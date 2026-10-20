import {writeFileSync} from 'node:fs';
import path from 'node:path';
const args=process.argv.slice(2),at=args.indexOf('--fixture-case'),mode=at<0?'success':args[at+1];
const record=args.indexOf('--fixture-receipt');
if(record>=0){const file=path.resolve(args[record+1]);if(!path.basename(path.dirname(file)).startsWith('ivy-config-'))throw Error('Disposable receipt required');writeFileSync(file,JSON.stringify({pid:process.pid,args}));}
if(mode==='exit'){process.exitCode=7;process.stdout.write('rejected by fixture');}
else if(mode==='malformed')process.stdout.write('{not-json');
else if(mode==='max-buffer')process.stdout.write('x'.repeat(4*1024*1024+1));
else if(mode==='denied')process.stdout.write(JSON.stringify({ok:false,code:'access_denied'}));
else if(mode==='missing-code')process.stdout.write(JSON.stringify({ok:false}));
else if(mode==='empty-code')process.stdout.write(JSON.stringify({ok:false,code:''}));
else if(mode==='null-envelope')process.stdout.write('null');
else if(mode==='undefined-data')process.stdout.write(JSON.stringify({ok:true}));
else if(mode==='null-data')process.stdout.write(JSON.stringify({ok:true,data:null}));
else process.stdout.write(JSON.stringify({ok:mode==='truthy-ok'?[]:true,data:{args,cwd:process.cwd()}}));
