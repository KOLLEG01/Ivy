import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { taskBoardFixture } from "./fixtures/task-board-fixture.mjs";

const fields = (title) => ({
  title,
  description: "",
  acceptanceCriteria: [],
  category: null,
  control: "user",
  priority: 2,
  executionRequirement: null,
  workspaceRequirement: { kind: "task_workspace" },
  dependencies: [],
  userContact: "ticket",
  nextReviewAt: null,
  dueAt: null,
});

test(
  "Task links and Wiki references use canonical routes, and task Markdown fields preview and render",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    await publishUi(f.client, {
      directory: resolve("dist/apps/wiki-ui"),
      definition: JSON.parse(await readFile("ui/wiki-ui/ui.json", "utf8")),
      mutationId: await newOperationId(f.client),
      expectedReleaseId: null,
    });
    const wiki = await f.client.request("objects.write", {
      mutationId: await newOperationId(f.client),
      contractVersion: "1.0.0",
      references: {},
      create: {
        contractKey: "wiki/page",
        parentId: null,
        ownerObjectId: null,
        name: "Reference note",
      },
      content: {
        encoding: "text",
        value: "# Reference note\n\nShared from a task.",
      },
    });
    await f.context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await f.open("#/new?node=browser-task-board");
    await f.page.getByLabel("Title", { exact: true }).fill("Linked task");
    const description = f.page.getByRole("textbox", { name: "Description", exact: true });
    await description.fill("Read ");
    await f.page
      .getByRole("button", { name: "Insert Wiki page" })
      .first()
      .click();
    const picker = f.page.getByRole("dialog", { name: "Insert Wiki page" });
    await picker.getByLabel("Search Wiki pages").fill("Reference note");
    await picker.getByRole("button", { name: "Reference note" }).click();
    const wikiUrl = f.base + "/ui/wiki-ui/#/page?id=" + wiki.object.id;
    await expect(
      description.getByRole("link", { name: "Reference note" }),
    ).toHaveAttribute("href", wikiUrl);
    // An image added before the Task exists is uploaded once the Task is created.
    const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9L+OQAAAAASUVORK5CYII=", "base64");
    await f.page.locator("input[type=file]").first().setInputFiles({ name: "pixel.png", mimeType: "image/png", buffer: png });
    await expect(description.getByRole("img", { name: "pixel.png" })).toBeVisible();
    await f.page
      .getByRole("button", { name: "Add acceptance criteria" })
      .click();
    await f.page.getByRole("textbox", { name: "Acceptance criteria", exact: true }).click();
    await f.page.keyboard.type("**Review** the reference");
    await expect(
      f.page.locator("strong").filter({ hasText: "Review" }),
    ).toBeVisible();
    await f.page.getByRole("button", { name: "Create task" }).click();
    await expect(f.page).toHaveURL(/#\/tasks\?id=/);
    const id = new URLSearchParams(
      new URL(f.page.url()).hash.split("?")[1],
    ).get("id");
    assert.ok(id);
    assert.equal(
      new URL(f.page.url()).hash,
      "#/tasks?id=" + id + "&node=browser-task-board",
    );
    const created = await f.task(id);
    assert.deepEqual(created.fields.acceptanceCriteria, [
      "**Review** the reference",
    ]);
    assert.equal(created.attachments.length, 1);
    assert.ok(created.fields.description.includes("attachment:" + created.attachments[0].attachmentId));
    await expect(f.page.getByRole("button", { name: "Enlarge pixel.png" })).toBeVisible();
    await expect(
      f.page.getByRole("link", { name: "Reference note" }),
    ).toHaveAttribute("href", wikiUrl);
    await expect(
      f.page.locator("strong").filter({ hasText: "Review" }),
    ).toBeVisible();
    await f.page.getByRole("button", { name: "More actions" }).click();
    await f.page.getByRole("menuitem", { name: "Copy task link" }).click();
    assert.equal(
      await f.page.evaluate(() => navigator.clipboard.readText()),
      f.url + "#/tasks?id=" + id + "&node=browser-task-board",
    );
    await f.page.getByRole("textbox", { name: "Comment" }).click();
    await f.page.keyboard.type("A **bold** comment");
    await f.page.getByRole("button", { name: "Add comment" }).click();
    await expect.poll(async () => (await f.task(id)).comments.at(-1)?.body).toBe("A **bold** comment");
    await expect(
      f.page.locator("strong").filter({ hasText: "bold" }),
    ).toBeVisible();
    await f.page.goto(wikiUrl);
    await expect(
      f.page.getByRole("heading", { name: "Reference note" }).first(),
    ).toBeVisible();
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);

test(
  "Task attachments expand below comments, preview Markdown in a dialog, show media inline and download other files",
  { timeout: 180000 },
  async (t) => {
    const f = await taskBoardFixture(t);
    const task = await f.invoke({
      action: "create",
      fields: fields("Attached files"),
    });
    const id = task.task.objectId;
    const png = Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/lYQAAAAASUVORK5CYII=",
      "base64",
    );
    const files = [
      {
        filename: "notes.md",
        mediaType: "application/octet-stream",
        bytes: Buffer.from("# Attachment markdown\n\nReadable in the task."),
      },
      { filename: "pixel.png", mediaType: "image/png", bytes: png },
      {
        filename: "clip.mp4",
        mediaType: "video/mp4",
        bytes: Buffer.from("video fixture"),
      },
      {
        filename: "bundle.zip",
        mediaType: "application/zip",
        bytes: Buffer.from("download fixture"),
      },
    ];
    for (const file of files) {
      const current = await f.client.request("objects.read", { objectId: id });
      await f.invoke({
        action: "uploadAttachment",
        taskId: id,
        expectedRevision: current.object.currentRevision,
        attachmentId: file.filename,
        filename: file.filename,
        mediaType: file.mediaType,
        bytesBase64: file.bytes.toString("base64"),
      });
    }
    await f.open("#/tasks?id=" + id + "&node=browser-task-board");
    await expect(
      f.page.getByRole("button", { name: "Attachments (4)" }),
    ).toBeVisible();
    await expect(f.page.getByText("notes.md")).toHaveCount(0);
    await f.page.getByRole("button", { name: "Attachments (4)" }).click();
    await expect(f.page.getByText("notes.md")).toBeVisible();
    await f.page.getByRole("button", { name: "Open notes.md" }).click();
    const dialog = f.page.getByRole("dialog", { name: "notes.md" });
    await expect(
      dialog.getByRole("heading", { name: "Attachment markdown" }),
    ).toBeVisible();
    await f.page.keyboard.press("Escape");
    await expect(f.page.getByRole("img", { name: "pixel.png" })).toBeVisible();
    await expect(f.page.locator('video[aria-label="clip.mp4"]')).toBeVisible();
    const [download] = await Promise.all([
      f.page.waitForEvent("download"),
      f.page.getByRole("button", { name: "Download" }).last().click(),
    ]);
    assert.equal(download.suggestedFilename(), "bundle.zip");
    assert.deepEqual(await readFile(await download.path()), files[3].bytes);
    await f.page.setViewportSize({ width: 390, height: 844 });
    assert.equal(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth > innerWidth,
      ),
      false,
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
  },
);
