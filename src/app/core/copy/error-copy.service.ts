import { Injectable } from "@angular/core";
import { GENERIC_FAILURE } from "./glossary";

/**
 * The ONE boundary between a raw Rust failure and a sentence a person reads.
 *
 * # The problem this replaces
 *
 * `AppError` is `Serialize` and crosses IPC as its bare `to_string()`, so every `String(e)` in the
 * frontend used to be a rendered developer message. There are ~2100 `AppError::*` constructions in
 * the Rust crate and a large minority of them carry vocabulary that must never reach a user
 * ("brain sidecar stdin missing", "account-session mutex poisoned", "E2EE decrypt/authentication
 * failed", "HKDF expand failed"). Components also *branched* on that prose, so rewording a Rust
 * string silently changed frontend behaviour.
 *
 * # The contract
 *
 * A failure that is MEANT to reach a banner or a toast carries a stable `[code]` at the front of
 * its message body (`src-tauri/src/errcode.rs`). Everything else is anonymous.
 *
 * ```text
 * "locked: [note-locked] note n1 is in a locked folder"
 *  └ variant tag ┘└ code ┘└──────── developer prose, NEVER rendered ────────┘
 * ```
 *
 * **DENY-BY-DEFAULT.** {@link ErrorCopyService.humanize} renders copy this module owns for a known
 * code and the generic sentence for everything else. There is deliberately no fallback heuristic
 * that inspects the message to decide whether it looks safe — a sniff for `::`, a path separator or
 * a snake_case token matches none of the ~161 never-show Rust strings, so a heuristic would be a
 * leak with a false sense of coverage.
 *
 * The one exception is by TYPE, never by text: a {@link UserFacingError} carries a sentence this
 * copy layer itself wrote in the frontend (the note-image preflight, which rejects before anything
 * crosses IPC and so has no `AppError` to tag). See that class for why a plain Tauri rejection can
 * never reach it.
 *
 * # Adding a code
 *
 * 1. Add the constant to `src-tauri/src/errcode.rs` (and to its pinned `ALL` list).
 * 2. Tag the `AppError` construction with `errcode::tag(...)`.
 * 3. Add the code to {@link ERROR_CODES} and its sentence to {@link BASE_COPY} here.
 *
 * Skipping step 3 is safe (the user gets the generic sentence); skipping step 2 silently downgrades
 * an already-written sentence to the generic one, which is why the Rust module's `ALL` list is
 * pinned by a test.
 *
 * # Scope of the P3 routing
 *
 * 185 `String(e)` sites across 47 files fed a rendered signal or a toast. Every one now goes
 * through {@link ErrorCopyService.humanize} or {@link ErrorCopyService.because}. NINE hand-rolled
 * `friendly*Error` mappers were DELETED outright and folded into the context map below (including
 * the two on the LOCK GATE — `share-panel.component.ts` and `note-share-panel.component.ts`);
 * a tenth, `note-editor.component.ts::friendlyLoadError`, keeps only its name and is now a
 * one-line delegator to {@link humanizeError}. None survives as a parallel decision table.
 *
 * The remaining raw-string reads in the app are deliberate and are NOT rendered:
 * `RecorderStore._error` (private, the source the code is extracted from) and
 * `detail.component.ts::masterErrorMessage`'s `/no master/` test (recorded as a prose coupling in
 * `errcode.rs`).
 *
 * # Where the guarantees are pinned
 *
 * `e2e/copy/error-copy-contract.spec.ts` exercises this table through a real rendered surface and
 * the real IPC boundary: ALL FOURTEEN {@link VARIANT_TAGS} stripped, deny-by-default for an
 * un-coded (and for an unknown-coded) failure, and every rung of the former document-import ladder
 * classified by code rather than prose. The Rust half of the contract — the wire shape and the
 * pinned code set — is pinned by `src-tauri/src/errcode.rs`'s own tests.
 */

/**
 * Every `AppError` variant tag, as `Display` writes it (`src-tauri/src/error.rs`).
 *
 * All fourteen are stripped before the `[code]` is read. `Other(#[from] anyhow::Error)` is
 * `#[error(transparent)]` and contributes no tag, which is exactly why it is not on this list and
 * exactly why an anonymous `Other` renders the generic sentence.
 */
const VARIANT_TAGS: readonly string[] = [
  "audio capture error",
  "transcription error",
  "summarizer error",
  "export error",
  "storage error",
  "migration error",
  "authentication error",
  "locked",
  "secrets error",
  "keychain access denied",
  "biometric authentication failed or was cancelled",
  "config error",
  "provider unavailable",
  "invalid argument",
];

