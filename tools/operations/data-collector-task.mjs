import { readFile } from "node:fs/promises";
import { dirname, resolve, relative, isAbsolute } from "node:path";
const path = resolve(process.argv[2] ?? "");
if (!process.argv[2])
  throw new Error(
    "Usage: node tools/operations/data-collector-task.mjs <template.task.json>",
  );
const task = JSON.parse(await readFile(path, "utf8"));
if (typeof task.script === "string" && task.script.startsWith("@file:")) {
  const source = resolve(dirname(path), task.script.slice(6)),
    rel = relative(dirname(path), source);
  if (rel.startsWith("..") || isAbsolute(rel))
    throw new Error("Script source must be beside the template.");
  task.script = await readFile(source, "utf8");
}
process.stdout.write(JSON.stringify(task, null, 2) + "\n");
