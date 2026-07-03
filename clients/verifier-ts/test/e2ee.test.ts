import { test } from 'node:test';
import assert from 'node:assert/strict';
import { requestAad, requestAadString, responseAad, responseAadString } from '../src/index.js';
import * as fx from './fixtures.js';

test('§7.3 request AAD matches the vector (string and bytes)', () => {
  const params = {
    algo: fx.E2EE_ALGO,
    model: fx.E2EE_MODEL,
    field: 'messages.0.content',
    nonce: fx.E2EE_NONCE,
    ts: fx.E2EE_TIMESTAMP,
  };
  assert.equal(requestAadString(params), fx.REQUEST_AAD);
  assert.equal(new TextDecoder().decode(requestAad(params)), fx.REQUEST_AAD);
});

test('§7.3 response AAD matches the vector (adds response id)', () => {
  const params = {
    algo: fx.E2EE_ALGO,
    model: fx.E2EE_MODEL,
    id: fx.RESPONSE_ID,
    field: 'choices.0.message.content',
    nonce: fx.E2EE_NONCE,
    ts: fx.E2EE_TIMESTAMP,
  };
  assert.equal(responseAadString(params), fx.RESPONSE_AAD);
  assert.equal(new TextDecoder().decode(responseAad(params)), fx.RESPONSE_AAD);
});
