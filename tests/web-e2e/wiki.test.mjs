import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { fixture } from "./fixtures/fixture.mjs";

async function wiki(t, viewport) {
  const f = await fixture(t, false, viewport);
  const directory = resolve("dist/apps/wiki-ui"),
    definition = JSON.parse(
      await readFile(resolve(directory, "ivy-ui.json"), "utf8"),
    );
  await publishUi(f.client, {
    directory,
    definition,
    expectedReleaseId: null,
    mutationId: await newOperationId(f.client),
  });
  await f.login();
  await f.page.getByRole("link", { name: "Open Wiki", exact: true }).click();
  await expect(
    f.page.getByRole("heading", { name: "Wiki", exact: true }),
  ).toBeVisible();
  assert.equal(new URL(f.page.url()).pathname, "/ivy/wiki/");
  return { ...f, definition };
}

test("Wiki URL stays stable on reload and the app rail marks Wiki and leads home", { timeout: 60000 }, async (t) => {
  const f = await wiki(t, { width: 1440, height: 900 });
  await f.page.goto(f.base + '/wiki#/home');
  assert.equal(new URL(f.page.url()).pathname, '/ivy/wiki/');
  await expect(f.page.getByRole('heading', { name: 'Wiki', exact: true })).toBeVisible();
  await f.page.goto(f.base + "/ui/wiki-ui/releases/retired/index.html#/home");
  assert.equal(new URL(f.page.url()).pathname, "/ivy/ui/wiki-ui/");
  await f.page.reload();
  await expect(f.page.getByRole("heading", { name: "Wiki", exact: true })).toBeVisible();
  const rail = f.page.locator(".ivy-rail");
  await expect(rail.getByRole("link", { name: "Wiki", exact: true })).toHaveAttribute("aria-current", "page");
  await expect(rail.getByRole("link", { name: "System", exact: true })).toBeVisible();
  await rail.getByRole("link", { name: "Ivy home", exact: true }).click();
  await expect(f.page.getByRole("heading", { name: "UIs", exact: true })).toBeVisible();
  await f.page.goto(f.base + "/ui/wiki-ui/");
  await f.page.setViewportSize({ width: 390, height: 844 });
  await f.page.getByRole("link", { name: "Ivy home" }).last().click();
  await expect(f.page.getByRole("heading", { name: "UIs", exact: true })).toBeVisible();
  assert.deepEqual(f.pageErrors, []);
  assert.deepEqual(f.externalRequests, []);
});
async function openAttachments(page) {
  if (await page.getByLabel("Choose attachment", { exact: true }).isVisible())
    return;
  await page.getByRole("button", { name: "Page options", exact: true }).click();
  await page.getByRole("menuitem", { name: "Attachments", exact: true }).click();
}
async function newPage(page, title, markdown) {
  const navigation = page.getByRole("navigation", { name: "Page navigation" });
  if (!(await navigation.isVisible()))
    await page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
  await navigation.getByRole("link", { name: "New page", exact: true }).click();
  // Typed per key: a re-rendered editable title would move the caret and reverse the text.
  await page.getByLabel("Page title", { exact: true }).pressSequentially(title);
  await page.getByLabel("Markdown", { exact: true }).fill(markdown);
  // New pages are created by autosave once they have a title.
  await expect(page).toHaveURL(/#\/page\?id=/, { timeout: 15000 });
  await expect(
    page.getByRole("heading", { name: title, exact: true }),
  ).toBeVisible();
  return new URLSearchParams(new URL(page.url()).hash.split("?")[1]).get("id");
}

for (const width of [1440, 390]) test("Wiki receives live changes, falls back and preserves drafts at " + width + "px", { timeout: 120000 }, async t => {
  const f = await wiki(t, { width, height: 900 });
  const id = await newPage(f.page, "Live note", "Saved baseline.");
  await f.client.request("contracts.register", { mutationId: await newOperationId(f.client), definition: {
    key: "fixture/unrelated", version: "1.0.0", owner: { kind: "agent" }, mediaType: "text/plain",
    retention: { objects: { mode: "retain" }, revisions: { mode: "all" } }, specMarkdown: "Unrelated browser fixture data.",
  } });
  let active, blocked = false, acknowledgements = 0, reads = 0, treeReads = 0, unrelatedHints = 0, browserWrites = 0;
  await f.page.routeWebSocket("**/ws", socket => {
    if (blocked) { socket.close(); return; }
    active = socket;
    const server = socket.connectToServer();
    server.onMessage(message => {
      const frame = JSON.parse(String(message));
      if (frame.result?.changes) acknowledgements++;
      if (frame.method === "notifications.changed" && frame.params.scopes.includes("objects/fixture/unrelated")) unrelatedHints++;
      socket.send(message);
    });
  });
  f.page.on("request", request => {
    if (request.url().endsWith("/api/v1/rpc")) {
      const body = request.postDataJSON();
      if (body.method === "objects.read" && body.params.objectId === id) reads++;
      if (body.method === "objects.query" && body.params.contractKey === "wiki/page" && body.params.where?.field === "object.parentId") treeReads++;
      if (body.method === "objects.write") browserWrites++;
    }
  });
  // Observe the browser's response stream. CDP can discard completed response bodies
  // when a later refresh aborts the previous controller, even after the UI read them.
  await f.page.addInitScript(({ id }) => {
    window.__liveRead = { revision: 0, content: "" };
    const fetch = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const response = await fetch(...args);
      const request = response.url.endsWith("/api/v1/rpc") && typeof args[1]?.body === "string" ? JSON.parse(args[1].body) : null;
      if (request?.method === "objects.read" && request.params.objectId === id && !request.params.revision) {
        const frame = await response.clone().json();
        if (frame.result?.content?.encoding === "text" && frame.result.revision.revision >= window.__liveRead.revision)
          window.__liveRead = { revision: frame.result.revision.revision, content: frame.result.content.value };
      }
      return response;
    };
  }, { id });
  const latestContent = () => f.page.evaluate(() => window.__liveRead.content);
  await f.page.reload();
  await expect.poll(() => acknowledgements).toBeGreaterThan(0);
  const editor = f.page.getByLabel("Markdown", { exact: true });
  await expect(editor).toContainText("Saved baseline.");
  await expect.poll(() => f.page.evaluate(() => window.__liveRead.revision)).toBe(1);
  await f.page.waitForTimeout(750);
  const idleReads = reads;
  // Keep browser deadlines and the real server's heartbeats on the same clock.
  await f.page.waitForTimeout(16_000);
  assert.equal(reads, idleReads, "a confirmed live subscription disables periodic data reads");
  assert.equal(browserWrites, 0, "an unchanged page is never saved");
  // The mobile hierarchy is mounted only while its drawer is open.
  if (width < 768) await f.page.getByRole("button", { name: "Toggle navigation", exact: true }).click();
  await expect(f.page.getByRole("treeitem", { name: "Live note", exact: true })).toBeVisible();
  await f.page.waitForTimeout(750);
  const idleTreeReads = treeReads;
  await f.client.request("objects.write", {
    mutationId: await newOperationId(f.client), contractVersion: "1.0.0", references: {},
    create: { contractKey: "fixture/unrelated", parentId: null, ownerObjectId: null, name: "Unrelated activity" },
    content: { encoding: "text", value: "This must not reload the Wiki hierarchy." },
  });
  await expect.poll(() => unrelatedHints).toBeGreaterThan(0);
  await f.page.waitForTimeout(750);
  assert.equal(treeReads, idleTreeReads, "unrelated contract writes do not refresh the Wiki hierarchy");
  const update = async content => {
    const current = await f.client.request("objects.read", { objectId: id });
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client), objectId: id, expectedRevision: current.revision.revision,
      contractVersion: "1.0.0", references: {}, content: { encoding: "text", value: content },
    });
  };
  // A page without local changes follows saved changes from elsewhere.
  await update("External push change.");
  await expect.poll(() => reads, { timeout: 5000 }).toBeGreaterThan(idleReads);
  await expect.poll(latestContent).toBe("External push change.");
  await expect(editor).toContainText("External push change.");
  blocked = true;
  active.close();
  await f.page.waitForTimeout(100);
  const fallbackReads = reads;
  await update("Change while push is unavailable.");
  await expect.poll(() => reads, { timeout: 20000 }).toBeGreaterThan(fallbackReads);
  await expect(editor).toContainText("Change while push is unavailable.");
  const beforeReconnect = acknowledgements;
  await update("Missed before reconnect.");
  blocked = false;
  await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
  await expect.poll(() => acknowledgements).toBeGreaterThan(beforeReconnect);
  await expect(editor).toContainText("Missed before reconnect.");
  assert.equal(browserWrites, 0, "live refresh and reconnect never write the page");
  // Local edits are saved automatically, without a save action.
  if (width < 768) await f.page.keyboard.press("Escape");
  await editor.fill("My local edit.");
  await expect(f.page.getByText(/^Saved revision \d+\.$/)).toBeVisible({ timeout: 10000 });
  await expect.poll(async () => (await f.client.request("objects.read", { objectId: id })).content.value).toBe("My local edit.\n");
  const beforeMove = treeReads;
  await f.client.request("objects.move", {
    mutationId: await newOperationId(f.client), objectId: id, parentId: null, name: "Renamed live note",
  });
  // The mobile hierarchy is not mounted while its drawer is closed.
  if (width >= 768) await expect.poll(() => treeReads).toBeGreaterThan(beforeMove);
  await expect(f.page.getByRole("heading", { name: "Renamed live note", exact: true })).toBeVisible();
  await expect(editor).toContainText("My local edit.");
  assert.deepEqual(f.pageErrors, []);
});

