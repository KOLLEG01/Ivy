import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next) {
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL) {
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source))) return next(source.href,context);
  }
  return next(specifier,context);
}});
const {contactWindowOpen}=await import('../../packages/sdk/src/contact-window.ts');
const {restrictAssessmentParameters}=await import('../../packages/sdk/src/assessment-policy.ts');
const windows=[];
for(const timeZone of ['UTC','Europe/Berlin','America/New_York','Australia/Lord_Howe','Pacific/Apia','Asia/Kathmandu','europe/berlin','+01','+0130','+01:30','-00:30']) {
  for(const start of ['00:00','01:30','02:00','23:30']) for(const end of ['00:00','02:30','12:00']) {
    for(const date of ['2026-03-29T00:30Z','2026-03-29T01:30Z','2026-10-25T00:30Z','2026-10-25T01:30Z','2026-11-01T05:30Z','2026-11-01T06:30Z','2026-04-04T15:00Z']) {
      const window={timeZone,start,end},now=Date.parse(date);
      windows.push({window,now,open:contactWindowOpen({window},new Date(now))});
    }
  }
}
const assessments=[];
for(const config of [undefined,{},null,[],{web_search:'live','features.shell_tool':true,custom:'\ud800'}]) {
  const input={threadStart:config===undefined?{}:{config},threadResume:{extra:1},turnStart:{environments:['x']},other:'preserved'};
  try {assessments.push({input,output:restrictAssessmentParameters(input)});} catch(error){assessments.push({input,error:error.code});}
}
process.stdout.write(JSON.stringify({windows,assessments,tzVersion:process.versions.tz}));
