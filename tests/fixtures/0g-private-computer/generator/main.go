// Command generator writes interop.json: a chat request and buffered and
// streamed responses sealed by 0G's own protocol library (pinned in go.mod),
// with broker-style §8 signatures, for PAG's native implementation to open and
// verify. HPKE randomness differs per run. Regenerate: go run . > ../interop.json
package main

import (
	"bytes"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/0gfoundation/0g-pc-e2ee/protocol/crypto"
	"github.com/0gfoundation/0g-pc-e2ee/protocol/proof"
	"github.com/0gfoundation/0g-pc-e2ee/protocol/wire"
	"github.com/decred/dcrd/dcrec/secp256k1/v4"
	"github.com/decred/dcrd/dcrec/secp256k1/v4/ecdsa"
	"golang.org/x/crypto/sha3"
)

var b64 = base64.RawURLEncoding

func keccak(parts ...[]byte) []byte {
	h := sha3.NewLegacyKeccak256()
	for _, p := range parts {
		h.Write(p)
	}
	return h.Sum(nil)
}

type signer struct {
	priv *secp256k1.PrivateKey
	addr string
}

func newSigner() signer {
	priv := secp256k1.PrivKeyFromBytes(bytes.Repeat([]byte{0x22}, 32))
	raw := priv.PubKey().SerializeUncompressed()
	return signer{priv: priv, addr: "0x" + hex.EncodeToString(keccak(raw[1:])[12:])}
}

// sign mirrors the broker: go-ethereum crypto.Sign(accounts.TextHash(text))
// with the recovery id normalized to 27/28.
func (s signer) sign(text string) proof.ChatSignature {
	digest := keccak([]byte("\x19Ethereum Signed Message:\n"+strconv.Itoa(len(text))), []byte(text))
	compact := ecdsa.SignCompact(s.priv, digest, false) // [V||R||S], V = 27 + recid
	rsv := append(append([]byte{}, compact[1:65]...), compact[0])
	return proof.ChatSignature{
		Text:           text,
		Signature:      "0x" + hex.EncodeToString(rsv),
		SigningAddress: s.addr,
		SigningAlgo:    "ecdsa",
	}
}

func raw(v any) json.RawMessage {
	b, err := json.Marshal(v)
	must(err)
	return b
}

func must(err error) {
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func main() {
	encPriv, encPub, err := crypto.GenerateRecipientKey()
	must(err)
	ephPriv, ephPub, err := crypto.GenerateRecipientKey()
	must(err)
	sg := newSigner()

	// Cleartext fields deliberately exercise JCS: a float, an integer beyond
	// 2^53, HTML-sensitive and non-ASCII strings, and object keys whose order
	// differs from their canonical order.
	request := wire.Request{
		"model":       raw("glm-5.3"),
		"temperature": json.RawMessage(`0.7`),
		"max_tokens":  json.RawMessage(`64`),
		"seed":        json.RawMessage(`12345678901234567890`),
		"stop":        raw([]string{"</s>", "fin <&> é"}),
		"logit_bias":  json.RawMessage(`{"50256":-100,"1000":5}`),
		"stream":      json.RawMessage(`true`),
		"messages":    json.RawMessage(`[{"role":"system","content":"Be brief."},{"role":"user","content":"Say ok <now> & é"}]`),
		"tools":       json.RawMessage(`[{"type":"function","function":{"name":"lookup","parameters":{"type":"object"}}}]`),
	}
	sealedReq, err := wire.SealRequestFor(wire.ProfileChat, encPub, request, nil, sg.addr, ephPub)
	must(err)
	reqAAD, reqCT, err := wire.FrameBinding(sealedReq)
	must(err)
	reqH := proof.BindingHash(reqAAD, reqCT)

	// Buffered response: one final frame, `model` unbound as a router may
	// rewrite it.
	response := wire.Response{
		"id":      raw("chatcmpl-fixture"),
		"object":  raw("chat.completion"),
		"created": json.RawMessage(`1760000000`),
		"model":   raw("glm-5.3"),
		"usage":   json.RawMessage(`{"prompt_tokens":12,"completion_tokens":1,"total_tokens":13}`),
		"choices": json.RawMessage(`[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]`),
	}
	sealedResp, err := wire.SealResponseFor(wire.ProfileChat, ephPub, response, nil, "model")
	must(err)
	openedResp, err := wire.OpenResponseFor(wire.ProfileChat, ephPriv, sealedResp)
	must(err)
	bufText, err := proof.SignedTextE2EE(sealedReq, sealedResp)
	must(err)

	// Streamed response: three frames, usage on the final one.
	chunks := []wire.Response{
		{"id": raw("chatcmpl-fixture"), "object": raw("chat.completion.chunk"), "created": json.RawMessage(`1760000000`), "model": raw("glm-5.3"),
			"choices": json.RawMessage(`[{"index":0,"delta":{"role":"assistant","content":"o"},"finish_reason":null}]`)},
		{"id": raw("chatcmpl-fixture"), "object": raw("chat.completion.chunk"), "created": json.RawMessage(`1760000000`), "model": raw("glm-5.3"),
			"choices": json.RawMessage(`[{"index":0,"delta":{"content":"k"},"finish_reason":"stop"}]`)},
		{"id": raw("chatcmpl-fixture"), "object": raw("chat.completion.chunk"), "created": json.RawMessage(`1760000000`), "model": raw("glm-5.3"),
			"usage": json.RawMessage(`{"prompt_tokens":12,"completion_tokens":2,"total_tokens":14}`), "choices": json.RawMessage(`[]`)},
	}
	rs, err := wire.NewResponseSealerFor(wire.ProfileChat, ephPub, "model")
	must(err)
	var frames, opened []wire.Response
	for i, c := range chunks {
		f, err := rs.SealFrame(c, nil, i == len(chunks)-1)
		must(err)
		frames = append(frames, f)
	}
	ro, err := wire.NewResponseOpenerFor(wire.ProfileChat, ephPriv, frames[0])
	must(err)
	for _, f := range frames {
		o, err := ro.OpenFrame(f)
		must(err)
		opened = append(opened, o)
	}
	binderFrames := make([]map[string]json.RawMessage, len(frames))
	for i, f := range frames {
		binderFrames[i] = f
	}
	streamText, err := proof.SignedTextE2EEStream(sealedReq, binderFrames)
	must(err)

	out := map[string]any{
		"source":                 "github.com/0gfoundation/0g-pc-e2ee/protocol@6d823311abcbf8ba41656f38395cc50f19360c1b",
		"enc_private_key":        b64.EncodeToString(encPriv),
		"enc_public_key":         b64.EncodeToString(encPub),
		"client_eph_private_key": b64.EncodeToString(ephPriv),
		"signer_address":         sg.addr,
		"request_plain":          request,
		"request_sealed":         sealedReq,
		"request_binding_hash":   hex.EncodeToString(reqH[:]),
		"response_sealed":        sealedResp,
		"response_plain":         openedResp,
		"response_signature":     sg.sign(bufText),
		"stream_frames":          frames,
		"stream_plain":           opened,
		"stream_signature":       sg.sign(streamText),
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	must(enc.Encode(out))
}
