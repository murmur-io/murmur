/**
 * The vocabulary Murmur speaks — ONE user-facing term per concept, and the terms that must never
 * reach a screen.
 *
 * # Why a module and not a style guide
 *
 * Murmur's whole differentiator is telling the truth about what leaves the Mac. That only works if
 * the user can *read* the sentence. Before this module the same concept had three names in three
 * files ("egress" / "cloud processing" / "leaves this Mac"), and the honest sentences were the
 * first casualties of every simplification pass.
 *
 * Two rules govern every edit here:
 *
 * 1. **A replacement must be MORE honest, not vaguer.** A rewrite that is shorter AND drops a noun
 *    phrase is wrong. "Emails, card numbers and phone numbers are removed" is not improved by
 *    "your data is protected" — the second says nothing and implies more.
 * 2. **Enumerate, never abstract.** Redaction copy names what is redacted AND what is not.
 *    `e2e/settings/privacy-honesty.spec.ts` pins the clauses a well-meaning simplification would
 *    destroy.
 *
 * # What is and is not enforced
 *
 * `scripts/check-vocabulary.mjs` (wired into `scripts/ci.sh`) is the regression gate: it scans the
 * user-visible strings of both trees for its OWN `BANNED` list — deliberately a separate, blunter
 * list than {@link NEVER_SHOW_TERMS}, because a scanner can only match tokens while this module
 * also carries the replacement each term maps to. Keep the two lists in sympathy; the scanner's is
 * the one that fails the build (its `.vocabulary-baseline.json` may only SHRINK).
 *
 * What NO scanner can check is the honesty rule above — a clause deleted for brevity is invisible
 * to a token match. `e2e/settings/privacy-honesty.spec.ts` pins those clauses by fact instead.
 */

/**
 * Terms that must NEVER appear in user-visible copy. Every one names an implementation detail the
 * user has no way to reason about, and every one has a replacement in {@link PLAIN_TERMS}.
 *
 * Deliberately NOT on this list, because they are the RIGHT words for this audience:
 * `Obsidian vault`, `Markdown`, `Touch ID`, `Keychain`, the provider brand names, and the
 * checkpoint names in the model catalog (a user should be able to see which model is on their
 * disk — they stay as muted secondary text).
 */
export const NEVER_SHOW_TERMS: readonly string[] = [
  "GGUF",
  "q8",
  "KEK",
  "DEK",
  "blob",
  "sidecar",
  "IPC",
  "egress",
  "embedding",
  "token",
  "quantization",
  "base URL",
  "MCP",
];

/**
 * The replacement for each banned term: the ONE phrase Murmur uses for that concept.
 *
 * Keys are lowercase so a scanner can look a hit up directly. A value is a phrase, not a synonym
 * ring — if you find yourself wanting a second phrasing for the same concept, the answer is to
 * change this one, everywhere, in one commit.
 */
export const PLAIN_TERMS: Readonly<Record<string, string>> = {
  gguf: "model file",
  q8: "smaller, faster version",
  kek: "your master key",
  dek: "your database key",
  blob: "encrypted copy",
  sidecar: "helper",
  ipc: "the app's own connection",
  egress: "what leaves your Mac",
  embedding: "search index",
  token: "roughly ¾ of a word",
  quantization: "how compact the model is",
  "base url": "server address",
  mcp: "local server for Claude",
};

/**
 * Concept → the ONE user-facing term, for concepts that were never jargon but WERE inconsistent.
 * These are the phrases that must match across Settings, Onboarding, the Record screen and the
 * ledger, so a user reading two screens learns one vocabulary rather than three.
 */
export const GLOSSARY = {
  /** The one-time permission that lets a cloud provider see a redacted transcript. */
  cloudConsent: "Allow cloud processing",
  /** The act of removing personal details before text leaves. Always ENUMERATED where it counts. */
  redaction: "Emails, phones and cards removed",
  /** The optional on-device model that additionally masks people's names. */
  nameMasking: "Name masking",
  /** The unit the egress ledger counts. Never renamed without changing what is counted. */
  tokensSent: "Tokens sent",
  /** The gloss that makes the unit above mean something. */
  tokensGloss: "roughly ¾ of a word each",
  /** How the ledger groups usage. The store groups by MODEL label — not by service. */
  byModel: "By model",
  /** What a locked folder is. */
  lockedFolder: "Locked folder",
} as const;

/**
 * The generic sentence a user sees when Murmur genuinely cannot say more (see
 * `error-copy.service.ts` — deny-by-default). Deliberately blames nothing and promises nothing.
 */
export const GENERIC_FAILURE = "Something went wrong. Please try again.";
