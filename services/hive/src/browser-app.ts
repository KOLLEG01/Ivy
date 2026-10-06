/** One installable application and worker scope for every UI in this Hive. */
export function browserAppManifest(basePath: string) {
  const root = basePath + "/";
  return {
    id: root,
    name: "Ivy",
    short_name: "Ivy",
    description: "Your Ivy workspace",
    start_url: root,
    scope: root,
    display: "standalone",
    background_color: "#ffffff",
    theme_color: "#ffffff",
    icons: [192, 512].map((size) => ({
      src: root + `app/icon-${size}.png`,
      sizes: `${size}x${size}`,
      type: "image/png",
      purpose: "any maskable",
    })),
  };
}

// No authenticated pages or API responses enter an offline cache. Published UIs
// retain their own release lifecycle; the worker only delivers browser messages.
export const browserAppWorker = `
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
const root = new URL(self.registration.scope);
const safeUrl = value => {
  try {
    const url = new URL(value || root.href, root);
    if (url.origin === root.origin && url.pathname.startsWith(root.pathname)) return url.href;
  } catch {}
  return root.href;
};
self.addEventListener('push', event => {
  let payload = {};
  try { payload = event.data.json() || {}; } catch {}
  event.waitUntil(self.registration.showNotification(payload.title || 'Ivy', {
    body: payload.body || 'There is an update in Ivy.', tag: payload.tag,
    icon: new URL('app/icon-192.png', root).href,
    data: { url: safeUrl(payload.url) }
  }));
});
self.addEventListener('notificationclick', event => {
  event.notification.close();
  event.waitUntil((async () => {
    const url = safeUrl(event.notification.data?.url);
    const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
    for (const client of windows) {
      const current = new URL(client.url);
      if (current.origin !== root.origin || !current.pathname.startsWith(root.pathname)) continue;
      try { await client.navigate(url); await client.focus(); return; } catch {}
    }
    await self.clients.openWindow(url);
  })());
});
`;
