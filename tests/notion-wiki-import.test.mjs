import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash } from "node:crypto";
import {
  applyImport,
  clearWiki,
  planImport,
  preloadMedia,
  readNotionTree,
  renderBlocks,
  validateSourceTree,
  verifyImport,
  verifyNotionRoots,
} from "../tools/operations/import-notion-wiki.mjs";

const ids = {
  workspace: "00000000-0000-0000-0000-000000000001",
  user: "00000000-0000-0000-0000-000000000002",
  contexts: "00000000-0000-0000-0000-000000000003",
  child: "00000000-0000-0000-0000-000000000004",
  grandchild: "00000000-0000-0000-0000-000000000005",
};

function text(content, url) {
  return {
    type: "text",
    plain_text: content,
    text: { content, ...(url ? { link: { url } } : {}) },
    ...(url ? { href: url } : {}),
  };
}

function block(id, type, content) {
  return { id, type, has_children: false, [type]: content };
}

test("reads paginated Notion descendants and omits only child-page navigation", async () => {
  const source = new Map([
    [
      `/v1/pages/${ids.workspace}`,
      {
        properties: { title: { type: "title", title: [text("IvyWorkspace")] } },
      },
    ],
    [
      `/v1/pages/${ids.user}`,
      {
        parent: { type: "page_id", page_id: ids.workspace },
        properties: { title: { type: "title", title: [text("User")] } },
      },
    ],
    [
      `/v1/pages/${ids.contexts}`,
      {
        parent: { type: "page_id", page_id: ids.workspace },
        properties: { title: { type: "title", title: [text("Kontexte")] } },
      },
    ],
    [
      `/v1/blocks/${ids.user}/children?page_size=100`,
      {
        results: [
          block("a", "heading_2", { rich_text: [text("Beschreibung")] }),
          block("b", "paragraph", { rich_text: [text("Inhalt")] }),
        ],
        has_more: true,
        next_cursor: "next",
      },
    ],
    [
      `/v1/blocks/${ids.user}/children?page_size=100&start_cursor=next`,
      {
        results: [
          block("c", "heading_2", { rich_text: [text("Unterseiten")] }),
          block("d", "paragraph", {
            rich_text: [
              text(
                "Projekt",
                `https://www.notion.so/${ids.child.replaceAll("-", "")}`,
              ),
            ],
          }),
          block(ids.child, "child_page", { title: "Projekt" }),
        ],
        has_more: false,
        next_cursor: null,
      },
    ],
    [
      `/v1/blocks/${ids.child}/children?page_size=100`,
      {
        results: [
          block("e", "paragraph", { rich_text: [text("Projektinhalt")] }),
          block(ids.grandchild, "child_page", { title: "Notiz" }),
        ],
        has_more: false,
        next_cursor: null,
      },
    ],
    [
      `/v1/blocks/${ids.grandchild}/children?page_size=100`,
      {
        results: [block("f", "paragraph", { rich_text: [text("Tiefe")] })],
        has_more: false,
        next_cursor: null,
      },
    ],
  ]);
  const get = async (path) => {
    assert.ok(source.has(path), `unexpected Notion request ${path}`);
    return structuredClone(source.get(path));
  };
  await verifyNotionRoots(get, [
    { id: ids.user, title: "User" },
    { id: ids.contexts, title: "Kontexte" },
  ]);
  const tree = await readNotionTree(get, ids.user, "User");
  assert.equal(tree.markdown, "## Beschreibung\n\nInhalt");
  assert.equal(tree.children[0].title, "Projekt");
  assert.equal(tree.children[0].markdown, "Projektinhalt");
  assert.equal(tree.children[0].children[0].markdown, "Tiefe");
});

test("copies an inline image into an owned Wiki attachment and verifies its bytes", async () => {
  const imageId = "00000000-0000-0000-0000-000000000099";
  const source = {
    title: "User", children: [],
    markdown: renderBlocks([block(imageId, "image", { type: "file", file: { url: "https://files.example.test/picture.png" }, caption: [] })], ids.user),
    media: [{ id: imageId, type: "image", url: "https://files.example.test/picture.png", name: null }],
  };
  const bytes = Buffer.from("89504e470d0a1a0a00000000", "hex");
  await preloadMedia([source], async () => ({ ok: true, headers: new Headers({ "Content-Length": String(bytes.length) }), arrayBuffer: async () => bytes }));
  assert.match(source.markdown, /notion-media:/);
  assert.equal(source.media[0].contentHash, `sha256:${createHash("sha256").update(bytes).digest("hex")}`);
  let page = null, attachment = null;
  const hive = async (method, params) => {
    if (method === "system.status") return { runtimeEpoch: "test" };
    if (method === "wiki.list") return { items: page && params.parentId === null ? [{ objectId: "page-1", values: { "object.name": "User" } }] : [], nextCursor: null };
    if (method === "objects.list") return { items: attachment ? [{ ...attachment.object, name: source.media[0].fileName }] : [], nextCursor: null };
    if (method === "wiki.create") {
      page = { object: { id: "page-1", path: "/User" }, revision: { revision: 1, references: {} }, content: { encoding: "text", value: params.markdown } };
      return page;
    }
    if (method === "wiki.read") return page;
    if (method === "objects.write" && params.create?.contractKey === "wiki/attachment") {
      assert.equal(params.create.parentId, "page-1");
      assert.equal(params.create.ownerObjectId, "page-1");
      assert.ok(Buffer.from(params.content.value, "base64").equals(bytes));
      attachment = { object: { id: "attachment-1", contractKey: "wiki/attachment", parentId: "page-1", ownerObjectId: "page-1" }, revision: { revision: 1, contentHash: source.media[0].contentHash, byteLength: bytes.length }, content: params.content };
      return attachment;
    }
    if (method === "objects.write") {
      assert.equal(params.objectId, "page-1");
      assert.equal(params.expectedRevision, 1);
      page = { ...page, revision: { revision: 2, references: params.references }, content: params.content };
      return page;
    }
    if (method === "objects.read") return attachment;
    throw new Error(`unexpected Hive method ${method}`);
  };
  const plans = await planImport(hive, [source]);
  await applyImport(hive, plans);
  await verifyImport(hive, plans);
  assert.equal(page.content.value, "![image](#/page?id=attachment-1&revision=1)");
  assert.deepEqual(page.revision.references["attachment.attachment-1"], { objectId: "attachment-1", revision: 1 });
  const repeated = await planImport(hive, [source]);
  assert.equal(repeated[0].action, "skip");
  await applyImport(hive, repeated);
  await verifyImport(hive, repeated);
});

