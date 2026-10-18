import { definition, inputs, outputOptions } from "../../services/dashboards/src/schema.ts";
const rows = [];
const base = { title: " A ", html: "<p>x</p>", sources: {} };
function add(schema, input) {
  const parsed = ({ definition, output: outputOptions, ...inputs })[schema].safeParse(input);
  rows.push({ schema, input, ...(parsed.success ? { value: parsed.data } : { issues: parsed.error.issues, message: parsed.error.message }) });
}
const values = [null, false, 0, -1, 1.5, 9007199254740992, "", "x", [], {}];
add("definition", base);
for (const schema of ["definition", "output", ...Object.keys(inputs)]) for (const v of values) add(schema, v);
for (const field of Object.keys({ ...base, refreshSeconds: 5, metadata: {}, imageToken: null })) {
  const missing = { ...base }; delete missing[field]; add("definition", missing);
  for (const value of values) add("definition", { ...base, [field]: value });
}
for (const title of ["\ufeff \tA\u2029", "\u0085A\u0085", "😀".repeat(200), "😀".repeat(201), "\ud800", "  "]) add("definition", { ...base, title });
for (const refreshSeconds of [0, 1, 4, 5, 86400, 86401]) add("definition", { ...base, refreshSeconds });
for (const imageToken of ["a".repeat(31), "a".repeat(32), "_".repeat(128), "a".repeat(129), "a".repeat(32) + "\n"]) add("definition", { ...base, imageToken });
for (const source of [...values, { objectId: "" }, { objectId: "x", extra: 1 }, { contractKey: "x" }, { contractKey: "", limit: 0 }, { objectId: "x", contractKey: "y" }, { contractKey: "x", limit: 101 }, { contractKey: "x", limit: 1.2 }]) add("definition", { ...base, sources: { feed: source } });
for (const sources of [{ "1bad": { objectId: "x" } }, { ["a".repeat(65)]: { objectId: "x" } }, Object.fromEntries(Array.from({ length: 21 }, (_, i) => ["a" + i, { objectId: "x" }])), JSON.parse('{"__proto__":null}')]) add("definition", { ...base, sources });
for (const metadata of [{ refreshSeconds: 2 }, { a: "x".repeat(7978) }, { a: "x".repeat(7979) }, { a: "\ud800" }, JSON.parse('{"__proto__":1,"a":{"__proto__":2,"b":3},"z":[{"__proto__":4}]}')]) add("definition", { ...base, metadata });
for (const input of [{}, { value: base }, { value: base, id: "id" }, { value: base, expectedRevision: 1 }, { value: base, id: "id", expectedRevision: 1 }, { value: base, id: "", expectedRevision: 0 }, { value: { ...base, extra: 1 }, id: "id" }]) add("save", input);
for (const schema of ["list", "read", "save", "delete", "data", "render", "image", "output"]) {
  const valid = { list: {}, read: { id: "x" }, save: { value: base }, delete: { id: "x", expectedRevision: 1 }, data: { id: "x" }, render: { id: "x" }, image: { id: "x", token: "token" }, output: {} }[schema];
  add(schema, valid); add(schema, { ...valid, extra: 1, other: 2 });
  for (const key of Object.keys(valid)) for (const value of values) add(schema, { ...valid, [key]: value });
}
for (const field of ["width", "height", "colorMode"]) for (const value of [...values, 99, 100, 4096, 4097, "grayscale", "monochrome"]) add("render", { id: "x", [field]: value });
add("output", { width: 4096, height: 4096 });
add("output", { width: 4097, height: 4097, extra: 1 });
add("definition", { ...base, extra: 1, other: 2 });
add("definition", JSON.parse('{"title":"A","html":"x","sources":{},"__proto__":1,"x\\ud800":2}'));
for (const length of [null, false, true, "", "0x0", "0b1", "0o3", "200", "201", "bad", [], [1], {}, -1, 300]) add("definition", { ...base, title: { length } });
add("definition", { ...base, metadata: { constructor: 1, toString: 2, "1": true, "0": false } });
add("definition", { ...base, sources: { "feed\n": { objectId: "x" } } });
add("read", { id: "x", "10": 1, "2": 2, "0": 3, "quote\"": 4 });
for (const rawInput of ['{"id":"x","10":1,"2":2,"0":3}', '{"title":"A","html":"x","sources":{"10":{},"2":null}}', '{"title":"A","html":"x","sources":{},"metadata":{"10":1,"2":2,"nested":{"10":1,"2":2}}}']) {
  add(rawInput.includes('"title"') ? "definition" : "read", JSON.parse(rawInput));
  rows.at(-1).rawInput = rawInput;
}
process.stdout.write(JSON.stringify(rows));
