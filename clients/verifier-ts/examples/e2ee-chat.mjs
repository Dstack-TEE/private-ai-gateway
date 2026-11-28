// E2EE v2.1 chat through the OpenAI SDK.
//
//   npm install @phala/aci-verifier openai
//   GATEWAY_URL=https://gateway.example API_URL=https://api.example/v1 \
//   COMPOSE_HASHES=<reviewed sha256(app_compose)>,... API_KEY=... MODEL=... \
//   node e2ee-chat.mjs
//
// GATEWAY_URL is the attested gateway; API_URL is whatever the requests go
// through, such as your own API in front of it.
import OpenAI from 'openai';
import { createE2eeFetch, E2EE_V2_1_X25519, verifyService } from '@phala/aci-verifier';

const { GATEWAY_URL, API_URL, COMPOSE_HASHES, API_KEY, MODEL } = process.env;
if (!COMPOSE_HASHES) throw new Error('set COMPOSE_HASHES to the gateway releases you accept');

// 1. Verify the gateway against the releases you reviewed. Behind a relay you
//    cannot see the gateway's TLS certificate, so bind the channel to its
//    attested E2EE v2.1 key instead (ACI §9.1(6)). Without the pin, any genuine
//    dstack workload would pass, including one run by whoever sits between
//    you and the gateway.
const transcript = await verifyService(GATEWAY_URL, {
  acceptedComposeHashes: COMPOSE_HASHES.split(','),
  channel: { e2ee: E2EE_V2_1_X25519 },
});
if (!transcript.verdict.verified) throw new Error(transcript.verdict.line);

// 2. Encrypt every request and decrypt every reply at the transport layer.
//    createE2eeFetch refuses a transcript that is not VERIFIED this way.
const client = new OpenAI({
  baseURL: API_URL,
  apiKey: API_KEY,
  fetch: createE2eeFetch({ transcript }),
});

// 3. Use the SDK as usual. Buffered and streamed replies both work.
const reply = await client.chat.completions.create({
  model: MODEL,
  messages: [{ role: 'user', content: 'Say hello in one word.' }],
});
console.log(reply.choices[0].message.content);

const stream = await client.chat.completions.create({
  model: MODEL,
  stream: true,
  messages: [{ role: 'user', content: 'Count to five.' }],
});
for await (const chunk of stream) process.stdout.write(chunk.choices[0]?.delta?.content ?? '');
process.stdout.write('\n');