test('Wiki rejects invalid revision links instead of opening an editable current page', { timeout: 60000 }, async t => {
  const f = await wiki(t, { width: 390, height: 844 });
  const id = await newPage(f.page, 'Revision links', 'Saved text.');
  for (const revision of ['', '0', '9007199254740992']) {
    await f.page.goto(f.base + '/ui/wiki-ui/#/page?id=' + id + '&revision=' + revision);
    await expect(f.page.getByText('This link has an invalid revision.', { exact: true })).toBeVisible();
    await expect(f.page.getByLabel('Markdown', { exact: true })).toHaveCount(0);
  }
  assert.equal((await f.client.request('objects.stat', { objectId: id })).currentRevision, 1);
  assert.deepEqual(f.pageErrors, []);
});

test('Wiki duplicate titles keep the draft editable without a false revision conflict', { timeout: 90000 }, async t => {
  const f = await wiki(t);
  await newPage(f.page, 'Occupied title', 'Other page.');
  const id = await newPage(f.page, 'Editable title', 'Original text.');
  await f.page.getByRole('heading', { name: 'Editable title', exact: true }).fill('Occupied title');
  await f.page.getByLabel('Markdown', { exact: true }).fill('Keep this draft after the rejected title.');
  await f.page.getByRole('button', { name: 'Save page', exact: true }).click();
  await expect(f.page.getByText('An Object already uses that sibling name.', { exact: true })).toBeVisible();
  await expect(f.page.getByRole('region', { name: 'Edit conflict', exact: true })).toHaveCount(0);
  await expect(f.page.getByRole('button', { name: 'Save page', exact: true })).toBeEnabled();
  await f.page.getByRole('heading', { name: 'Occupied title', exact: true }).fill('Corrected title');
  await expect(f.page.getByText('Saved revision 2.', { exact: true })).toBeVisible({ timeout: 15000 });
  const saved = await f.client.request('objects.read', { objectId: id });
  assert.equal(saved.object.name, 'Corrected title');
  assert.equal(saved.content.value, 'Keep this draft after the rejected title.\n');
  assert.deepEqual(f.pageErrors, []);
});

