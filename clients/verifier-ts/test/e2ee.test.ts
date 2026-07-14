import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  e2eeAad,
  sealUnit,
  openUnit,
  openE2eeChannel,
  toBase64,
  fromBase64,
  fromHex,
  REQUEST_CONTEXT,
  RESPONSE_CONTEXT,
  AciError,
  AciFormatError,
  type ReportVerification,
  type WorkloadKeyset,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

/** A passing report verification for the fixture keyset, as the channel gate expects. */
function verified(): ReportVerification {
  return {
    ok: true,
    checks: [],
    workloadKeysetDigest: fx.KEYSET_DIGEST,
    keysetBytes: fx.KEYSET_BYTES,
    keyset: fx.KEYSET,
  };
}

test('§7.1 AAD binds context, model, and the request client key', () => {
  // Response: context || 0x00 || model. Request appends 0x00 || client key hex.
  assert.deepEqual(e2eeAad(RESPONSE_CONTEXT, 'demo-model'), enc.encode('aci.e2ee.v3.response\x00demo-model'));
  assert.deepEqual(
    e2eeAad(REQUEST_CONTEXT, 'demo-model', 'ccdd'),
    enc.encode('aci.e2ee.v3.request\x00demo-model\x00ccdd'),
  );
});

test('§7.1 seal/open round trip; layout and per-unit freshness', async () => {
  const plaintext = enc.encode(fx.REQUEST_BODY);
  const sealed = await sealUnit(fromHex(fx.e2eeKey.publicKeyHex), REQUEST_CONTEXT, 'demo-model', plaintext);
  // ephemeral_public_key (32) || gcm_nonce (12) || ciphertext || tag (16)
  assert.equal(sealed.length, 32 + 12 + plaintext.length + 16);

  const opened = await openUnit(fx.e2eeKey.privateKey, REQUEST_CONTEXT, 'demo-model', sealed);
  assert.deepEqual(opened, plaintext);

  // Fresh ephemeral key and nonce per sealed unit.
  const again = await sealUnit(fromHex(fx.e2eeKey.publicKeyHex), REQUEST_CONTEXT, 'demo-model', plaintext);
  assert.notDeepEqual(again, sealed);
});

test('the AAD binds the sealed unit to its context and envelope model', async () => {
  const sealed = await sealUnit(
    fromHex(fx.e2eeKey.publicKeyHex),
    REQUEST_CONTEXT,
    'demo-model',
    enc.encode('secret'),
  );
  await assert.rejects(() => openUnit(fx.e2eeKey.privateKey, REQUEST_CONTEXT, 'other-model', sealed));
  await assert.rejects(() => openUnit(fx.e2eeKey.privateKey, RESPONSE_CONTEXT, 'demo-model', sealed));
});

test('tampered ciphertext fails AEAD authentication; short units are malformed', async () => {
  const sealed = await sealUnit(
    fromHex(fx.e2eeKey.publicKeyHex),
    REQUEST_CONTEXT,
    'demo-model',
    enc.encode('secret'),
  );
  const bad = sealed.slice();
  bad[50] = bad[50]! ^ 0xff;
  await assert.rejects(() => openUnit(fx.e2eeKey.privateKey, REQUEST_CONTEXT, 'demo-model', bad));
  await assert.rejects(
    () => openUnit(fx.e2eeKey.privateKey, REQUEST_CONTEXT, 'demo-model', sealed.slice(0, 40)),
    AciFormatError,
  );
});

test('§7.2/§7.3 channel: seal a request, service unseals the exact bytes, responses open', async () => {
  const channel = await openE2eeChannel(verified());
  assert.equal(channel.serviceKey.public_key, fx.e2eeKey.publicKeyHex);

  const sealedReq = await channel.seal(fx.REQUEST_BODY);
  assert.deepEqual(sealedReq.headers, {
    'X-E2EE-Version': '3',
    'X-Client-Pub-Key': channel.clientPublicKey,
    'X-Model-Pub-Key': fx.e2eeKey.publicKeyHex,
  });

  // The envelope keeps model in plaintext for routing; sealed_b64 carries the body.
  const envelope = JSON.parse(sealedReq.body) as { model: string; sealed_b64: string };
  assert.equal(envelope.model, 'demo-model');

  // Service side: unseal to the client's exact original request bytes (§7.2).
  // The request AAD binds X-Client-Pub-Key, so the recompute uses it.
  const received = await openUnit(
    fx.e2eeKey.privateKey,
    REQUEST_CONTEXT,
    envelope.model,
    fromBase64(envelope.sealed_b64),
    channel.clientPublicKey,
  );
  assert.equal(dec.decode(received), fx.REQUEST_BODY);

  // Service side: seal the buffered response to X-Client-Pub-Key (§7.3).
  const sealedResp = await sealUnit(
    fromHex(channel.clientPublicKey),
    RESPONSE_CONTEXT,
    envelope.model,
    enc.encode(fx.RESPONSE_BODY),
  );
  const opened = await sealedReq.open(JSON.stringify({ sealed_b64: toBase64(sealedResp) }));
  assert.equal(dec.decode(opened), fx.RESPONSE_BODY);
});

test('§7.3 streaming: each event payload opens; [DONE] passes through', async () => {
  const channel = await openE2eeChannel(verified());
  const sealedReq = await channel.seal(fx.REQUEST_BODY);

  const event = '{"choices":[{"delta":{"content":"hel"}}],"id":"chatcmpl-123"}';
  const sealedEvent = await sealUnit(
    fromHex(channel.clientPublicKey),
    RESPONSE_CONTEXT,
    'demo-model',
    enc.encode(event),
  );
  const opened = await sealedReq.openStreamEvent(JSON.stringify({ sealed_b64: toBase64(sealedEvent) }));
  assert.equal(opened, event);
  assert.equal(await sealedReq.openStreamEvent('[DONE]'), '[DONE]');
});

test('a response sealed under a different envelope model does not open (replay guard, §7.2)', async () => {
  const channel = await openE2eeChannel(verified());
  const sealedReq = await channel.seal(fx.REQUEST_BODY); // model: demo-model
  const sealedResp = await sealUnit(
    fromHex(channel.clientPublicKey),
    RESPONSE_CONTEXT,
    'other-model',
    enc.encode(fx.RESPONSE_BODY),
  );
  await assert.rejects(() => sealedReq.open(JSON.stringify({ sealed_b64: toBase64(sealedResp) })));
});

test('the channel gate refuses unverified reports and unlisted keys', async () => {
  await assert.rejects(() => openE2eeChannel({ ...verified(), ok: false }), AciError);

  const noE2ee = structuredClone(fx.KEYSET) as WorkloadKeyset;
  noE2ee.e2ee_public_keys = [{ key_id: 'other', algo: 'unrecognized-suite', public_key: 'ab'.repeat(32) }];
  await assert.rejects(() => openE2eeChannel({ ...verified(), keyset: noE2ee }), AciError);
});

test('seal requires a string model in the request body (§7.2)', async () => {
  const channel = await openE2eeChannel(verified());
  await assert.rejects(() => channel.seal('{"messages":[]}'), AciFormatError);
  await assert.rejects(() => channel.seal('not json'), AciFormatError);
});