/**
 * The allowlist. Mirrors `src-tauri/src/errcode.rs::ALL` — same order, same spelling.
 *
 * A code NOT in this list renders the generic sentence even if the Rust side emitted it, so the two
 * halves diverging is a copy regression, never a leak.
 */
export const ERROR_CODES = [
  "cloud-consent",
  "share-consent",
  "org-consent",
  "note-locked",
  "meeting-locked",
  "doc-locked",
  "folder-locked",
  "recording-linked-note",
  "note-missing",
  "note-folder-missing",
  "doc-unsupported",
  "doc-no-text",
  "doc-too-large",
  "doc-password",
  "doc-unreadable",
  "touch-id-cancelled",
  "touch-id-failed",
  "keychain-denied",
  "sharing-unreachable",
  "share-busy",
  "sharing-rate-limited",
  "sharing-rejected",
  "sharing-signin-required",
  "sharing-account-required",
  "sharing-upgrade-required",
  "org-edit-conflict",
  "reminders-denied",
  // Added 2026-08-28 with the meeting→note conversion refusals. Each of these was reachable in
  // production while ANONYMOUS, so deny-by-default rendered "Couldn't convert this meeting. Please
  // try again." for a failure the user could have fixed in one click.
  "convert-no-transcript",
  "note-template-missing",
  "note-provider-empty",
  "share-active",
  "folder-closing",
  "container-unavailable",
  "org-rotation-pending",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

const KNOWN_CODES: ReadonlySet<string> = new Set<string>(ERROR_CODES);

/**
 * Where the failure happened. Selects a context-specific sentence for the same code (a locked
 * folder means something different when you are saving a note than when you are sharing one) and a
 * context-specific fallback for an anonymous failure.
 */
export type ErrorContext =
  | "generic"
  | "recording"
  | "note-save"
  | "note-load"
  | "note-share"
  | "meeting-share"
  | "org-share"
  | "brain-import"
  | "brain-delete"
  | "doc-note"
  | "unlock"
  | "account"
  | "convert"
  | "tasks";

/**
 * The sentence for each code when no context adds anything.
 *
 * # The document-import ladder that no longer needs an order
 *
 * `brain.component.ts::friendlyImportError` used to test these by prose in a LOAD-BEARING order:
 * `no text found` had to beat the generic `unsupported document type`, or a scanned PDF was
 * reported to the user as an unsupported file type. That failure mode is now structurally
 * impossible — each Rust site carries exactly ONE code (`errcode::DOC_NO_TEXT` in
 * `extract/pdf.rs`, `errcode::DOC_UNSUPPORTED` in `commands/documents.rs`), so a scanned PDF can
 * never be classified as an unsupported type. This map is a lookup, not a ladder.
 */
const BASE_COPY: Readonly<Record<ErrorCode, string>> = {
  "cloud-consent":
    "This needs your one-time permission to send a redacted transcript to your AI provider.",
  "share-consent":
    "Sharing needs your one-time permission first — confirm the upload notice in Settings.",
  "org-consent":
    "Sharing to your organisation needs your one-time permission first — confirm it in Settings ▸ Organization.",
  "note-locked": "This note is locked — unlock its folder to change it.",
  "meeting-locked": "This meeting is locked — unlock it first.",
  "doc-locked": "That folder is locked — unlock it first.",
  "folder-locked": "That folder is locked — unlock it first.",
  "recording-linked-note":
    "This recording has a linked note. Move it between open folders, or remove the destination folder lock first.",
  "note-missing": "This note no longer exists.",
  "note-folder-missing": "That folder no longer exists.",
  "doc-unsupported":
    "That file type can’t be imported. Try Markdown, text, PDF, Word, PowerPoint, Excel, HTML or an image.",
  "doc-no-text": "No readable text was found in that file, even after reading the images.",
  // Covers all four Rust sites that carry this code: the zip-bomb ceiling (`extract/ooxml.rs`,
  // `extract/xlsx.rs`), the universal extracted-text ceiling and the flow-file read cap
  // (`extract/mod.rs`), and the HTML file cap (`extract/html.rs`). The sentence names BOTH shapes
  // — the file on disk and the text it unpacks to — because a bare "too large" leaves the user
  // measuring the wrong thing when a 2 MB archive expands past the limit.
  "doc-too-large":
    "That file goes past the safe import limit — either the file itself or the text it unpacks to is too big to add.",
  "doc-password": "That PDF is password-protected — unlock it and try again.",
  "doc-unreadable":
    "That file couldn’t be read — it may be damaged or in an unexpected format.",
  "touch-id-cancelled": "Touch ID was cancelled.",
  "touch-id-failed": "Touch ID didn’t recognise you.",
  "keychain-denied":
    "macOS wouldn’t release the key. Unlock your Mac’s login keychain and try again.",
  "sharing-unreachable":
    "Can’t reach the sharing server. Check your connection, then try again.",
  "share-busy":
    "Another sharing operation is still running. Give it a moment, then try again.",
  "sharing-rate-limited": "Too many attempts — wait a minute, then try again.",
  "sharing-rejected":
    "That didn’t work. Check the code (it may have expired) and try again.",
  "sharing-signin-required": "You’ve been signed out — sign in again to continue.",
  // DISTINCT from `sharing-signin-required` on purpose. That one means a session EXISTED and
  // lapsed; this one means there has never been an account on this device — the DEFAULT
  // local-first user, who is the product's headline promise. Telling that person they have
  // "been signed out" is a lie, and it was the second-order trap inside the 2.0 Tasks blocker.
  "sharing-account-required":
    "Sharing needs a Murmur account. Everything already on this Mac keeps working without one.",
  "sharing-upgrade-required":
    "Sharing is paused until the server supports crash-safe share creation. Try again after the server is updated.",
  "org-edit-conflict":
    "This shared note changed elsewhere. Your draft is still here — reload the latest version and try again.",
  "reminders-denied":
    "Grant Reminders access in System Settings ▸ Privacy & Security ▸ Reminders.",
  "convert-no-transcript":
    "This recording has no transcript yet, so there’s nothing to turn into a note.",
  "note-template-missing":
    "That note template no longer exists — pick another one in Settings ▸ Notes.",
  "note-provider-empty":
    "Your AI provider returned an empty note. Try again, or switch provider in Settings.",
  // Names the ACTION, not the mechanism: "a share is active" tells the user nothing they can do.
  // Both the link-share and the org-brain share reach this code, so the sentence covers both.
  "share-active":
    "This note is shared. Revoke its share first, then try again.",
  "folder-closing":
    "That folder is being prepared for sharing. Try again in a moment.",
  // Deliberately NOT phrased as a failure. The removal SUCCEEDED; what is outstanding is the key
  // rotation that locks the removed person out of anything published from now on. Telling the user
  // to "try again" would invite them to re-remove somebody who is already gone, and hide the one
  // fact that matters: until the rotation lands, new posts are still readable with their old key.
  "org-rotation-pending":
    "Removed. Their access key is still being rotated — until that finishes, anything new you share here could still be readable by them. Murmur keeps retrying.",
  "container-unavailable":
    "This recording isn’t in a folder Murmur can add a note to. Move it, then try again.",
};

/**
 * Per-context overrides. Each entry says the SAME fact as {@link BASE_COPY} plus what to do next
 * here — an override that only shortens the sentence is a regression, not a win.
 */
const CONTEXT_COPY: Partial<
  Record<ErrorContext, Partial<Record<ErrorCode, string>>>
> = {
  "note-save": {
    "note-locked": "This note is locked — unlock its folder to edit.",
    "folder-locked": "This note is locked — unlock its folder to edit.",
    "note-missing": "This note no longer exists — it may have been deleted elsewhere.",
  },
  "note-load": {
    "note-missing": "This note was deleted.",
  },
  "note-share": {
    "note-locked": "This note is locked — unlock its folder to share it.",
    "folder-locked": "This note is locked — unlock its folder to share it.",
    "touch-id-cancelled":
      "Touch ID was cancelled. Use Unlock for sharing to unlock with your password.",
    "touch-id-failed":
      "Couldn’t unlock with Touch ID. Use Unlock for sharing to unlock with your password.",
    "keychain-denied":
      "Couldn’t unlock with Touch ID. Use Unlock for sharing to unlock with your password.",
  },
  "meeting-share": {
    "meeting-locked": "This meeting is locked — unlock its folder to share it.",
    "touch-id-cancelled":
      "Touch ID was cancelled. Use Unlock for sharing to unlock with your password.",
    "touch-id-failed":
      "Couldn’t unlock with Touch ID. Use Unlock for sharing to unlock with your password.",
    "keychain-denied":
      "Couldn’t unlock with Touch ID. Use Unlock for sharing to unlock with your password.",
  },
  account: {
    "touch-id-cancelled":
      "Touch ID was cancelled. Sign in with your password to share instead.",
    "touch-id-failed":
      "Couldn’t unlock with Touch ID. Sign in with your password to share instead.",
    "keychain-denied":
      "Couldn’t unlock with Touch ID. Sign in with your password to share instead.",
    // The accept-into picker offers "Shared (default)" alongside the open folders, so BOTH escapes
    // are named — dropping "or the default" would hide the one destination that always exists.
    "folder-locked":
      "That folder is locked. Unlock it, or accept into an open folder or the default one.",
    "note-locked":
      "That folder is locked. Unlock it, or accept into an open folder or the default one.",
  },
  tasks: {
    "sharing-account-required":
      "Tasks are shared work inside an organization — sign in to your Murmur account to see them.",
    "sharing-signin-required":
      "Your sharing session ended — sign in again to see your organization’s tasks.",
  },
  "org-share": {
    "note-locked":
      "This item is locked — unlock its folder before adding it to the org brain.",
    "meeting-locked":
      "This item is locked — unlock its folder before adding it to the org brain.",
    "doc-locked":
      "This item is locked — unlock its folder before adding it to the org brain.",
    "folder-locked":
      "This item is locked — unlock its folder before adding it to the org brain.",
  },
  "brain-import": {
    "doc-locked": "That folder is locked — unlock it first to add to the brain.",
    "folder-locked": "That folder is locked — unlock it first to add to the brain.",
    "doc-no-text": "No readable text found in that file, even after OCR.",
  },
  "brain-delete": {
    "doc-locked": "That folder is locked — unlock it first to delete its items.",
    "folder-locked": "That folder is locked — unlock it first to delete its items.",
  },
  "doc-note": {
    "doc-locked": "This document is in a locked folder — unlock it first.",
    "folder-locked": "This document is in a locked folder — unlock it first.",
    "doc-no-text": "This document has no text to turn into a note.",
    "cloud-consent":
      "Making a note needs your one-time permission to send redacted text to your AI provider.",
  },
  recording: {
    "meeting-locked": "This meeting is locked — unlock its folder to finish the note.",
  },
  convert: {
    "meeting-locked": "This meeting is locked — unlock its folder to convert it.",
    "folder-locked": "This meeting’s folder is locked — unlock it to convert it.",
    "share-active":
      "This meeting’s note is shared. Revoke its share first, then convert again.",
    "cloud-consent":
      "Converting needs your one-time permission to send a redacted transcript to your AI provider.",
  },
  unlock: {
    "touch-id-cancelled": "Touch ID was cancelled — try again.",
    "touch-id-failed": "Touch ID didn’t recognise you — try again.",
  },
};

/** The "we genuinely can't say more" sentence, tuned per surface. */
const CONTEXT_FALLBACK: Partial<Record<ErrorContext, string>> = {
  "note-save": "Couldn’t save this note. Please try again.",
  "note-load": "Couldn’t open this note. Please try again.",
  "note-share": "Couldn’t share this note. Please try again.",
  "meeting-share": "Couldn’t share this meeting. Please try again.",
  "org-share": "Couldn’t share that to your organisation. Please try again.",
  "brain-import": "Couldn’t add that to the brain. Please try again.",
  "brain-delete": "Couldn’t remove that. Please try again.",
  "doc-note": "Couldn’t make a note from this document. Please try again.",
  unlock: "Couldn’t unlock. Please try again.",
  tasks: "Couldn’t load shared tasks. Please try again.",
  recording: "Couldn’t finish that recording. Please try again.",
  convert: "Couldn’t convert this meeting. Please try again.",
};

/** Matches a leading `[kebab-code]` — strict, never a sniff. */
const CODE_PATTERN = /^\[([a-z0-9]+(?:-[a-z0-9]+)*)\]\s*/;

/**
 * A failure whose message was ALREADY written by this copy layer, in the frontend.
 *
 * # Why this exists, and why it is not a hole in deny-by-default
 *
 * Not every rejection a component catches comes from Rust. A handful of checks run entirely in the
 * webview and reject with a finished sentence — the note-image preflight in
 * `services/note-attachment.service.ts` is the whole population: it inspects a pasted image's
 * bounded header and refuses a decompression bomb *before* any decoder allocates, so its refusal
 * never crosses IPC and there is no `AppError` to carry an `errcode`. Routing those through
 * {@link humanizeError} as anonymous failures replaced a specific, actionable sentence ("That
 * image's dimensions are too large to process safely.") with "Something went wrong." — strictly
 * less honest, which rule 1 of the glossary forbids.
 *
 * The distinction this class draws is a TYPE, not a shape of text. That matters: the module doc
 * rules out a heuristic that inspects a message to decide whether it looks safe, and this is not
 * one. A Tauri rejection arrives as a plain string (and a `throw` inside a component arrives as a
 * plain `Error`), so neither can ever be an instance of this class — only code that deliberately
 * constructs a `UserFacingError` opts its sentence in, and that constructor is the review point.
 *
 * The message MUST be finished user copy: a whole sentence, no variant tag, no `[code]`, no
 * identifier, and nothing from `glossary.ts`'s never-show list. If a sentence would vary by
 * surface, it belongs in {@link BASE_COPY} under a real code instead.
 */
export class UserFacingError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UserFacingError";
  }
}

