import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
const config=readFileSync('services/chat-bridge/src/whatsapp/config.ts','utf8');
const helpers=config.slice(config.indexOf('export const phone'),config.indexOf('export async function readWhatsAppConfig')).replaceAll('export ','');
const source=readFileSync('services/chat-bridge/src/whatsapp/socket.ts','utf8');
const method=source.slice(source.indexOf('  async admitted('),source.indexOf('  private async inbound('));
const create=new Function(stripTypeScriptTypes(helpers+'\nclass Oracle { constructor(config) { this.config=config; }\n'+method+'}',{mode:'transform'})+'\nreturn {phone,jidPhone,Oracle};');
const {phone,jidPhone,Oracle}=create();
const rows=readFileSync(0,'utf8').trimEnd().split('\n').filter(Boolean);
const result=[];
for(const row of rows){
  const values=row.split('\t').map(v=>Buffer.from(v,'hex').toString('utf8'));
  if(values[0]==='parse'){result.push((phone(values[1])??'-')+'\t'+(jidPhone(values[1])??'-'));continue;}
  const [_,id,jid,alternate,fromMe,participant,mapped,...phones]=values;
  let lookups=0;
  const oracle=new Oracle({senders:phones.map((phoneNumber,index)=>({phoneNumber,index}))});
  const selected=await oracle.admitted({key:{id,remoteJid:jid,remoteJidAlt:alternate,fromMe:fromMe==='1',participant}},
    {signalRepository:{lidMapping:{getPNForLID:async()=>{lookups++;return mapped||null;}}}});
  result.push((selected?.index??'-')+'\t'+lookups);
}
process.stdout.write(result.join('\n'));
