// Independent OpenSSL verification/decryption; no original backend or push library.
import assert from 'node:assert/strict';
import {createECDH, createDecipheriv, createPublicKey, hkdfSync, verify} from 'node:crypto';

const input = JSON.parse(await new Response(process.stdin).text());
const receiver = createECDH('prime256v1');
receiver.setPrivateKey(Buffer.from(input.receiverPrivate, 'base64url'));
const receiverPublic = receiver.getPublicKey();
const derive = (secret, salt, info, length) =>
  Buffer.from(hkdfSync('sha256', secret, salt, info, length));

function decrypt(body) {
  if (body.length === 0) return Buffer.alloc(0);
  const salt = body.subarray(0, 16), rs = body.readUInt32BE(16), keyLength = body[20];
  assert.equal(rs, 4096);
  assert.equal(keyLength, 65);
  const sender = body.subarray(21, 21 + keyLength);
  assert.equal(sender[0], 4);
  const ikm = derive(receiver.computeSecret(sender), Buffer.from(input.auth, 'base64url'),
    Buffer.concat([Buffer.from('WebPush: info\0'), receiverPublic, sender]), 32);
  const key = derive(ikm, salt, Buffer.from('Content-Encoding: aes128gcm\0'), 16);
  const nonce = derive(ikm, salt, Buffer.from('Content-Encoding: nonce\0'), 12);
  const chunks = [];
  for (let offset = 21 + keyLength, sequence = 0n; offset < body.length; offset += rs, ++sequence) {
    const record = body.subarray(offset, offset + rs), iv = Buffer.from(nonce);
    iv.writeBigUInt64BE(iv.readBigUInt64BE(4) ^ sequence, 4);
    const cipher = createDecipheriv('aes-128-gcm', key, iv);
    cipher.setAuthTag(record.subarray(-16));
    const plain = Buffer.concat([cipher.update(record.subarray(0, -16)), cipher.final()]);
    let end = plain.length;
    while (end > 0 && plain[end - 1] === 0) --end;
    assert.equal(plain[end - 1], offset + record.length === body.length ? 2 : 1);
    chunks.push(plain.subarray(0, end - 1));
  }
  return Buffer.concat(chunks);
}

console.log(JSON.stringify(input.requests.map(request => {
  try {
    const {headers} = request, body = Buffer.from(request.body, 'base64');
    assert.equal(headers.TTL, '3600');
    assert.equal(headers.Urgency, 'normal');
    assert.equal(headers['Content-Length'], String(body.length));
    assert.equal(headers['Content-Encoding'], body.length ? 'aes128gcm' : undefined);
    assert.equal(headers['Content-Type'], body.length ? 'application/octet-stream' : undefined);
    const match = /^vapid t=([^,]+), k=([A-Za-z0-9_-]+)$/.exec(headers.Authorization);
    assert.ok(match);
    const [header, claims, signature] = match[1].split('.');
    assert.deepEqual(JSON.parse(Buffer.from(header, 'base64url')), {typ: 'JWT', alg: 'ES256'});
    const point = Buffer.from(match[2], 'base64url');
    assert.equal(point.length, 65);
    assert.equal(point[0], 4);
    const key = createPublicKey({format: 'jwk', key: {kty: 'EC', crv: 'P-256',
      x: point.subarray(1, 33).toString('base64url'), y: point.subarray(33).toString('base64url')}});
    assert.ok(verify('sha256', Buffer.from(`${header}.${claims}`),
      {key, dsaEncoding: 'ieee-p1363'}, Buffer.from(signature, 'base64url')));
    return {ok: true, claims: JSON.parse(Buffer.from(claims, 'base64url')),
      plaintext: decrypt(body).toString('base64')};
  } catch (error) {
    return {ok: false, error: error.message};
  }
})));
