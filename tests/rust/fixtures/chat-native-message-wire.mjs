// Locally generated protocol messages; no transport or account is opened.
import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {BufferJSON,proto} from 'baileys';
const [mode,root]=process.argv.slice(2);
const encode=value=>JSON.stringify(value,BufferJSON.replacer);
const bytes=(n=32)=>Buffer.alloc(n,17);
if(mode==='prepare'){
 const samples=[
  {},
  {conversation:'Native message — 日本語'},
  {locationMessage:{degreesLatitude:NaN,degreesLongitude:Infinity}},
  {locationMessage:{degreesLatitude:-Infinity,degreesLongitude:-0}},
  {imageMessage:{url:'https://example.invalid/media',mimetype:'image/png',caption:'',fileSha256:bytes(),fileLength:'9007199254740993',mediaKey:bytes(),fileEncSha256:bytes(),jpegThumbnail:bytes(0),width:0,height:0,contextInfo:{mentionedJid:[],stanzaId:'QUOTE',quotedMessage:{conversation:'Quoted text'},forwardingScore:0,isForwarded:false}}},
  {audioMessage:{mimetype:'audio/ogg; codecs=opus',fileLength:'19',seconds:2,ptt:true,mediaKeyTimestamp:'-2',mediaKey:bytes(),waveform:Buffer.from([0,31,255]),contextInfo:{}}},
  {documentWithCaptionMessage:{message:{documentMessage:{title:'Notes',fileName:'native.txt',mimetype:'text/plain',fileLength:'18',caption:'Document caption',mediaKey:bytes(),fileSha256:bytes()}}}},
  {contactsArrayMessage:{displayName:'',contacts:[{displayName:'Synthetic contact',vcard:'BEGIN:VCARD\nEND:VCARD'}]},messageContextInfo:{messageSecret:bytes()}},
  {extendedTextMessage:{text:'Link',previewType:0,contextInfo:{mentionedJid:['15555550101@s.whatsapp.net'],expiration:0}}},
  {pollCreationMessage:{name:'Choice',options:[{optionName:'One'},{optionName:'Two'}],selectableOptionsCount:1}},
  {interactiveMessage:{header:{title:'Native',hasMediaAttachment:false},body:{text:'Choose'},nativeFlowMessage:{buttons:[{name:'quick_reply',buttonParamsJson:'{"display_text":"Continue","id":"synthetic"}'}],messageParamsJson:'{}'}}},
 ];
 const expected=[];
 for(let i=0;i<samples.length;i++){
  const data=proto.Message.encode(proto.Message.fromObject(samples[i])).finish();
  writeFileSync(join(root,`wire-${i}.bin`),data);
  expected.push(encode(proto.Message.decode(data)));
 }
 writeFileSync(join(root,'expected.json'),JSON.stringify(expected));
}else if(mode==='verify'){
 const expected=JSON.parse(readFileSync(join(root,'expected.json'),'utf8'));
 for(let i=0;i<expected.length;i++)assert.equal(encode(proto.Message.decode(readFileSync(join(root,`native-${i}.bin`)))),expected[i],`Native protocol round trip ${i}`);
}else throw Error('Unknown fixture mode');
