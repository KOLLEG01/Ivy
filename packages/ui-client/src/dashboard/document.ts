// Shared by the browser viewer and image renderer; contains no credentials.
export function dashboardDocument(html: string, channel: string): string {
  const token = JSON.stringify(channel).replaceAll("<", "\\u003c");
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; connect-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'"><style>html,body{margin:0;min-height:100vh;background:white}*{box-sizing:border-box}</style><script>
(() => {
 const channel = ${token}; let handler = null, pending = null, rendering = false;
 window.dashboard = { onUpdate(fn) { handler = fn; }, data: {}, output: {} };
 window.addEventListener('message', async event => {
  if (event.source !== parent || event.data?.channel !== channel || event.data?.type !== 'update') return;
  pending = event.data;
  if (rendering) return;
  rendering = true;
  while (pending) {
  const {data, output, sequence} = pending; pending = null;
  dashboard.data = data; dashboard.output = output;
  try {
   if (handler) await handler({data, output});
   await document.fonts.ready;
   await Promise.all(Array.from(document.images, img => img.decode().catch(() => {})));
   await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
   parent.postMessage({channel, type:'rendered', sequence}, '*');
  } catch(error) { parent.postMessage({channel, type:'error', sequence, message:String(error)}, '*'); }
  }
  rendering = false;
 });
 window.addEventListener('load', () => parent.postMessage({channel, type:'ready'}, '*'));
})();
</script></head><body>${html}</body></html>`;
}