test('Wiki finishes a pending page creation without undoing navigation', { timeout: 90000 }, async t => {
  const f = await wiki(t, { width: 390, height: 844 });
  const navigation = f.page.getByRole('navigation', { name: 'Page navigation' });
  await f.page.getByRole('button', { name: 'Toggle navigation', exact: true }).click();
  await navigation.getByRole('link', { name: 'New page', exact: true }).click();
  await f.page.getByLabel('Page title', { exact: true }).fill('Background note');
  await f.page.getByLabel('Markdown', { exact: true }).fill('Save while leaving the editor.');
  let releaseResponse, savedOnServer;
  const responseGate = new Promise(resolve => { releaseResponse = resolve; });
  const committed = new Promise(resolve => { savedOnServer = resolve; });
  t.after(() => releaseResponse());
  await f.page.route('**/api/v1/rpc', async route => {
    const request = route.request().postDataJSON();
    if (request.method !== 'objects.write' || request.params.create?.contractKey !== 'wiki/page') return route.continue();
    const response = await route.fetch();
    savedOnServer();
    await responseGate;
    await route.fulfill({ response });
  });
  // Autosave creates the page; its response is held while the user navigates away.
  await committed;
  await f.page.getByRole('button', { name: 'Toggle navigation', exact: true }).click();
  await navigation.getByRole('link', { name: 'Home', exact: true }).click();
  await expect(f.page.getByRole('heading', { name: 'Wiki', exact: true })).toBeVisible();
  releaseResponse();
  await expect.poll(() => f.page.evaluate(() => Object.keys(localStorage).some(key => key.startsWith('ivy.wiki.draft:') && JSON.parse(localStorage.getItem(key)).title === 'Background note'))).toBe(false);
  await expect(f.page).toHaveURL(/#\/home$/);
  await expect(f.page.locator('main').getByRole('link', { name: 'Background note', exact: true })).toBeVisible();
  assert.deepEqual(f.pageErrors, []);
});

test('Wiki explains inherited archives and uploads past archived filename collisions', { timeout: 90000 }, async t => {
  const f = await wiki(t, { width: 390, height: 844 });
  const id = await newPage(f.page, 'Archive owner', 'Saved text.');
  const child = await f.client.request('wiki.create', { mutationId: await newOperationId(f.client), title: 'Archived child', parentId: id, markdown: 'Child text.' });
  const attachment = await f.client.request('objects.write', { mutationId: await newOperationId(f.client), contractVersion: '1.0.0', references: {}, create: { contractKey: 'wiki/attachment', name: 'notes.txt', parentId: id, ownerObjectId: id }, content: { encoding: 'base64', value: Buffer.from('Old file').toString('base64') } });
  await f.client.request('objects.archive', { mutationId: await newOperationId(f.client), objectId: attachment.object.id, archived: true });
  await openAttachments(f.page);
  await f.page.getByLabel('Choose attachment', { exact: true }).setInputFiles({ name: 'notes.txt', mimeType: 'text/plain', buffer: Buffer.from('New file') });
  await f.page.getByRole('button', { name: 'Upload attachment', exact: true }).click();
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toContainText('notes (2).txt');
  await expect(f.page.getByText('Saved revision 2.', { exact: true })).toBeVisible({ timeout: 15000 });
  // The next automatic save is held until the page has been archived elsewhere.
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  t.after(() => release());
  await f.page.route('**/api/v1/rpc', async route => {
    if (route.request().postDataJSON().method === 'objects.write') await gate;
    return route.continue();
  });
  await f.page.getByLabel('Markdown', { exact: true }).fill('A stale edit after archival.');
  await f.client.request('wiki.archive', { mutationId: await newOperationId(f.client), objectId: id, archived: true });
  release();
  // The archive makes the retained draft read-only; the held save is refused, not retried.
  await expect(f.page.getByLabel('Markdown', { exact: true })).toHaveAttribute('contenteditable', 'false');
  await expect(f.page.getByText('Not saved', { exact: true })).toBeVisible();
  await expect(f.page.getByRole('region', { name: 'Edit conflict', exact: true })).toHaveCount(0);
  await expect(f.page.getByLabel('Markdown', { exact: true })).toHaveText('A stale edit after archival.');
  assert.equal((await f.client.request('objects.stat', { objectId: id })).currentRevision, 2);
  await f.page.goto(f.base + '/ui/wiki-ui/#/page?id=' + child.object.id);
  await expect(f.page.getByText('This page is inside an archived page. Restore its parent first.', { exact: true })).toBeVisible();
  await f.page.getByRole('button', { name: 'Page options', exact: true }).click();
  await expect(f.page.getByRole('menuitem', { name: 'Restore page', exact: true })).toHaveCount(0);
  assert.deepEqual(f.pageErrors, []);
});

test("Wiki uploads an image as an owned attachment and embeds it in editor and viewer", { timeout: 90000 }, async (t) => {
  const f = await wiki(t, { width: 1280, height: 900 });
  const id = await newPage(f.page, "Image example", "Before image.");
  let releaseList, holdList = false;
  const listGate = new Promise(resolve => { releaseList = resolve; });
  t.after(() => releaseList());
  await f.page.route('**/api/v1/rpc', async route => {
    const request = route.request().postDataJSON();
    if (request.method === 'objects.write' && request.params.create?.contractKey === 'wiki/attachment') {
      const response = await route.fetch();
      holdList = true;
      return route.fulfill({ response });
    }
    if (holdList && request.method === 'objects.list' && request.params.parentId === id) await listGate;
    return route.continue();
  });
  await openAttachments(f.page);
  const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9L+OQAAAAASUVORK5CYII=", "base64");
  await f.page.getByLabel("Choose attachment", { exact: true }).setInputFiles({ name: "example.png", mimeType: "image/png", buffer: png });
  await f.page.getByRole("button", { name: "Upload attachment", exact: true }).click();
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1, { timeout: 30000 });
  // The embed is saved with the page automatically.
  await expect(f.page.getByText('Saved revision 2.', { exact: true })).toBeVisible({ timeout: 15000 });
  releaseList();
  await f.page.unrouteAll({ behavior: 'wait' });
  const editor = f.page.getByLabel('Markdown', { exact: true });
  await editor.click();
  await editor.press('Control+End');
  await editor.press('Enter');
  await editor.pressSequentially('After image.');
  await expect.poll(async () => (await f.client.request('objects.read', { objectId: id })).content.value, { timeout: 15000 }).toMatch(/After image\./);
  await f.page.reload();
  const embedded = f.page.locator('.wiki-document img.ivy-embedded-image');
  await expect(embedded).toHaveCount(1);
  await expect(embedded).toHaveAttribute('src', /^blob:/);
  assert.equal(await embedded.evaluate(image => image.naturalWidth), 1);
  await f.page.getByRole('button', { name: 'Enlarge example.png', exact: true }).click();
  await expect(f.page.getByRole('dialog')).toBeVisible();
  await f.page.getByRole('button', { name: 'Original size', exact: true }).click();
  await expect(f.page.getByRole('button', { name: 'Fit to window', exact: true })).toBeVisible();
  await f.page.getByRole('dialog').press('Escape');
  const saved = await f.client.request('objects.read', { objectId: id });
  assert.match(saved.content.value, /After image\./);
  assert.match(saved.content.value, /!\[example\.png\]/);
  // A clicked image is selected, so Backspace removes it like text.
  await embedded.click();
  await f.page.keyboard.press('Backspace');
  await expect(embedded).toHaveCount(0);
  await expect.poll(async () => (await f.client.request('objects.read', { objectId: id })).content.value, { timeout: 15000 }).not.toMatch(/!\[example\.png\]/);
  await f.page.goto(f.base + '/ui/wiki-ui/#/page?id=' + id + '&revision=' + saved.revision.revision);
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1);
  const attachments = await f.client.request('objects.list', { parentId: id, limit: 50 });
  assert.equal(attachments.items.filter(item => item.contractKey === 'wiki/attachment').length, 1);
  assert.deepEqual(f.pageErrors, []);
  assert.deepEqual(f.externalRequests, []);
});

