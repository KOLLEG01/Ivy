import path from 'node:path';
// Batch lexical comparisons without touching the process filesystem or real env.
const originalCwd = process.cwd, originalEnv = process.env;
const values = ['', '.', '..', './', '../..', '/', '//', '///a', 'a', 'a/', 'a/../b', '..a',
  '\\', '\\a', 'C:', 'c:', 'C:child', 'D:child', 'C:/', 'c:\\a\\..\\b\\',
  'C:/a', 'C:/a/b', 'c:/A/B', 'D:/a', '//server', '//server/share', '//server/share/',
  '//server/other/a', '//other/share', '//?/C:/a/../b', '//./pipe/name', '//?/COM1:',
  'CON:', 'COM1:child', 'COM¹:child', 'NULx', 'a/../C:/b', 'a:/../b', './C:',
  'C:/İ/child', 'C:/i\u0307/other', 'C:/ΟΣ/a', 'C:/ος/b', 'C:/𐐀/a', 'C:/𐐨/b',
  'C:/\ud800/a', 'C:/\ud801/b', '\ud800', '/a/\udc00/', 'foo:bar', 'x\\CON:\\..\\a'];
const cases = [];
try {
  for (const style of ['win32','posix']) {
    const cwd = style === 'win32' ? 'C:\\work\\current' : '/work/current';
    const drives = {'C:':'C:\\drive\\current','D:':'D:\\other\\current'};
    process.cwd = () => cwd;
    process.env = new Proxy({}, {get: (_, key) => typeof key === 'string' && key[0] === '=' ? drives[key.slice(1).toUpperCase()] : originalEnv[key]});
    const add = (operation, args) => cases.push({style,cwd,drives,operation,args,expected:path[style][operation](...args)});
    for (const value of values) {add('normalize',[value]);add('isAbsolute',[value]);add('resolve',[value]);add('dirname',[value]);add('basename',[value]);}
    add('join',[]);add('resolve',[]);
    for (const a of values) for (const b of values) {
      add('join',[a,b]);add('resolve',[a,b]);add('relative',[a,b]);
      const local=path[style].relative(path[style].resolve(a),path[style].resolve(b));
      cases.push({style,cwd,drives,operation:'inside',args:[a,b],expected:local!=='..'&&!local.startsWith('..\\')&&!local.startsWith('../')&&!path[style].isAbsolute(local)});
    }
    for (const value of values) {add('join',['',value,'..','leaf/']);add('resolve',[value,'','..','leaf/']);}
  }
  process.stdout.write(JSON.stringify(cases));
} finally { process.cwd = originalCwd; process.env = originalEnv; }
