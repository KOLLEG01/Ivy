import {IvyError} from '../../packages/contracts/src/errors.ts';
const values=[null,false,true,0,-0,1e21,1e-7,1.7976931348623157e308,'','x','\ud800','\udfff','😀',{},[],[null,null],{valueOf:null},{nested:{toString:null}},JSON.parse('{"__proto__":{"toString":null}}')];
for(const override of [null,false,true,0,'x',[],{}, {toString:null}]){
 const value={toString:override};values.push(value,[value],[[value]],[null,'x',[null,value]],{nested:value},{valueOf:override},{toString:override,valueOf:1});
}
for(const value of [...values])values.push([value,null,'\ud800'],[[value]],{ordinary:value});
const rows=values.map(value=>{let expected;try{expected={ok:true,value:String(value)};}catch(e){expected={ok:false,error:IvyError.from(e).toWire()};}return {value:JSON.stringify(value),expected:JSON.stringify(expected)};});
process.stdout.write(JSON.stringify(rows));
