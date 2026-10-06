import test from "node:test";
import assert from "node:assert/strict";
import { attachmentView } from "../ui/task-board-ui/src/attachment-view.js";

test("Task attachments choose a safe readable view or a download", () => {
  assert.equal(
    attachmentView("notes.md", "application/octet-stream"),
    "markdown",
  );
  assert.equal(attachmentView("notes.txt", "text/markdown"), "markdown");
  assert.equal(attachmentView("photo.png", "image/png"), "image");
  assert.equal(attachmentView("clip.mp4", "video/mp4"), "video");
  assert.equal(attachmentView("archive.mkv", "video/x-matroska"), "download");
  assert.equal(attachmentView("report.pdf", "application/pdf"), "pdf");
  assert.equal(attachmentView("data.json", "application/json"), "text");
  assert.equal(attachmentView("bundle.zip", "application/zip"), "download");
  assert.equal(attachmentView("active.svg", "image/svg+xml"), "download");
});