/**
 * Reduce anything a `catch` can hand us to the raw wire string, with the JS `Error:` wrapper and
 * the `AppError` variant tag removed. Exported for tests and for the few call sites that must still
 * inspect the raw text (a `console` breadcrumb — never a rendered string).
 */
export function stripErrorPrefixes(e: unknown): string {
  let s = typeof e === "string" ? e : String(e);
  // `new Error(msg)` stringifies as "Error: msg"; Tauri rejects with the bare string, but a
  // frontend-thrown Error can reach the same handler.
  s = s.replace(/^Error:\s*/, "").trim();
  for (const tag of VARIANT_TAGS) {
    if (s.startsWith(`${tag}: `)) {
      return s.slice(tag.length + 2).trim();
    }
  }
  return s;
}

/**
 * The stable machine code of a failure, or `null` when it carries none.
 *
 * This is what behaviour branches on — never the prose. `record.component.ts` asks
 * `errorCode(...) === "cloud-consent"` to decide whether to show the Allow banner; before the code
 * existed it regex-matched a Rust sentence, so rewording that sentence broke the consent flow for
 * every cloud user.
 */
export function errorCodeOf(e: unknown): ErrorCode | null {
  const match = CODE_PATTERN.exec(stripErrorPrefixes(e));
  if (!match) {
    return null;
  }
  const code = match[1];
  return KNOWN_CODES.has(code) ? (code as ErrorCode) : null;
}