test('Wiki paste, drop, duplicate filenames and restored drafts retain real attachment bytes and atomic page references', { timeout: 120000 }, async t => {
  const f = await wiki(t, { width: 390, height: 844 });
  const id = await newPage(f.page, 'Clipboard files', 'Beginning. End.');
  const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9L+OQAAAAASUVORK5CYII=';
  const editor = f.page.getByLabel('Markdown', { exact: true });
  await editor.focus();
  await editor.press('Control+Home');
  await editor.press('ArrowRight');
  await editor.evaluate((element, png) => {
    const data = new DataTransfer();
    data.items.add(new File([Uint8Array.from(atob(png), c => c.charCodeAt(0))], 'clipboard.png', { type: 'image/png' }));
    data.items.add(new File(['Original file bytes.'], 'notes [draft].txt', { type: 'text/plain' }));
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
  }, png);
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1);
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toHaveCount(1);
  // Both uploads and their embeds are saved together with the page, automatically.
  await expect(f.page.getByText('Saved revision 2.', { exact: true })).toBeVisible({ timeout: 15000 });
  await f.page.reload();
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1);
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toContainText('notes [draft].txt');
  const first = await f.client.request('objects.read', { objectId: id });
  assert.equal(Object.keys(first.revision.references).length, 2);
  assert.doesNotMatch(first.content.value, /blob:|data:image/);
  assert.match(first.content.value, /^B!\[clipboard\.png\]/, 'Paste stays at the cursor');
  await editor.evaluate(element => {
    const data = new DataTransfer(); data.items.add(new File(['Second file bytes.'], 'notes [draft].txt', { type: 'text/plain' }));
    const box = element.getBoundingClientRect();
    element.dispatchEvent(new DragEvent('drop', { dataTransfer: data, bubbles: true, cancelable: true, clientX: box.left + 20, clientY: box.top + 20 }));
  });
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toHaveCount(2);
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link').filter({ hasText: '(2)' })).toContainText('notes [draft] (2).txt');
  await editor.evaluate(element => {
    const data = new DataTransfer(); data.items.add(new File([new Uint8Array(8 * 1024 * 1024 + 1)], 'too-large.bin'));
    element.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
  });
  await expect(f.page.getByRole('alert').filter({ hasText: 'at most 8 MiB' })).toBeVisible();
  const files = await f.client.request('objects.list', { parentId: id });
  assert.equal(files.items.length, 3);
  for (const item of files.items) assert.equal(item.ownerObjectId, id);
  await expect(f.page.getByText('Saved revision 3.', { exact: true })).toBeVisible({ timeout: 15000 });
  await f.page.goto(f.base + '/ui/wiki-ui/#/page?id=' + id + '&revision=2');
  await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toHaveCount(1);
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1);
  await f.page.getByRole('button', { name: 'Images and files', exact: true }).click();
  await expect(f.page.getByText('Files in this revision', { exact: true })).toBeVisible();
  await expect(f.page.getByRole('button', { name: /^Download notes/ })).toHaveCount(1);
  assert.ok(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth));
  assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
});

