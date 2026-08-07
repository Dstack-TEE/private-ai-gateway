// @phala/dcap-qvl parses DER/ASN.1 with the `buffer` package and expects a
// global `Buffer`; esbuild injects this into the browser bundle. All other
// crypto is Web Crypto (the package maps node `crypto` and `node-fetch` away in
// its browser field, using the platform's global `fetch`).
import { Buffer } from 'buffer';
globalThis.Buffer = globalThis.Buffer || Buffer;