/** True when `e` carries exactly `code`. */
export function errorHasCode(e: unknown, code: ErrorCode): boolean {
  return errorCodeOf(e) === code;
}

/**
 * The sentence to show for `e` on the `context` surface.
 *
 * Deny-by-default: an un-coded failure NEVER renders its own text, no matter how harmless it looks.
 */
export function humanizeError(e: unknown, context: ErrorContext = "generic"): string {
  if (e instanceof UserFacingError) {
    return e.message;
  }
  const code = errorCodeOf(e);
  if (code === null) {
    return CONTEXT_FALLBACK[context] ?? GENERIC_FAILURE;
  }
  return CONTEXT_COPY[context]?.[code] ?? BASE_COPY[code];
}

/**
 * `"<what failed> — <why>"`, for the many call sites that used to write
 * `"Couldn’t save tags: " + String(e)`.
 *
 * The action half is the surface's own words and always survives; the reason half is
 * {@link humanizeError}, so a known code explains itself and an anonymous failure degrades to a
 * plain "please try again" instead of pasting a Rust string after a friendly prefix.
 *
 * `action` is written WITHOUT trailing punctuation ("Couldn’t save tags").
 */
export function describeFailure(
  action: string,
  e: unknown,
  context: ErrorContext = "generic",
): string {
  if (e instanceof UserFacingError) {
    return `${action} — ${e.message}`;
  }
  const code = errorCodeOf(e);
  if (code === null) {
    return `${action}. Please try again.`;
  }
  return `${action} — ${CONTEXT_COPY[context]?.[code] ?? BASE_COPY[code]}`;
}

/**
 * Injectable face of the module functions above.
 *
 * Components inject this; the pure functions exist so the same logic is reachable from a store's
 * `computed()` and from unit tests without a TestBed. Both are the same code — there is exactly one
 * decision table.
 */
@Injectable({ providedIn: "root" })
export class ErrorCopyService {
  /** The user-facing sentence for a failure. Deny-by-default — see the module doc. */
  humanize(e: unknown, context: ErrorContext = "generic"): string {
    return humanizeError(e, context);
  }

  /** The stable machine code, for BEHAVIOUR decisions. Never render this. */
  codeOf(e: unknown): ErrorCode | null {
    return errorCodeOf(e);
  }

  /** True when the failure is exactly `code`. */
  is(e: unknown, code: ErrorCode): boolean {
    return errorHasCode(e, code);
  }

  /** `"<what failed> — <why>"` for a surface that wants to name the action it attempted. */
  because(action: string, e: unknown, context: ErrorContext = "generic"): string {
    return describeFailure(action, e, context);
  }
}