test("preserves nested list indentation and rejects unhandled content blocks", () => {
  const list = block("a", "bulleted_list_item", {
    rich_text: [text("Parent")],
  });
  list.children = [
    block("b", "bulleted_list_item", { rich_text: [text("Child")] }),
  ];
  assert.equal(renderBlocks([list], ids.user), "- Parent\n  - Child");
  assert.equal(
    renderBlocks(
      [block("heading", "heading_4", { rich_text: [text("Detail")] })],
      ids.user,
    ),
    "#### Detail",
  );
  const table = block("table", "table", {});
  table.children = [
    block("row-1", "table_row", { cells: [[text("Name")], [text("Wert")]] }),
    block("row-2", "table_row", { cells: [[text("A")], [text("B")]] }),
  ];
  assert.equal(
    renderBlocks([table], ids.user),
    "| Name | Wert |\n| --- | --- |\n| A | B |",
  );
  assert.throws(
    () => renderBlocks([block("c", "unknown_type", {})], ids.user),
    /Unsupported Notion block/,
  );
});

test("rejects a page the Wiki cannot store before any import", () => {
  assert.throws(
    () =>
      validateSourceTree([
        {
          title: "User",
          markdown: "",
          children: [{ title: "A/B", markdown: "", children: [] }],
        },
      ]),
    /title or depth is invalid/,
  );
});

test("updates existing Wiki pages, creates missing descendants, and verifies every page", async () => {
  const records = new Map([
    [
      "root",
      {
        id: "root",
        parentId: null,
        name: "User",
        path: "/User",
        markdown: "old",
        revision: 1,
      },
    ],
  ]);
  const hive = async (method, params) => {
    if (method === "system.status") return { runtimeEpoch: "test" };
    if (method === "wiki.read") {
      const value = [...records.values()].find(
        (item) => item.id === params.objectId || item.path === params.path,
      );
      assert.ok(value, `missing Wiki page ${JSON.stringify(params)}`);
      return {
        object: { id: value.id, path: value.path },
        revision: { revision: value.revision },
        content: { encoding: "text", value: value.markdown },
      };
    }
    if (method === "wiki.list") {
      return {
        items: [...records.values()]
          .filter((item) => item.parentId === params.parentId)
          .map((item) => ({
            objectId: item.id,
            values: { "object.name": item.name },
          })),
        nextCursor: null,
      };
    }
    if (method === "wiki.update") {
      const value = records.get(params.objectId);
      assert.equal(value.revision, params.expectedRevision);
      value.markdown = params.markdown;
      value.revision++;
      return { object: { id: value.id } };
    }
    if (method === "wiki.create") {
      const id = `new-${records.size}`;
      const parent = records.get(params.parentId);
      records.set(id, {
        id,
        parentId: params.parentId,
        name: params.title,
        path: `${parent.path}/${params.title}`,
        markdown: params.markdown,
        revision: 1,
      });
      return { object: { id } };
    }
    throw new Error(`unexpected Hive method ${method}`);
  };
  const sources = [
    {
      title: "User",
      markdown: "new",
      children: [{ title: "Projekt", markdown: "text", children: [] }],
    },
  ];
  const plans = await planImport(hive, sources);
  assert.equal(plans[0].action, "update");
  assert.equal(plans[0].children[0].action, "create");
  await applyImport(hive, plans);
  await verifyImport(hive, plans);
  assert.equal(records.size, 2);
  assert.equal(records.get("root").markdown, "new");
});

test("creates Wiki roots after a clear and permanently removes each Wiki subtree", async () => {
  const source = [{ title: "User", markdown: "text", children: [] }];
  const emptyHive = async (method) => {
    assert.equal(method, "wiki.list");
    return { items: [], nextCursor: null };
  };
  assert.equal((await planImport(emptyHive, source))[0].action, "create");

  const roots = [{ objectId: "root-user" }, { objectId: "root-contexts" }];
  let remaining = [...roots];
  const calls = [];
  const hive = async (method, params) => {
    calls.push({ method, params });
    if (method === "wiki.list") return { items: remaining, nextCursor: null };
    if (method === "system.status") return { runtimeEpoch: "test" };
    if (method === "wiki.read") return { revision: { revision: 2 } };
    if (method === "objects.delete") {
      remaining = remaining.filter((root) => root.objectId !== params.objectId);
      return { deleted: true };
    }
    throw new Error(`unexpected Hive method ${method}`);
  };
  assert.equal(await clearWiki(hive), 2);
  assert.deepEqual(remaining, []);
  assert.deepEqual(
    calls
      .filter((call) => call.method === "objects.delete")
      .map((call) => call.params.objectId),
    ["root-user", "root-contexts"],
  );
});