test('Wiki keeps uploads through a page conflict and still shows other images when one attachment cannot be read', { timeout: 90000 }, async t => {
  const f = await wiki(t);
  const id = await newPage(f.page, 'Concurrent media', 'Original text.');
  // The first automatic save is lost in transit, so another writer gets in before the retry.
  let lost = false;
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (!lost && body.method === 'objects.write' && body.params.objectId === id) { lost = true; return route.abort(); }
    return route.continue();
  });
  await f.page.getByLabel('Markdown', { exact: true }).fill('My retained draft.');
  await expect.poll(() => lost, { timeout: 15000 }).toBe(true);
  await f.page.unroute('**/api/v1/rpc');
  const remoteFile = await f.client.request('objects.write', { mutationId: await newOperationId(f.client), contractVersion: '1.0.0', references: {}, create: { contractKey: 'wiki/attachment', name: 'remote.txt', parentId: id, ownerObjectId: id }, content: { encoding: 'base64', value: Buffer.from('Remote file').toString('base64') } });
  const remoteReferences = { ['attachment.' + remoteFile.object.id]: { objectId: remoteFile.object.id, revision: 1 } };
  await f.client.request('objects.write', { mutationId: await newOperationId(f.client), objectId: id, expectedRevision: 1, contractVersion: '1.0.0', references: remoteReferences, content: { encoding: 'text', value: 'Another writer.' } });
  await openAttachments(f.page);
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9L+OQAAAAASUVORK5CYII=', 'base64');
  await f.page.getByLabel('Choose attachment', { exact: true }).setInputFiles([
    { name: 'first.png', mimeType: 'image/png', buffer: png },
    { name: 'second.png', mimeType: 'image/png', buffer: png },
  ]);
  await f.page.getByRole('button', { name: 'Upload attachment', exact: true }).click();
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(2);
  await expect(f.page.getByRole('heading', { name: 'This page changed while you were editing' })).toBeVisible({ timeout: 20000 });
  assert.equal((await f.client.request('objects.read', { objectId: id })).content.value, 'Another writer.');
  await expect(f.page.getByLabel('Markdown', { exact: true })).toContainText('My retained draft.');
  await f.page.getByRole('button', { name: 'Use revision 2 as save base', exact: true }).click();
  await expect(f.page.getByText('Saved revision 3.', { exact: true })).toBeVisible({ timeout: 15000 });
  const saved = await f.client.request('objects.read', { objectId: id });
  assert.equal(Object.keys(saved.revision.references).length, 3);
  assert.deepEqual(saved.revision.references['attachment.' + remoteFile.object.id], remoteReferences['attachment.' + remoteFile.object.id]);
  const first = (await f.client.request('objects.list', { parentId: id })).items.find(file => file.name === 'first.png');
  await f.page.route('**/api/v1/rpc', async route => {
    const body = route.request().postDataJSON();
    if (body.method === 'objects.read' && body.params.objectId === first.id) return route.abort();
    return route.continue();
  });
  await f.page.reload();
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(1);
  await expect(f.page.getByText('1 attachment(s) could not be loaded. Use Retry attachments; other attachments remain available.')).toBeVisible();
  await f.page.unroute('**/api/v1/rpc');
  // Live setup may refresh after reload and recover before the retry control is clicked.
  const retryAttachments = f.page.getByRole('button', { name: 'Retry attachments', exact: true });
  if (await retryAttachments.isVisible()) {
    try { await retryAttachments.click({ timeout: 2000 }); }
    catch (error) {
      if (await f.page.locator('.wiki-document img.ivy-embedded-image').count() !== 2) throw error;
    }
  }
  await expect(f.page.locator('.wiki-document img.ivy-embedded-image')).toHaveCount(2);
  assert.deepEqual(f.pageErrors, []); assert.deepEqual(f.externalRequests, []);
});

