import assert from "node:assert/strict";
import { generateKeyPairSync, randomBytes, webcrypto } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { runInNewContext } from "node:vm";

const html = await readFile(
  new URL(
    "../docs/examples/data-collector/social/oauth-callback.html",
    import.meta.url,
  ),
  "utf8",
);
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];

async function receive(fragment, language = "en") {
  const nodes = Object.fromEntries(
    ["h1", "label", "#note", "#status", "#oauth-response"].map((id) => [
      id,
      { textContent: "", value: "" },
    ]),
  );
  const location = { hash: `#${fragment}`, pathname: "/static/callback.html" };
  const document = { documentElement: {}, querySelector: (id) => nodes[id] };
  let ready;
  document.addEventListener = (name, callback) => {
    assert.equal(name, "DOMContentLoaded");
    ready = callback;
  };
  runInNewContext(script, {
    location,
    document,
    navigator: { language },
    history: {
      replaceState: (_state, _title, path) => {
        assert.equal(path, location.pathname);
        location.hash = "";
      },
    },
    URLSearchParams,
    TextEncoder,
    Uint8Array,
    atob,
    btoa,
    crypto: webcrypto,
  });
  assert.equal(location.hash, "", "Remove credentials before preparing the UI");
  await ready();
  return { nodes, document };
}

test("Social OAuth callback exposes only ciphertext and preserves the state for private verification", async () => {
  const { publicKey, privateKey } = generateKeyPairSync("rsa", {
    modulusLength: 2048,
  });
  const nonce = randomBytes(32).toString("hex");
  const state = Buffer.from(
    JSON.stringify({
      version: 1,
      nonce,
      key: publicKey.export({ format: "jwk" }),
    }),
  ).toString("base64url");
  const token = "fixture-secret-access-token";
  const { nodes, document } = await receive(
    new URLSearchParams({
      access_token: token,
      state,
      expires_in: "3600",
    }).toString(),
    "de-DE",
  );
  assert.equal(document.documentElement.lang, "de");
  assert.equal(JSON.stringify(nodes).includes(token), false);
  const packet = JSON.parse(nodes["#oauth-response"].value);
  assert.equal(packet.nonce, nonce);
  const recipient = await webcrypto.subtle.importKey(
    "jwk",
    privateKey.export({ format: "jwk" }),
    { name: "RSA-OAEP", hash: "SHA-256" },
    false,
    ["decrypt"],
  );
  const rawKey = await webcrypto.subtle.decrypt(
    "RSA-OAEP",
    recipient,
    Buffer.from(packet.key, "base64"),
  );
  const key = await webcrypto.subtle.importKey(
    "raw",
    rawKey,
    "AES-GCM",
    false,
    ["decrypt"],
  );
  const plain = await webcrypto.subtle.decrypt(
    { name: "AES-GCM", iv: Buffer.from(packet.iv, "base64") },
    key,
    Buffer.from(packet.cipher, "base64"),
  );
  assert.deepEqual(JSON.parse(new TextDecoder().decode(plain)), {
    access_token: token,
    state,
    expires_in: "3600",
  });
});

test("Social OAuth callback scrubs malformed sessions without exposing tokens or provider errors", async () => {
  for (const response of [
    { access_token: "fixture-secret-access-token" },
    { access_token: "fixture-secret-access-token", state: "malformed" },
    { error: "<script>untrusted provider error</script>", state: "malformed" },
  ]) {
    const { nodes } = await receive(new URLSearchParams(response).toString());
    assert.equal(nodes["#oauth-response"].value, "");
    assert.equal(
      nodes["#status"].textContent,
      "The provider did not complete authorization.",
    );
    assert.equal(
      JSON.stringify(nodes).includes(response.access_token ?? response.error),
      false,
    );
  }
});
