import assert from 'node:assert/strict';
const values=new Set([
 '', ' ', '2026-01-01', '2026-01-01T01:00:00+01:00','01/01/2026','2026-02-30T00:00:00Z',
 '2026-01-01T00:00:00','2026','1','0','50','99','100','123','2026-02','2026-1-2',
 '2026-13-01','2026-01-32','2026-00-00','not a date','Jan 1','1 Jan','Jan','March 0 2026',
 '2026-01-01\0garbage','2026-01-01\ud800','\ud800 Jan 1 2026','(unclosed Jan 1 2026','Jan 1 2026 (comment)',
 '2026-01-01T00:00:00.0000000001Z','2026-01-01T00:00:00.0000000012Z',
 '+275760-09-13T00:00:00.000Z','+275760-09-13T00:00:00.001Z','-271821-04-20T00:00:00Z','-271821-04-19T23:59:59.999Z',
 '2026-03-29T02:30:00','2026-10-25T02:30:00','2026-03-08T02:30:00','2026-11-01T01:30:00',
 '2011-12-30T12:00:00','2026-10-04T02:15:00','2026-04-05T01:45:00',
 '1900-01-01T00:00:00','1800-01-01T00:00:00','-000001-01-01T00:00:00','+260000-07-01T00:00:00',
]);
for(const year of ['0000','0001','0099','1900','2000','2026','+010000','-000001','-000000','+000000','999999999999'])
 for(const month of ['01','02','04','12','13'])for(const day of ['01','28','29','30','31','32'])
  for(const tail of ['','T00:00:00Z','T24:00:00.000Z','T24:00:00.001Z'])values.add(`${year}-${month}-${day}${tail}`);
for(const time of ['0:00','00:00','23:59','24:00','24:01','25:00','00:60','00:00:60','00:00:00.1','00:00:00.12','00:00:00.1234','00::','00:00.123','00:00:'])
 for(const zone of ['','Z','z','GMT','+01:00','-0530','+24:00','+2360','+1','+001','+12345'])values.add(`2026-01-02T${time}${zone}`);
for(const date of ['Jan 1 2026','1 January 26','Januaryblah 1, 2026','13/1/2026','2026/2/30','2.30.2026','1-2-3','1 2 49','1 2 50','2026-001-002'])
 for(const time of ['',' 1:02',' 1::03',' 1:02:03.4',' 12:01 pm',' 0:01 AM',' 13:00 PM'])
  for(const zone of ['',' GMT',' UTC+01:30',' GMT-8',' PST',' GMT+999999999:59'])values.add(date+time+zone);
for(const ws of [' ','\t','\n','\r','\v','\f','\u00a0','\u1680','\u2000','\u2028','\u2029','\u202f','\u205f','\u3000','\ufeff','\u0085','\u180e']) {
 for(const date of ['2026-01-01','Jan 1 2026','2026-01-01T00:00:00Z'])values.add(ws+date+ws);
 values.add('Jan'+ws+'1'+ws+'2026');
}
// Exact inputs retained from consumer event-admission evidence.
for(const value of ["ok","","/","//","/a~1b/~0key/1/x","/a~01b","/__proto__","/__proto__/own","/constructor","/toString","/😀","/\ud800","/missing","/0","/1/x","/01","/-0","/+0","/1.0","/length","/length/x","/99999999999999999999999999","/4294967295","x","xx","/~2","/~","text","\ud800","\ud801","2026-01-01T00:00:00.000Z","/complete","/seen","true","2025-12-31T23:59:59.999Z","2026-01-01T00:01:05.000Z","2026-01-01T00:01:05.001Z","2026-01-01","2026-01-01T01:00:00+01:00","01/01/2026","2026-02-30T00:00:00Z","invalid","/absent","conversation","fixture","item","value","a","secretary.channel.batch","fixture.source","retained","secretary.message.batch"])values.add(value);
// Deterministic token mutations exercise fallback from a partial ISO parse.
let seed=815;
const tokens=['1','01','2026','-','/',':','.',',','T','Z','Jan','GMT','AM','+','(',')',' ','\0','\ud800','0000000001'];
for(let i=0;i<1200;i++){let value='';for(let j=0;j<7;j++){seed=(Math.imul(seed,1664525)+1013904223)>>>0;value+=tokens[seed%tokens.length];}values.add(value);}
assert.equal(Date.parse('2026-02-30T00:00:00Z'),Date.UTC(2026,2,2));
process.stdout.write(JSON.stringify([...values].map(value=>({value,expected:Date.parse(value)}))));