test(
  "Wiki asks before permanently deleting a page and removes its revisions",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t, { width: 390, height: 844 });
    const id = await newPage(f.page, "Delete me", "First revision.");
    await f.page.getByRole("button", { name: "Page options" }).click();
    await f.page.getByRole("menuitem", { name: "Delete permanently…" }).click();
    const dialog = f.page.getByRole("dialog", { name: /Are you sure you want to permanently delete/ });
    await expect(dialog).toContainText("All revisions will be removed");
    await dialog.getByRole("button", { name: "Cancel" }).click();
    await expect(f.page.getByRole("heading", { name: "Delete me" })).toBeVisible();
    await f.page.getByRole("button", { name: "Page options" }).click();
    await f.page.getByRole("menuitem", { name: "Delete permanently…" }).click();
    await dialog.getByRole("button", { name: "Delete permanently" }).click();
    await expect(f.page.getByRole("heading", { name: "Wiki", exact: true })).toBeVisible();
    await assert.rejects(f.client.request("objects.stat", { objectId: id }), (error) => error.code === "not_found");
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "Wiki is always editable and turns a heading shortcut into WYSIWYG before Enter",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t);
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
      .getByRole("link", { name: "New page", exact: true })
      .click();
    await f.page.getByLabel("Page title", { exact: true }).fill("Instant notes");
    const editor = f.page.getByLabel("Markdown", { exact: true });
    await editor.pressSequentially("## ");
    await expect(editor.locator("h2")).toBeVisible();
    await editor.pressSequentially("Heading without Enter");
    await expect(editor.locator("h2")).toHaveText("Heading without Enter");
    await expect(f.page.getByText("Add block", { exact: true })).toHaveCount(0);
    await expect(f.page.getByText("Document", { exact: true })).toHaveCount(0);
    await expect(f.page.getByText("Hive Console", { exact: true })).toHaveCount(0);
    await expect(f.page.getByRole("tree", { name: "Pages" })).toBeVisible();
    await expect(f.page).toHaveURL(/#\/page\?id=/, { timeout: 15000 });
    await expect(
      f.page.getByRole("heading", { name: "Instant notes", exact: true }),
    ).toBeVisible();
    await expect(f.page.getByLabel("Markdown", { exact: true })).toBeVisible();
    await expect(
      f.page.getByRole("button", { name: "Edit page", exact: true }),
    ).toHaveCount(0);
    const id = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      objectId: id,
      expectedRevision: 1,
      contractVersion: "1.0.0",
      references: {},
      content: {
        encoding: "text",
        value: "```mermaid\nflowchart LR\n  Draft --> Saved\n```",
      },
    });
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.locator('.ivy-mermaid-preview svg[aria-label="Mermaid diagram"]'),
    ).toBeVisible();
    await expect(f.page.locator(".ivy-mermaid-preview image")).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Wiki uses a fresh edit revision and two real pages preserve conflicting drafts without rebasing on refresh",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t);
    const id = await newPage(f.page, "Working notes", "Original saved text.");
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      objectId: id,
      expectedRevision: 1,
      contractVersion: "1.0.0",
      references: {},
      content: { encoding: "text", value: "Newer content before Edit." },
    });
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(f.page.getByLabel("Markdown", { exact: true })).toHaveText(
      "Newer content before Edit.",
    );
    const second = await f.context.newPage();
    second.on("pageerror", (error) => f.pageErrors.push(error.message));
    await second.goto(f.page.url());
    await expect(second.getByLabel("Markdown", { exact: true })).toHaveText(
      "Newer content before Edit.",
    );
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("First writer keeps this result.");
    await expect(
      f.page.getByText("Saved revision 3.", { exact: true }),
    ).toBeVisible({ timeout: 15000 });
    // The second tab still builds on revision 2, so its automatic save meets a conflict.
    await second
      .getByLabel("Markdown", { exact: true })
      .fill("Second writer retains this draft.");
    await second.getByRole("heading", { name: "Working notes", exact: true }).fill("Second writer title");
    await expect(
      second.getByRole("heading", {
        name: "This page changed while you were editing",
        exact: true,
      }),
    ).toBeVisible({ timeout: 15000 });
    await second.getByLabel("Markdown", { exact: true }).focus();
    await second.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(second.getByLabel("Markdown", { exact: true })).toBeFocused();
    await second.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(second.getByLabel("Markdown", { exact: true })).toHaveText(
      "Second writer retains this draft.",
    );
    await expect(
      second.getByRole("button", { name: "Save page", exact: true }),
    ).toBeDisabled();
    assert.equal((await f.client.request("objects.stat", { objectId: id })).name, "Working notes", "A failed content save must not rename the page");
    await second
      .getByRole("button", { name: "Use revision 3 as save base", exact: true })
      .click();
    await expect(
      second.getByText("Saved revision 4.", { exact: true }),
    ).toBeVisible({ timeout: 15000 });
    assert.equal(
      (await f.client.request("objects.read", { objectId: id })).content.value,
      "Second writer retains this draft.\n",
    );
    assert.equal((await f.client.request("objects.stat", { objectId: id })).name, "Second writer title");
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Wiki page icons and shared tree controls preserve explicit sibling order",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t);
    const firstId = await newPage(f.page, "Zebra page", "Created first."),
      secondId = await newPage(f.page, "Alpha page", "Created second.");
    const tree = f.page.getByRole("tree", { name: "Pages" });
    const labels = () =>
      tree
        .getByRole("treeitem")
        .evaluateAll((items) =>
          items.map((item) => item.getAttribute("aria-label")),
        );
    await expect.poll(labels).toEqual(["Zebra page", "Alpha page"]);
    // Pages without child pages offer no expand control, without loading their empty branch.
    await expect(tree.getByRole("button", { name: /^Expand / })).toHaveCount(0);
    await expect(f.page.locator('link[rel="icon"]')).toHaveAttribute(
      "href",
      /^data:image\/svg\+xml,/,
    );
    const alpha = tree.getByRole("treeitem", {
      name: "Alpha page",
      exact: true,
    });
    await expect(alpha).toHaveAttribute("draggable", "true");
    await alpha
      .getByRole("button", { name: "Move Alpha page up", exact: true })
      .click();
    await expect.poll(labels).toEqual(["Alpha page", "Zebra page"]);
    const zebra = tree.getByRole("treeitem", {
      name: "Zebra page",
      exact: true,
    });
    // Dropping on the middle of a page nests it; dropping on its top edge places it beside it.
    await alpha.dragTo(zebra, { targetPosition: { x: 60, y: 16 } });
    await expect
      .poll(async () => (await f.client.request("objects.stat", { objectId: secondId })).parentId)
      .toBe(firstId);
    await expect(alpha).toHaveAttribute("aria-level", "2");
    await alpha.dragTo(zebra, { targetPosition: { x: 60, y: 2 } });
    await expect
      .poll(async () => (await f.client.request("objects.stat", { objectId: secondId })).parentId)
      .toBe(null);
    await expect.poll(labels).toEqual(["Alpha page", "Zebra page"]);

    await f.page
      .getByRole("button", { name: "Page options", exact: true })
      .click();
    await f.page
      .getByRole("menuitem", { name: "Move page", exact: true })
      .click();
    await f.page.getByLabel("Page icon", { exact: true }).fill("📘");
    await f.page
      .getByRole("button", { name: "Save location", exact: true })
      .click();
    await expect(alpha.getByText("📘", { exact: true })).toBeVisible();
    const first = await f.client.request("objects.stat", { objectId: firstId }),
      second = await f.client.request("objects.stat", { objectId: secondId });
    assert.equal(first.position, second.position + 1);
    assert.equal(second.icon, "📘");
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Wiki tree, stable links, attachment bytes and exact history work with safe Markdown",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t);
    const id = await newPage(
      f.page,
      "Reference material",
      '## Saved knowledge\n\n<script>alert("inert")</script>\n\n![Not fetched](https://invalid.test/pixel)\n',
    );
    await expect(
      f.page.getByText('<script>alert("inert")</script>', { exact: true }),
    ).toBeVisible();
    assert.equal(
      await f.page.locator("main img, main script, main iframe").count(),
      0,
    );
    await openAttachments(f.page);
    const stable = await f.page
      .getByLabel("Stable Object link", { exact: true })
      .inputValue();
    await f.page
      .getByRole("link", { name: "Add child page", exact: true })
      .click();
    await f.page.getByLabel("Page title", { exact: true }).fill("Related page");
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("A connected page.");
    await expect(f.page).toHaveURL(/#\/page\?id=/, { timeout: 15000 });
    await expect(
      f.page.getByRole("heading", { name: "Related page", exact: true }),
    ).toBeVisible();
    const childId = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    await f.page.goto(stable);
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      objectId: id,
      expectedRevision: 1,
      contractVersion: "1.0.0",
      references: {},
      content: {
        encoding: "text",
        value: "Follow the [related note](#/page?id=" + childId + ").",
      },
    });
    await f.page.evaluate(() => window.dispatchEvent(new Event("online")));
    await expect(
      f.page.getByRole("link", { name: "related note", exact: true }),
    ).toBeVisible();
    await f.page
      .getByRole("link", { name: "related note", exact: true })
      .click();
    await expect(
      f.page.getByText("A connected page.", { exact: true }),
    ).toBeVisible();
    await f.page.goto(stable);
    const bytes = Buffer.from([0, 1, 254, 255, 60, 62, 0, 42]);
    await openAttachments(f.page);
    await f.page
      .getByLabel("Choose attachment", { exact: true })
      .setInputFiles({
        name: "proof.bin",
        mimeType: "application/octet-stream",
        buffer: bytes,
      });
    await f.page
      .getByRole("button", { name: "Upload attachment", exact: true })
      .click();
    await expect(f.page.locator('.wiki-document a.ivy-attachment-link')).toContainText('proof.bin');
    await expect(f.page.getByText('Saved revision 3.', { exact: true })).toBeVisible({ timeout: 15000 });
    await f.page.locator('.wiki-document a.ivy-attachment-link').click();
    await expect(
      f.page.getByText(
        "No inline preview is available for this file type. Download the file to open it in its application.",
        { exact: true },
      ),
    ).toBeVisible();
    const downloading = f.page.waitForEvent("download");
    await f.page
      .getByRole("button", { name: "Download file", exact: true })
      .click();
    const downloaded = resolve(f.root, "wiki-proof.bin");
    await (await downloading).saveAs(downloaded);
    assert.deepEqual(await readFile(downloaded), bytes);
    await f.client.request("objects.move", {
      mutationId: await newOperationId(f.client),
      objectId: id,
      parentId: null,
      name: "Renamed reference",
    });
    await f.page.goto(stable);
    await expect(
      f.page.getByRole("heading", { name: "Renamed reference", exact: true }),
    ).toBeVisible();
    await f.page
      .getByRole("button", { name: "Page options", exact: true })
      .click();
    await f.page
      .getByRole("menuitem", { name: "Page history", exact: true })
      .click();
    await f.page.getByRole("link", { name: /^Revision 1\b/ }).click();
    await expect(
      f.page.getByText('<script>alert("inert")</script>', { exact: true }),
    ).toBeVisible();
    await expect(
      f.page.getByRole("button", { name: "Edit page", exact: true }),
    ).toHaveCount(0);
    assert.equal(
      (await f.client.request("objects.stat", { objectId: id }))
        .currentRevision,
      3,
    );
    await f.page.goto(stable);
    await f.page
      .getByRole("navigation", { name: "Page navigation" })
      .getByRole("link", { name: "Home", exact: true })
      .click();
    await f.page.getByLabel("Search pages", { exact: true }).fill("connected");
    await f.page.getByLabel("Search pages", { exact: true }).press("Enter");
    await expect(
      f.page
        .locator("main")
        .getByRole("link", { name: "Related page", exact: true }),
    ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Wiki lost save response retains its exact mutation on reload and an unsupported version keeps a draft read-only",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t);
    const id = await newPage(f.page, "Recoverable work", "First version.");
    const calls = [];
    f.page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/rpc")) {
        const body = request.postDataJSON();
        if (body.method === "objects.write") calls.push(body.params);
      }
    });
    // Every response is lost after the server committed it; autosave keeps retrying the same mutation.
    await f.page.route("**/api/v1/rpc", async (route) => {
      if (route.request().postDataJSON().method !== "objects.write")
        return route.continue();
      await route.fetch();
      await route.abort();
    });
    await f.page.getByRole("heading", { name: "Recoverable work", exact: true }).fill("Recovered title");
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("A durable saved edit.");
    await expect(f.page.getByText("Saving paused, retrying", { exact: true })).toBeVisible({ timeout: 15000 });
    await expect(f.page.getByRole("heading", { name: "Recovered title", exact: true })).toHaveAttribute("contenteditable", "plaintext-only");
    await f.page.unroute("**/api/v1/rpc");
    await f.page.reload();
    await expect(f.page.getByText(/^Saved( revision \d+\.)?$/)).toBeVisible({ timeout: 15000 });
    await expect.poll(async () => (await f.client.request("objects.read", { objectId: id })).content.value).toBe("A durable saved edit.\n");
    assert.equal((await f.client.request("objects.stat", { objectId: id })).currentRevision, 2);
    assert.ok(calls.length >= 2);
    for (const call of calls) assert.deepEqual(call, calls[0]);
    assert.equal((await f.client.request("objects.stat", { objectId: id })).name, "Recovered title");
    // A newer, unsupported page version arrives while a local edit is on its way.
    let release;
    const gate = new Promise((resolve) => { release = resolve; });
    t.after(() => release());
    await f.page.route("**/api/v1/rpc", async (route) => {
      if (route.request().postDataJSON().method === "objects.write") await gate;
      return route.continue();
    });
    const before = calls.length;
    await f.page
      .getByLabel("Markdown", { exact: true })
      .fill("Keep this unsaved draft through the update.");
    await expect.poll(() => calls.length, { timeout: 15000 }).toBeGreaterThan(before);
    await f.client.request("contracts.register", {
      mutationId: await newOperationId(f.client),
      definition: {
        ...f.definition.dataContracts[0],
        version: "2.0.0",
        specMarkdown:
          "A future incompatible Wiki data version for this explicit browser fixture.",
      },
    });
    await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      objectId: id,
      expectedRevision: 2,
      contractVersion: "2.0.0",
      references: {},
      content: { encoding: "text", value: "Upgraded by a newer writer." },
    });
    release();
    await expect(f.page.getByText("This Wiki release cannot write the current contract version. Your draft remains available to copy.", { exact: true })).toBeVisible({ timeout: 15000 });
    await expect(f.page.getByLabel("Markdown", { exact: true })).toHaveText(
      "Keep this unsaved draft through the update.",
    );
    await expect(
      f.page.getByRole("button", { name: "Save page", exact: true }),
    ).toBeDisabled();
    await f.page.reload();
    await expect(f.page.getByLabel("Markdown", { exact: true })).toHaveText(
      "Keep this unsaved draft through the update.",
    );
    await expect(
      f.page.getByRole("button", { name: "Save page", exact: true }),
    ).toBeDisabled();
    assert.equal(
      (await f.client.request("objects.stat", { objectId: id }))
        .currentRevision,
      3,
    );
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "mobile Wiki reconciles a lost attachment response only after reselecting the original file",
  { timeout: 90000 },
  async (t) => {
    const f = await wiki(t, { width: 390, height: 844 });
    await newPage(
      f.page,
      "Mobile collection",
      "Material captured from a narrow screen.",
    );
    const calls = [],
      bytes = Buffer.from("Exact original attachment bytes.");
    f.page.on("request", (request) => {
      if (request.method() === "POST" && request.url().endsWith("/rpc")) {
        const body = request.postDataJSON();
        if (
          body.method === "objects.write" &&
          body.params.create?.contractKey === "wiki/attachment" && body.params.create?.name === 'original.bin'
        )
          calls.push(body.params);
      }
    });
    await f.page.route("**/api/v1/rpc", async (route) => {
      const body = route.request().postDataJSON();
      if (
        body.method !== "objects.write" ||
        body.params.create?.contractKey !== "wiki/attachment" || body.params.create?.name !== 'original.bin'
      )
        return route.continue();
      await route.fetch();
      await route.abort();
    });
    await openAttachments(f.page);
    await f.page
      .getByLabel("Choose attachment", { exact: true })
      .setInputFiles([{
        name: 'kept.txt', mimeType: 'text/plain', buffer: Buffer.from('Successful first file in the batch.'),
      }, {
        name: "original.bin",
        mimeType: "application/octet-stream",
        buffer: bytes,
      }]);
    await f.page
      .getByRole("button", { name: "Upload attachment", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", {
        name: "Retry original upload",
        exact: true,
      }),
    ).toBeEnabled();
    await f.page.unrouteAll({ behavior: "wait" });
    await f.page.reload();
    await f.page
      .getByRole("button", { name: "Open attachments", exact: true })
      .click();
    await expect(
      f.page.getByRole("button", {
        name: "Retry original upload",
        exact: true,
      }),
    ).toBeDisabled();
    assert.equal(calls.length, 1);
    await openAttachments(f.page);
    await f.page
      .getByLabel("Choose attachment", { exact: true })
      .setInputFiles({
        name: "original.bin",
        mimeType: "application/octet-stream",
        buffer: Buffer.from("Different bytes."),
      });
    await expect(f.page.getByRole("alert")).toContainText(
      "Reselect the original file",
    );
    await expect(
      f.page.getByRole("button", {
        name: "Retry original upload",
        exact: true,
      }),
    ).toBeDisabled();
    await openAttachments(f.page);
    await f.page
      .getByLabel("Choose attachment", { exact: true })
      .setInputFiles({
        name: "original.bin",
        mimeType: "application/octet-stream",
        buffer: bytes,
      });
    await f.page
      .getByRole("button", { name: "Retry original upload", exact: true })
      .click();
    await expect(f.page.locator('.wiki-document a.ivy-attachment-link').filter({ hasText: 'original.bin' })).toHaveCount(1);
    await expect(f.page.locator('.wiki-document a.ivy-attachment-link').filter({ hasText: 'kept.txt' })).toHaveCount(1);
    await expect(f.page.getByText('An attachment needs your attention.', { exact: true })).toHaveCount(0);
    assert.deepEqual(calls[1], calls[0]);
    assert.equal(calls.length, 2);
    const attachments = await f.client.request("objects.query", {
      contractKey: "wiki/attachment",
    });
    assert.equal(attachments.items.length, 2);
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    const pageId = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    await expect.poll(async () => Object.keys((await f.client.request("objects.read", { objectId: pageId })).revision.references).length, { timeout: 15000 }).toBe(2);
    await expect(f.page.getByText(/^Saved revision \d+\.$/)).toBeVisible({ timeout: 15000 });
    await f.client.request("objects.move", {
      mutationId: await newOperationId(f.client),
      objectId: pageId,
      parentId: null,
      name: "A".repeat(255),
    });
    await f.page.reload();
    await expect(
      f.page.getByRole("heading", { name: "A".repeat(255), exact: true }),
    ).toBeVisible();
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
