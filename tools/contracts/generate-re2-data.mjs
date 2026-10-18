import {readFileSync,writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import {dirname,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

// RE2JS is a build-time oracle only. The native backend embeds these bounded tables.
const require=createRequire(import.meta.url);
const root=fileURLToPath(new URL('../../',import.meta.url));
const sourceArg=process.argv.indexOf('--source');
const sourcePath=sourceArg<0?resolve(dirname(require.resolve('re2js')),'index.js'):resolve(process.argv[sourceArg+1]);
const source=readFileSync(sourcePath,'utf8');
const version=JSON.parse(readFileSync(resolve(dirname(sourcePath),'../package.json'),'utf8')).version;
const pinned=JSON.parse(readFileSync(resolve(root,'package.json'),'utf8')).dependencies.re2js;
if(version!==pinned||process.versions.unicode!=='17.0')throw Error('Generate with the pinned RE2JS and Unicode 17.0 case tables.');
const {Parser,Unicode,UnicodeTables,CharClass,PERL_GROUPS,POSIX_GROUPS}=await import('data:text/javascript;base64,'+Buffer.from(source+'\nexport {Parser,Unicode,UnicodeTables,CharClass,PERL_GROUPS,POSIX_GROUPS};').toString('base64'));
const groups={};
for(const name of [...new Set(['Any','Ascii','Assigned','Lc',...Object.keys(UnicodeTables.CATEGORIES.initializer),...Object.keys(UnicodeTables.SCRIPTS.initializer)])].sort()){
  const pair=Parser.unicodeTable(name);
  groups[name]=[false,true].map(fold=>{
    const cc=new CharClass();
    if(fold&&pair.fold){
      const ranges=new CharClass().appendTable(pair.tab).appendTable(pair.fold).cleanClass().toArray();
      cc.appendClassWithSign(ranges,pair.sign);
    }else cc.appendTableWithSign(pair.tab,pair.sign);
    return cc.cleanClass().toArray();
  });
}
for(const [name,group] of [...PERL_GROUPS,...POSIX_GROUPS])groups[name]=[false,true].map(fold=>new CharClass().appendGroup(group,fold).cleanClass().toArray());
const folds=[];
for(let point=0;point<=0x10ffff;point++){const folded=Unicode.simpleFold(point);if(folded!==point)folds.push([point,folded]);}
const data=JSON.stringify({version,unicode:process.versions.unicode,sourceHash:'sha256:'+createHash('sha256').update(source).digest('hex'),groups,folds})+'\n';
const output=resolve(root,'packages/contracts/rust/src/re2-data.json');
if(process.argv.includes('--check')){if(readFileSync(output,'utf8')!==data)throw Error('RE2 tables are stale.');}
else writeFileSync(output,data);
