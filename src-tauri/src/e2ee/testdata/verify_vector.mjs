// Rust↔browser interop cross-check for Murmur's mode-A link share (spec T2.3).
//
// Reads `link_share_vector.json` (emitted by the Rust test `link_share_interop_vector` run with
// MURMUR_EMIT_VECTORS=1) and, using ONLY Node's built-in WebCrypto (`crypto.subtle` — the same
// AES-GCM + HKDF-SHA256 primitives the dependency-free browser share-viewer will use), decrypts the
// no-password link share and asserts the recovered inner envelope equals the expected plaintext.
// This proves the RustCrypto `aes-gcm` cell (`nonce||ct||tag`, AAD-bound) and the `hkdf` KEK_link
// derivation are byte-compatible with WebCrypto BEFORE the real JS viewer exists.
//
// The password vector is NOT verified here: Node core ships no Argon2id, and adding an npm dep is out
// of scope. The no-password case is the mandatory cross-check; the password path is covered by the
// Rust round-trip test and will get a hash-wasm cross-check in the JS viewer milestone (M3).
//
// Run:  node verify_vector.mjs
// Exit: 0 on success, 1 on any mismatch/failure.

import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";

const { subtle } = webcrypto;
const enc = new TextEncoder();
const dec = new TextDecoder();

const b64u = (s) => new Uint8Array(Buffer.from(s, "base64url"));

function assert(cond, msg) {
  if (!cond) {
    throw new Error(`ASSERT FAILED: ${msg}`);
  }
}

// HKDF-SHA256 → 32 bytes. WebCrypto requires an explicit salt; an empty salt is byte-identical to the
// RustCrypto `Hkdf::new(None, ..)` all-zero salt (HMAC zero-pads short keys to the block size), which
// is what the `gateSecret` cross-check below confirms.
async function hkdf32(ikm, salt, infoStr) {
  const keyMaterial = await subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const bits = await subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info: enc.encode(infoStr) },
    keyMaterial,
    256,
  );
  return new Uint8Array(bits);
}

// Open an AES-256-GCM cell `nonce(12) || ciphertext || tag(16)` with associated data `aadStr`.
async function openCell(keyBytes, cell, aadStr) {
  const key = await subtle.importKey("raw", keyBytes, "AES-GCM", false, ["decrypt"]);
  const iv = cell.slice(0, 12);
  const ctAndTag = cell.slice(12); // WebCrypto expects the tag appended to the ciphertext.
  const pt = await subtle.decrypt(
    { name: "AES-GCM", iv, additionalData: enc.encode(aadStr), tagLength: 128 },
    key,
    ctAndTag,
  );
  return new Uint8Array(pt);
}

function bytesEqual(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

async function verifyNoPassword(v, aad) {
  const L = b64u(v.lB64);
  const gateSalt = b64u(v.gateSaltB64);
  const linkNkAad = `murmur-link/v1|${v.shareId}|${v.rev}`;
  assert(linkNkAad === aad.linkNk, "reconstructed linkNk AAD matches the fixture");

  // 1. KEK_link = HKDF(L, salt=gateSalt, info=linkNkAad)  (no password → ikm = L).
  const kekLink = await hkdf32(L, gateSalt, linkNkAad);

  // 2. Unwrap NK under KEK_link (AAD = linkNk).
  const nk = await openCell(kekLink, b64u(v.wrappedNkB64), linkNkAad);
  assert(nk.length === 32, "unwrapped NK is 32 bytes");

  // 3. Open the content cell C under NK (AAD = share_content).
  const contentAad = `murmur-share/v1|${v.shareId}|${v.rev}`;
  assert(contentAad === aad.content, "reconstructed content AAD matches the fixture");
  const ptBytes = await openCell(nk, b64u(v.ciphertextCellB64), contentAad);
  const envelope = JSON.parse(dec.decode(ptBytes));

  // 4. Assert the decrypted inner envelope equals the Rust-declared expected plaintext.
  assert(envelope.v === v.expected.v, `envelope.v (${envelope.v} === ${v.expected.v})`);
  assert(envelope.title === v.expected.title, `title matches ("${envelope.title}")`);
  assert(envelope.markdown === v.expected.markdown, "markdown matches");
  assert(envelope.createdAt === v.expected.createdAt, "createdAt matches");

  // 5. Bonus: recompute the L-derived gate secret with an EMPTY salt and confirm it equals the Rust
  //    value — this validates the Hkdf::new(None) ⇔ WebCrypto empty-salt equivalence directly.
  const gate = await hkdf32(L, new Uint8Array(0), "murmur-link/v1:gate");
  assert(bytesEqual(gate, b64u(v.gateSecretB64)), "gate_secret matches (no-salt HKDF equivalence)");

  return { title: envelope.title, markdownBytes: ptBytes.length };
}

async function main() {
  const path = new URL("./link_share_vector.json", import.meta.url);
  const vector = JSON.parse(readFileSync(path, "utf8"));

  const result = await verifyNoPassword(vector.noPassword, vector.aad);
  console.log("PASS  no-password link share decrypted via WebCrypto");
  console.log(`      title    = "${result.title}"`);
  console.log(`      markdown = ${result.markdownBytes} plaintext bytes`);

  if (vector.withPassword) {
    console.log(
      "SKIP  password link share (Node core has no Argon2id; covered by the Rust round-trip + JS viewer M3)",
    );
  }

  console.log("\nRust ↔ WebCrypto interop: OK");
}

main().catch((err) => {
  console.error("FAIL ", err.message);
  process.exit(1);
});
