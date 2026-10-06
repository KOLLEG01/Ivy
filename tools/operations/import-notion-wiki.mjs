import { parseArgs } from "node:util";
import { createHash, randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";

// Credentials are read only from process environment variables. By default this is a dry run:
// node tools/operations/import-notion-wiki.mjs --notion-user PAGE_ID --notion-contexts PAGE_ID --hive-base HTTPS_URL
// Set NOTION_API_KEY and IVY_HIVE_CREDENTIAL; add --apply to import, and --clear-first to
// permanently replace the entire Wiki. All source pages and media are loaded before deletion.

const NOTION_VERSION = "2026-03-11";
const NOTION_ORIGIN = "https://api.notion.com";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function pageUrl(id) {
  return `https://www.notion.so/${id.replaceAll("-", "")}`;
}

function blockUrl(pageId, blockId) {
  return `${pageUrl(pageId)}#${blockId.replaceAll("-", "")}`;
}

function notionPageId(url) {
  const match = /([0-9a-f]{32})(?:[?#/]|$)/i.exec(
    url?.replaceAll("-", "") ?? "",
  );
  return match?.[1].toLowerCase() ?? null;
}

function plainText(items = []) {
  return items
    .map(
      (item) =>
        item.plain_text ?? item.text?.content ?? item.mention?.page?.id ?? "",
    )
    .join("");
}

function escapeMarkdown(value) {
  return value.replaceAll("\\", "\\\\").replace(/([`*_\[\]<>])/g, "\\$1");
}

function richText(items = []) {
  return items
    .map((item) => {
      let value =
        item.type === "equation"
          ? `$${item.equation?.expression ?? ""}$`
          : item.annotations?.code
            ? (item.plain_text ?? item.text?.content ?? "")
            : escapeMarkdown(item.plain_text ?? item.text?.content ?? "");
      if (item.annotations?.code) {
        const fence = "`".repeat(
          Math.max(
            1,
            ...[...value.matchAll(/`+/g)].map((match) => match[0].length + 1),
          ),
        );
        value = `${fence}${value}${fence}`;
      } else {
        if (item.annotations?.bold) value = `**${value}**`;
        if (item.annotations?.italic) value = `*${value}*`;
        if (item.annotations?.strikethrough) value = `~~${value}~~`;
      }
      const href = item.href ?? item.text?.link?.url;
      return href ? `[${value}](${href})` : value;
    })
    .join("");
}

function isHeading(block) {
  return /^heading_[1-6]$/.test(block.type);
}

function isChildLink(block, childIds) {
  if (block.type === "link_to_page") {
    return childIds.has(
      block.link_to_page?.page_id?.replaceAll("-", "").toLowerCase(),
    );
  }
  if (
    !["paragraph", "bulleted_list_item", "numbered_list_item"].includes(
      block.type,
    )
  )
    return false;
  const parts = block[block.type]?.rich_text ?? [];
  const nonblank = parts.filter((part) => plainText([part]).trim());
  return (
    nonblank.length > 0 &&
    nonblank.every((part) => {
      const id =
        part.mention?.page?.id?.replaceAll("-", "").toLowerCase() ??
        notionPageId(part.href ?? part.text?.link?.url);
      return id && childIds.has(id);
    })
  );
}

function allBlocks(blocks) {
  return blocks.flatMap((block) => [block, ...allBlocks(block.children ?? [])]);
}

export function stripSubpageNavigation(
  blocks,
  childIds = new Set(
    allBlocks(blocks)
      .filter((block) => block.type === "child_page")
      .map((block) => block.id.replaceAll("-", "").toLowerCase()),
  ),
) {
  return blocks.filter((block, index) => {
    if (block.type === "child_page" || isChildLink(block, childIds))
      return false;
    if (
      !isHeading(block) ||
      plainText(block[block.type]?.rich_text).trim().toLocaleLowerCase("de") !==
        "unterseiten"
    )
      return true;
    let sawChild = false;
    for (const next of blocks.slice(index + 1)) {
      if (isHeading(next)) break;
      if (next.type === "child_page" || isChildLink(next, childIds)) {
        sawChild = true;
        continue;
      }
      if (
        next.type === "paragraph" &&
        !plainText(next.paragraph?.rich_text).trim()
      )
        continue;
      return true;
    }
    return !sawChild;
  });
}

function renderChildren(block, pageId, indent, childIds) {
  return renderBlocks(block.children ?? [], pageId, indent, childIds);
}

function renderBlock(block, pageId, indent, childIds) {
  const data = block[block.type] ?? {};
  const text = richText(data.rich_text);
  const nested =
    block.type === "table"
      ? ""
      : renderChildren(block, pageId, indent + 2, childIds);
  switch (block.type) {
    case "paragraph":
      return [text, nested].filter(Boolean).join("\n\n");
    case "heading_1":
      return `# ${text}${nested ? `\n\n${nested}` : ""}`;
    case "heading_2":
      return `## ${text}${nested ? `\n\n${nested}` : ""}`;
    case "heading_3":
      return `### ${text}${nested ? `\n\n${nested}` : ""}`;
    case "heading_4":
      return `#### ${text}${nested ? `\n\n${nested}` : ""}`;
    case "heading_5":
      return `##### ${text}${nested ? `\n\n${nested}` : ""}`;
    case "heading_6":
      return `###### ${text}${nested ? `\n\n${nested}` : ""}`;
    case "bulleted_list_item":
    case "numbered_list_item":
    case "to_do": {
      const marker =
        block.type === "bulleted_list_item"
          ? "-"
          : block.type === "numbered_list_item"
            ? "1."
            : `- [${data.checked ? "x" : " "}]`;
      return `${" ".repeat(indent)}${marker} ${text}${nested ? `\n${nested}` : ""}`;
    }
    case "quote":
      return `> ${text.replaceAll("\n", "\n> ")}${
        nested
          ? `\n>\n${nested
              .split("\n")
              .map((line) => `> ${line}`)
              .join("\n")}`
          : ""
      }`;
    case "callout":
      return `> ${data.icon?.emoji ? `${data.icon.emoji} ` : ""}${text}${
        nested
          ? `\n>\n${nested
              .split("\n")
              .map((line) => `> ${line}`)
              .join("\n")}`
          : ""
      }`;
    case "toggle":
      return `${" ".repeat(indent)}- **${text}**${nested ? `\n${nested}` : ""}`;
    case "code": {
      const code = plainText(data.rich_text);
      const fence = "`".repeat(
        Math.max(
          3,
          ...[...code.matchAll(/`+/g)].map((match) => match[0].length + 1),
        ),
      );
      const language =
        data.language === "plain text" ? "text" : (data.language ?? "");
      return `${fence}${language}\n${code}\n${fence}`;
    }
    case "divider":
      return "---";
    case "equation":
      return `$$\n${data.expression ?? ""}\n$$`;
    case "bookmark":
    case "embed":
    case "link_preview":
      return `[${richText(data.caption) || data.url}](${data.url})`;
    case "image":
    case "file":
    case "pdf":
    case "audio":
    case "video": {
      const caption = richText(data.caption) || block.type;
      const marker = `notion-media:${block.id}`;
      return block.type === "image"
        ? `![${caption}](${marker})`
        : `[${caption}](${marker})`;
    }
    case "link_to_page":
      return `[Notion page](${pageUrl(data.page_id)})`;
    case "table": {
      const rows = (block.children ?? []).map(
        (row) => row.table_row?.cells?.map(richText) ?? [],
      );
      if (!rows.length) return "";
      const width = Math.max(...rows.map((row) => row.length));
      const line = (row) =>
        `| ${Array.from({ length: width }, (_, i) => (row[i] ?? "").replaceAll("|", "\\|")).join(" | ")} |`;
      return [
        line(rows[0]),
        line(Array(width).fill("---")),
        ...rows.slice(1).map(line),
      ].join("\n");
    }
    case "table_of_contents":
    case "breadcrumb":
      return "";
    case "column_list":
    case "column":
    case "synced_block":
      if (data.synced_from && !nested)
        throw new Error(
          `Synced Notion block needs expansion: ${blockUrl(pageId, block.id)}`,
        );
      return nested;
    default:
      throw new Error(
        `Unsupported Notion block ${block.type} at ${blockUrl(pageId, block.id)}`,
      );
  }
}

export function renderBlocks(
  blocks,
  pageId,
  indent = 0,
  childIds = new Set(
    allBlocks(blocks)
      .filter((block) => block.type === "child_page")
      .map((block) => block.id.replaceAll("-", "").toLowerCase()),
  ),
) {
  const kept = stripSubpageNavigation(blocks, childIds);
  const parts = kept
    .map((block) => ({
      block,
      markdown: renderBlock(block, pageId, indent, childIds),
    }))
    .filter((part) => part.markdown.trim());
  return parts
    .map((part, index) => {
      const previous = parts[index - 1]?.block.type;
      const current = part.block.type;
      const list = ["bulleted_list_item", "numbered_list_item", "to_do"];
      const separator =
        index && list.includes(previous) && list.includes(current)
          ? "\n"
          : index
            ? "\n\n"
            : "";
      return separator + part.markdown;
    })
    .join("")
    .trimEnd();
}

export function makeNotionClient(token, fetchImpl = fetch) {
  return async function notionGet(path) {
    for (let attempt = 0; attempt < 6; attempt++) {
      const response = await fetchImpl(new URL(path, NOTION_ORIGIN), {
        headers: {
          Authorization: `Bearer ${token}`,
          "Notion-Version": NOTION_VERSION,
        },
        signal: AbortSignal.timeout(35_000),
      });
      if (response.ok) return response.json();
      const body = await response.text();
      if (
        ![429, 500, 502, 503, 504, 529].includes(response.status) ||
        attempt === 5
      ) {
        throw new Error(`Notion GET ${path}: HTTP ${response.status}: ${body}`);
      }
      const retryAfterHeader = response.headers.get("Retry-After");
      const retryAfter =
        retryAfterHeader === null ? NaN : Number(retryAfterHeader);
      await sleep(
        (Number.isFinite(retryAfter) && retryAfter >= 0
          ? retryAfter
          : Math.min(2 ** attempt, 30)) *
          1000 +
          Math.random() * 250,
      );
    }
  };
}

async function listNotionChildren(notionGet, blockId) {
  const blocks = [];
  let cursor;
  do {
    const query = new URLSearchParams({ page_size: "100" });
    if (cursor) query.set("start_cursor", cursor);
    const page = await notionGet(`/v1/blocks/${blockId}/children?${query}`);
    blocks.push(...page.results);
    cursor = page.has_more ? page.next_cursor : null;
    if (page.has_more && !cursor)
      throw new Error(`Notion omitted a cursor for ${blockId}`);
  } while (cursor);
  return blocks;
}

async function readBlocks(notionGet, pageId) {
  const blocks = await listNotionChildren(notionGet, pageId);
  for (const block of blocks) {
    if (
      block.has_children &&
      !["child_page", "child_database"].includes(block.type)
    ) {
      block.children = await readBlocks(notionGet, block.id);
    }
  }
  return blocks;
}

export async function readNotionTree(
  notionGet,
  id,
  title,
  onPageRead = () => {},
) {
  title = title.normalize("NFC");
  const blocks = await readBlocks(notionGet, id);
  const descendants = allBlocks(blocks);
  const databases = descendants.filter(
    (block) => block.type === "child_database",
  );
  if (databases.length)
    throw new Error(
      `Notion database under ${title}: ${databases.map((block) => blockUrl(id, block.id)).join(", ")}`,
    );
  const children = [];
  for (const block of descendants.filter(
    (block) => block.type === "child_page",
  )) {
    children.push(
      await readNotionTree(
        notionGet,
        block.id,
        block.child_page.title,
        onPageRead,
      ),
    );
  }
  const names = new Set();
  for (const child of children) {
    if (names.has(child.title))
      throw new Error(
        `Duplicate Notion child title under ${title}: ${child.title}`,
      );
    names.add(child.title);
  }
  const page = {
    id,
    title,
    markdown: renderBlocks(blocks, id).trim(),
    media: descendants.filter((block) => ["image", "file", "pdf", "audio", "video"].includes(block.type)).map((block) => {
      const data = block[block.type];
      const url = data.type === "external" ? data.external?.url : data.file?.url;
      if (!url || !/^https:\/\//i.test(url)) throw new Error(`Notion media has no HTTPS source: ${blockUrl(id, block.id)}`);
      return { id: block.id, type: block.type, url, name: data.name ?? null };
    }),
    children,
  };
  onPageRead({ title, blockCount: descendants.length });
  return page;
}

export function validateSourceTree(sources) {
  function validate(page, depth) {
    if (
      !page.title.isWellFormed() ||
      page.title.length < 1 ||
      page.title.length > 255 ||
      /[\/\\\0]/.test(page.title) ||
      page.title === "." ||
      page.title === ".." ||
      depth > 64
    ) {
      throw new Error(
        `Notion page title or depth is invalid for the Wiki: ${page.title}`,
      );
    }
    if (
      page.markdown.length > 1_048_576 ||
      Buffer.byteLength(page.markdown, "utf8") > 1_048_576
    ) {
      throw new Error(
        `Notion page content exceeds the Wiki limit: ${page.title}`,
      );
    }
    for (const media of page.media ?? []) {
      if (!page.markdown.includes(`notion-media:${media.id}`))
        throw new Error(`Notion media is missing from rendered page: ${page.title}/${media.id}`);
    }
    for (const child of page.children) validate(child, depth + 1);
  }
  for (const source of sources) validate(source, 1);
}

const MAX_ATTACHMENT_BYTES = 8 * 1024 * 1024;
const mediaTypes = [
  { type: "image/png", ext: "png", match: (b) => b.subarray(0, 8).equals(Buffer.from("89504e470d0a1a0a", "hex")) },
  { type: "image/jpeg", ext: "jpg", match: (b) => b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff },
  { type: "image/gif", ext: "gif", match: (b) => b.subarray(0, 6).toString("ascii") === "GIF87a" || b.subarray(0, 6).toString("ascii") === "GIF89a" },
  { type: "image/webp", ext: "webp", match: (b) => b.subarray(0, 4).toString("ascii") === "RIFF" && b.subarray(8, 12).toString("ascii") === "WEBP" },
];

export async function preloadMedia(sources, fetchImpl = fetch, onLoaded = () => {}) {
  async function visit(page) {
    for (const media of page.media ?? []) {
      let response;
      for (let attempt = 0; attempt < 4; attempt++) {
        response = await fetchImpl(media.url, { signal: AbortSignal.timeout(60_000) });
        if (response.ok) break;
        if (![429, 500, 502, 503, 504].includes(response.status) || attempt === 3)
          throw new Error(`Notion media ${media.id}: HTTP ${response.status}`);
        await sleep(1000 * 2 ** attempt);
      }
      const length = Number(response.headers.get("Content-Length"));
      if (length > MAX_ATTACHMENT_BYTES) throw new Error(`Notion media exceeds 8 MiB: ${page.title}/${media.id}`);
      const bytes = Buffer.from(await response.arrayBuffer());
      if (!bytes.length || bytes.length > MAX_ATTACHMENT_BYTES) throw new Error(`Notion media has invalid size: ${page.title}/${media.id}`);
      const image = mediaTypes.find((item) => item.match(bytes));
      if (media.type === "image" && !image) throw new Error(`Unsupported Notion image format: ${page.title}/${media.id}`);
      const urlName = decodeURIComponent(new URL(media.url).pathname.split("/").pop() ?? "");
      const originalName = media.name || urlName;
      const extension = image?.ext ?? (/^[\w .()-]{1,200}\.[a-z0-9]{1,12}$/i.test(originalName) ? originalName.split(".").pop().toLowerCase() : "bin");
      media.fileName = `notion-${media.type}-${media.id}.${extension}`;
      media.bytes = bytes;
      media.contentHash = `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
      onLoaded({ page: page.title, id: media.id, byteLength: bytes.length });
    }
    for (const child of page.children) await visit(child);
  }
  for (const source of sources) await visit(source);
}

function notionTitle(page) {
  return Object.values(page.properties ?? {})
    .find((value) => value.type === "title")
    ?.title?.map((item) => item.plain_text ?? item.text?.content ?? "")
    .join("");
}

export async function verifyNotionRoots(notionGet, roots) {
  const pages = await Promise.all(
    roots.map(async (root) => notionGet(`/v1/pages/${root.id}`)),
  );
  for (let i = 0; i < roots.length; i++) {
    if (notionTitle(pages[i]) !== roots[i].title)
      throw new Error(`Notion root title differs from ${roots[i].title}`);
    if (pages[i].parent?.type !== "page_id")
      throw new Error(`${roots[i].title} has no Notion parent page`);
  }
  const parentId = pages[0].parent.page_id;
  if (pages.some((page) => page.parent.page_id !== parentId))
    throw new Error("Notion roots do not share one parent");
  const parent = await notionGet(`/v1/pages/${parentId}`);
  if (notionTitle(parent) !== "IvyWorkspace")
    throw new Error("Notion roots are not under IvyWorkspace");
}

export function makeHiveClient(base, credential, fetchImpl = fetch) {
  const endpoint = new URL("/api/v1/rpc", base);
  return async function hive(method, params) {
    const id = randomUUID();
    const response = await fetchImpl(endpoint, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${credential}`,
      },
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
      signal: AbortSignal.timeout(35_000),
    });
    const frame = await response.json();
    if (frame.id !== id || frame.error)
      throw new Error(
        `Hive ${method}: ${JSON.stringify(frame.error ?? frame)}`,
      );
    return frame.result;
  };
}

async function listWikiChildren(hive, parentId, includeArchived = false) {
  const children = [];
  let cursor;
  do {
    const page = await hive("wiki.list", {
      parentId,
      limit: 100,
      ...(includeArchived ? { includeArchived: true } : {}),
      ...(cursor ? { cursor } : {}),
    });
    children.push(...page.items);
    cursor = page.nextCursor;
  } while (cursor);
  return children;
}

async function planPage(hive, source, current, path) {
  let markdown = source.markdown;
  const attachments = [], mediaExisting = new Map();
  if (current && source.media?.length) {
    const objects = [];
    let cursor;
    do {
      const result = await hive("objects.list", { parentId: current.object.id, limit: 100, ...(cursor ? { cursor } : {}) });
      objects.push(...result.items); cursor = result.nextCursor;
    } while (cursor);
    for (const media of source.media) {
      const item = objects.find((item) => item.name === media.fileName && item.contractKey === "wiki/attachment");
      if (!item) continue;
      const saved = await hive("objects.read", { objectId: item.id });
      if (saved.object.ownerObjectId !== current.object.id) throw new Error(`Attachment owner differs: ${path}/${media.fileName}`);
      mediaExisting.set(media.id, saved);
      const ref = current.revision.references?.[`attachment.${item.id}`];
      if (saved.revision.contentHash === media.contentHash && ref?.revision === saved.revision.revision && ref.objectId === item.id) {
        markdown = markdown.replaceAll(`notion-media:${media.id}`, `#/page?id=${encodeURIComponent(item.id)}&revision=${saved.revision.revision}`);
        attachments.push({ objectId: item.id, revision: saved.revision.revision, media });
      }
    }
  }
  const action = !current
    ? "create"
    : current.content.value === markdown && !markdown.includes("notion-media:")
      ? "skip"
      : "update";
  const plan = { source, current, path, action, markdown, attachments, mediaExisting, children: [] };
  const existing = current
    ? await listWikiChildren(hive, current.object.id)
    : [];
  for (const child of source.children) {
    const match = existing.filter(
      (item) => item.values["object.name"] === child.title,
    );
    if (match.length > 1)
      throw new Error(`Ambiguous Wiki title: ${path}/${child.title}`);
    const page = match.length
      ? await hive("wiki.read", { objectId: match[0].objectId })
      : null;
    plan.children.push(
      await planPage(hive, child, page, `${path}/${child.title}`),
    );
  }
  return plan;
}

export async function planImport(hive, sources) {
  const plans = [];
  const existingRoots = await listWikiChildren(hive, null);
  for (const source of sources) {
    const matches = existingRoots.filter(
      (item) => item.values["object.name"] === source.title,
    );
    if (matches.length > 1)
      throw new Error(`Ambiguous Wiki root title: ${source.title}`);
    const current = matches.length
      ? await hive("wiki.read", { objectId: matches[0].objectId })
      : null;
    plans.push(await planPage(hive, source, current, `/${source.title}`));
  }
  return plans;
}

function flatten(plans) {
  return plans.flatMap((plan) => [plan, ...flatten(plan.children)]);
}

export async function clearWiki(hive) {
  const roots = await listWikiChildren(hive, null, true);
  const epoch = (await hive("system.status", {})).runtimeEpoch;
  for (const root of roots) {
    const page = await hive("wiki.read", { objectId: root.objectId });
    const mutationId = `${epoch}:${Date.now()}:${randomUUID()}`;
    await hive("objects.delete", {
      objectId: root.objectId,
      expectedRevision: page.revision.revision,
      mutationId,
    });
  }
  const remaining = await listWikiChildren(hive, null, true);
  if (remaining.length)
    throw new Error(
      `Wiki clear incomplete: ${remaining.length} root pages remain.`,
    );
  return roots.length;
}

export async function applyImport(hive, plans, onPageWritten = () => {}) {
  const epoch = (await hive("system.status", {})).runtimeEpoch;
  async function apply(plan, parentId) {
    let objectId = plan.current?.object.id;
    if (plan.action !== "skip") {
      const mutationId = `${epoch}:${Date.now()}:${randomUUID()}`;
      try {
        const hasMedia = !!plan.source.media?.length;
        const result =
          plan.action === "create"
            ? await hive("wiki.create", {
                mutationId,
                title: plan.source.title,
                parentId,
                markdown: hasMedia ? "" : plan.source.markdown,
              })
            : await hive("wiki.update", {
                mutationId,
                objectId,
                expectedRevision: plan.current.revision.revision,
                markdown: hasMedia ? plan.current.content.value : plan.source.markdown,
              });
        objectId = result.object.id;
        if (hasMedia) {
          const references = {};
          let markdown = plan.source.markdown;
          plan.attachments = [];
          for (const media of plan.source.media) {
            if (!media.bytes || !media.contentHash || !media.fileName)
              throw new Error(`Media was not preloaded: ${media.id}`);
            const existing = plan.mediaExisting?.get(media.id);
            const saved = existing?.revision.contentHash === media.contentHash ? existing : await hive("objects.write", {
              mutationId: `${epoch}:${Date.now()}:${randomUUID()}`,
              contractVersion: "1.0.0",
              references: {},
              ...(existing ? { objectId: existing.object.id, expectedRevision: existing.revision.revision } : { create: { contractKey: "wiki/attachment", parentId: objectId, ownerObjectId: objectId, name: media.fileName } }),
              content: { encoding: "base64", value: media.bytes.toString("base64") },
            });
            if (saved.revision.contentHash !== media.contentHash || saved.revision.byteLength !== media.bytes.length)
              throw new Error(`Hive attachment bytes differ: ${media.id}`);
            const ref = { objectId: saved.object.id, revision: saved.revision.revision };
            references[`attachment.${saved.object.id}`] = ref;
            const localUrl = `#/page?id=${encodeURIComponent(saved.object.id)}&revision=${saved.revision.revision}`;
            markdown = markdown.replaceAll(`notion-media:${media.id}`, localUrl);
            plan.attachments.push({ ...ref, media });
          }
          if (markdown.includes("notion-media:")) throw new Error(`Unresolved media on ${plan.path}`);
          const page = await hive("wiki.read", { objectId });
          await hive("objects.write", {
            mutationId: `${epoch}:${Date.now()}:${randomUUID()}`,
            contractVersion: "1.0.0",
            objectId,
            expectedRevision: page.revision.revision,
            references: { ...page.revision.references, ...references },
            content: { encoding: "text", value: markdown },
          });
          plan.markdown = markdown;
        }
      } catch (error) {
        throw new Error(
          `${plan.path}: mutation ${mutationId} has uncertain outcome; inspect it before retrying. ${error.message}`,
          { cause: error },
        );
      }
    }
    plan.objectId = objectId;
    onPageWritten({ path: plan.path, action: plan.action });
    for (const child of plan.children) await apply(child, objectId);
  }
  for (const plan of plans) await apply(plan, null);
}

export async function verifyImport(hive, plans) {
  for (const plan of flatten(plans)) {
    const page = await hive("wiki.read", {
      objectId: plan.objectId ?? plan.current?.object.id,
    });
    if (
      page.object.path !== plan.path ||
      page.content.value !== (plan.markdown ?? plan.source.markdown)
    ) {
      throw new Error(`Imported Wiki page differs from Notion: ${plan.path}`);
    }
    for (const attachment of plan.attachments ?? []) {
      const reference = page.revision.references[`attachment.${attachment.objectId}`];
      if (reference?.objectId !== attachment.objectId || reference.revision !== attachment.revision)
        throw new Error(`Wiki attachment reference is missing: ${plan.path}/${attachment.media.id}`);
      const saved = await hive("objects.read", { objectId: attachment.objectId, revision: attachment.revision });
      if (saved.object.contractKey !== "wiki/attachment" || saved.object.parentId !== page.object.id || saved.object.ownerObjectId !== page.object.id || saved.revision.contentHash !== attachment.media.contentHash || saved.revision.byteLength !== attachment.media.bytes.length || saved.content.encoding !== "base64" || !Buffer.from(saved.content.value, "base64").equals(attachment.media.bytes))
        throw new Error(`Wiki attachment differs from Notion: ${plan.path}/${attachment.media.id}`);
    }
    const children = await listWikiChildren(hive, page.object.id);
    for (const expected of plan.children) {
      if (
        !children.some(
          (child) =>
            child.objectId ===
            (expected.objectId ?? expected.current?.object.id),
        )
      ) {
        throw new Error(`Imported Wiki child is missing: ${expected.path}`);
      }
    }
  }
}

function argumentsFromCli() {
  const { values } = parseArgs({
    options: {
      "notion-user": { type: "string" },
      "notion-contexts": { type: "string" },
      "hive-base": { type: "string" },
      "notion-token-env": { type: "string", default: "NOTION_API_KEY" },
      "hive-credential-env": { type: "string", default: "IVY_HIVE_CREDENTIAL" },
      apply: { type: "boolean", default: false },
      "clear-first": { type: "boolean", default: false },
    },
  });
  for (const option of ["notion-user", "notion-contexts", "hive-base"]) {
    if (!values[option]) throw new Error(`Missing --${option}`);
  }
  for (const option of ["notion-user", "notion-contexts"]) {
    if (
      !/^[0-9a-f]{8}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{4}-?[0-9a-f]{12}$/i.test(
        values[option],
      )
    ) {
      throw new Error(`--${option} must be a Notion page ID`);
    }
  }
  const notionToken = process.env[values["notion-token-env"]];
  const hiveCredential = process.env[values["hive-credential-env"]];
  if (!notionToken || !hiveCredential)
    throw new Error(
      "Notion and Hive credentials must be set in the selected environment variables",
    );
  return { values, notionToken, hiveCredential };
}

async function main() {
  const { values, notionToken, hiveCredential } = argumentsFromCli();
  const notion = makeNotionClient(notionToken);
  const hive = makeHiveClient(values["hive-base"], hiveCredential);
  const roots = [
    { id: values["notion-user"], title: "User" },
    { id: values["notion-contexts"], title: "Kontexte" },
  ];
  await verifyNotionRoots(notion, roots);
  const sources = [];
  const pageProgress = ({ title, blockCount }) =>
    console.error(`Read ${title} (${blockCount} blocks)`);
  for (const root of roots) {
    sources.push(
      await readNotionTree(notion, root.id, root.title, pageProgress),
    );
  }
  validateSourceTree(sources);
  await preloadMedia(sources, fetch, ({ page, id, byteLength }) =>
    console.error(`Loaded media ${page}/${id} (${byteLength} bytes)`),
  );
  const plans = await planImport(hive, sources);
  if (values["clear-first"]) {
    if (!values.apply)
      throw new Error(
        "--clear-first requires --apply; it permanently deletes all Wiki pages.",
      );
    const deleted = await clearWiki(hive);
    console.log(`Deleted ${deleted} Wiki root pages and their descendants.`);
  }
  const finalPlans = values["clear-first"]
    ? await planImport(hive, sources)
    : plans;
  for (const plan of flatten(finalPlans))
    console.log(`${plan.action.padEnd(6)} ${plan.path}`);
  if (!values.apply) {
    console.log("Dry run only. Pass --apply to write these pages.");
    return;
  }
  await applyImport(hive, finalPlans, ({ path, action }) =>
    console.error(`${action === "skip" ? "Verified" : "Wrote"} ${path}`),
  );
  await verifyImport(hive, finalPlans);
  console.log(
    `Verified ${flatten(finalPlans).length} pages and ${flatten(finalPlans).reduce((sum, plan) => sum + (plan.attachments?.length ?? 0), 0)} attachments; wrote ${flatten(finalPlans).filter((plan) => plan.action !== "skip").length} pages.`,
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
