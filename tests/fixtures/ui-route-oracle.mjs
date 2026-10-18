import {reservedUiSlugs,uiPath} from '../../packages/contracts/src/ui-route.ts';
import {IvyError} from '../../packages/contracts/src/errors.ts';
const strings=['','abc','a/b','a?b#c','a%20b',"a !~*'()",'\0','\ud800','\udfff','\ud800x','x\udc00','😀','é','e\u0301','中文','.', '..','\\',...reservedUiSlugs];
for(let n=0;n<128;n++)strings.push(String.fromCharCode(n));
for(const n of [0x80,0x7ff,0x800,0xd7ff,0xe000,0xffff,0x10000,0x10ffff])strings.push(String.fromCodePoint(n));
const rows=[];
for(const id of strings)for(const slug of [undefined,'','valid',id]){
 const metadata={uiId:id,...(slug===undefined?{}:{slug})};let result;
 try{result={ok:true,value:uiPath(metadata)};}catch(e){result={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({metadata:JSON.stringify(metadata),result:JSON.stringify(result)});
}
process.stdout.write(JSON.stringify({reservedUiSlugs,rows}));
