const escapeHtml = (value: string) => value.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');

// Served separately so the login page keeps the existing script-src 'self' policy.
export const loginThemeScript = `
const root = document.documentElement;
const button = document.getElementById('theme-toggle');
const system = matchMedia('(prefers-color-scheme: dark)');
const apply = theme => {
  root.dataset.theme = theme;
  button.setAttribute('aria-label', theme === 'dark' ? 'Use light theme' : 'Use dark theme');
  button.title = button.getAttribute('aria-label');
};
let saved;
try { saved = localStorage.getItem('ivy.theme'); } catch {}
apply(saved === 'light' || saved === 'dark' ? saved : system.matches ? 'dark' : 'light');
button.hidden = false;
button.addEventListener('click', () => {
  saved = root.dataset.theme === 'dark' ? 'light' : 'dark';
  apply(saved);
  try { localStorage.setItem('ivy.theme', saved); } catch {}
});
system.addEventListener('change', () => { if (!saved) apply(system.matches ? 'dark' : 'light'); });
const destination = document.querySelector('input[name="returnTo"]');
if (destination && location.hash && !destination.value.includes('#')) destination.value += location.hash;
if (destination && new URLSearchParams(location.search).get('resume') === '1') {
  const target = destination.value;
  fetch(target.split('#', 1)[0], { credentials: 'same-origin' }).then(response => {
    if (response.ok && response.headers.get('content-type')?.startsWith('text/html') &&
        new URL(response.url).pathname !== location.pathname) location.replace(target);
  }).catch(() => {});
}
`;

export function loginPage(basePath: string, returnTo: string, rejected = false): string {
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark"><link rel="icon" type="image/svg+xml" href="${escapeHtml(basePath + '/mcp-icon.svg')}"><title>Sign in · Ivy</title>
<style>
:root { color-scheme: light; --bg: #fafafa; --fg: #202020; --muted: #707070; --line: #dedede; --field: #fff; --hover: #ededed; }
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) { color-scheme: dark; --bg: #191919; --fg: #ededed; --muted: #aaa; --line: #414141; --field: #222; --hover: #303030; } }
:root[data-theme="dark"] { color-scheme: dark; --bg: #191919; --fg: #ededed; --muted: #aaa; --line: #414141; --field: #222; --hover: #303030; }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.5 "Segoe UI", system-ui, sans-serif; -webkit-font-smoothing: antialiased; }
.layout { min-height: 100svh; display: grid; grid-template-rows: auto 1fr auto; }
header { display: flex; align-items: center; justify-content: space-between; padding: 28px 36px; }
.logo { width: 104px; height: auto; display: block; }
button, input { font: inherit; }
input[type="password"] { font-size: 16px; }
button { cursor: pointer; }
.theme { width: 44px; height: 44px; border: 0; border-radius: 50%; background: transparent; color: var(--muted); }
.theme:hover { background: var(--hover); color: var(--fg); }
.theme svg { width: 20px; height: 20px; vertical-align: middle; }
main { width: min(100%, 440px); padding: 48px 28px 88px; margin: auto; }
h1 { font-size: 30px; font-weight: 600; letter-spacing: -.04em; line-height: 1.2; margin: 0 0 12px; }
.intro { color: var(--muted); margin: 0 0 36px; }
label { display: block; font-size: 13px; font-weight: 600; margin-bottom: 9px; }
input[type="password"] { display: block; width: 100%; height: 48px; padding: 0 14px; border: 1px solid var(--line); border-radius: 8px; background: var(--field); color: var(--fg); }
input::placeholder { color: var(--muted); }
:focus-visible { outline: 2px solid var(--fg); outline-offset: 4px; }
.submit { width: 100%; height: 46px; margin-top: 20px; border: 1px solid var(--fg); border-radius: 8px; background: var(--fg); color: var(--bg); font-weight: 600; }
.submit:hover { opacity: .85; }
.error { margin: 0 0 24px; padding: 12px 14px; border: 1px solid var(--fg); border-radius: 8px; font-size: 14px; }
footer { padding: 24px; text-align: center; font-size: 12px; color: var(--muted); }
@media (max-width: 480px) { header { padding: 20px 24px; } main { padding-top: 32px; padding-bottom: 56px; } h1 { font-size: 28px; } }
</style><script src="${escapeHtml(basePath + '/login/theme.js')}" defer></script></head>
<body><div class="layout"><header><span aria-label="Ivy"><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 358 128" fill="currentColor" class="logo" aria-hidden="true">
  <path d="M20 18C43 18 61 33 64 54C67 33 85 18 108 18C108 43 95 61 74 69V110H54V69C33 61 20 43 20 18Z"/>
  <path d="M160 33H174V95H160ZM194 33H209L226 77L243 33H258L233 95H219ZM272 33H288L305 58L322 33H338L312 72V95H298V72Z"/>
</svg></span><button id="theme-toggle" class="theme" type="button" aria-label="Change theme" hidden><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><circle cx="12" cy="12" r="4"/><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5"/></svg></button></header>
<main><h1>Sign in to Ivy</h1><p class="intro">Your workspace, ready when you are.</p>
${rejected ? '<p class="error" id="login-error" role="alert">That credential was not recognized. Check it and try again.</p>' : ''}
<form method="post" action="${escapeHtml(basePath + '/login')}"><label for="credential">Hive credential</label><input id="credential" type="password" name="token" required maxlength="4096" autocomplete="current-password" placeholder="Enter your credential"${rejected ? ' aria-invalid="true" aria-describedby="login-error"' : ''}><input type="hidden" name="returnTo" value="${escapeHtml(returnTo)}"><button class="submit" type="submit">Sign in</button></form></main>
<footer>Ivy workspace</footer></div></body></html>`;
}
