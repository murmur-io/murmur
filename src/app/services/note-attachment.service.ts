import { Injectable, inject } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type {
  NoteAttachmentDto,
  NoteAttachmentOwnerKind,
} from "../core/models";

const MAX_EDGE = 2560;
const MAX_OUTPUT_BYTES = 3 * 1024 * 1024;
const MAX_SOURCE_BYTES = 24 * 1024 * 1024;
const MAX_DIMENSION_HEADER_BYTES = 1024 * 1024;
const MAX_SOURCE_EDGE = 32768;
const MAX_SOURCE_PIXELS = 25_000_000;
export const MAX_NOTE_ATTACHMENTS = 16;
const INPUT_IMAGE_TYPES = new Set([
  "image/png",
  "image/jpeg",
  "image/webp",
]);
const ATTACHMENT_REF_RE =
  /murmur-attachment:\/\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/gi;
const PNG_SIGNATURE = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] as const;
// Ancillary chunks that can carry embedded text / profiles / timestamps — the private metadata the
// local normalizer exists to strip. WebKit's own `canvas.toBlob("image/png")` emits `eXIf`, so it
// MUST be dropped before upload; `sRGB` (and other rendering-intent chunks) are benign and kept.
const PNG_METADATA_CHUNKS = new Set(["eXIf", "tEXt", "zTXt", "iTXt", "iCCP", "tIME"]);

/**
 * Remove privacy-bearing ancillary chunks from a PNG byte stream, preserving chunk order and every
 * structural/rendering chunk verbatim (each PNG chunk carries its own CRC, so dropping an ancillary
 * chunk needs no re-CRC). If the input is not a well-formed PNG it is returned unchanged and the
 * backend validator fails closed. Exported for direct unit testing.
 */
export function stripPngMetadata(
  bytes: Uint8Array<ArrayBuffer>,
): Uint8Array<ArrayBuffer> {
  // Signature (8) + at least one 12-byte chunk header/footer must be present.
  if (bytes.length < PNG_SIGNATURE.length + 12) {
    return bytes;
  }
  for (let i = 0; i < PNG_SIGNATURE.length; i += 1) {
    if (bytes[i] !== PNG_SIGNATURE[i]) {
      return bytes;
    }
  }
  const kept: Uint8Array<ArrayBuffer>[] = [bytes.subarray(0, PNG_SIGNATURE.length)];
  let offset: number = PNG_SIGNATURE.length;
  let sawIend = false;
  while (offset + 8 <= bytes.length) {
    const length =
      ((bytes[offset] << 24) >>> 0) +
      (bytes[offset + 1] << 16) +
      (bytes[offset + 2] << 8) +
      bytes[offset + 3];
    const type = String.fromCharCode(
      bytes[offset + 4],
      bytes[offset + 5],
      bytes[offset + 6],
      bytes[offset + 7],
    );
    const end = offset + 12 + length;
    if (end > bytes.length) {
      // Malformed length overruns the buffer — leave the bytes untouched; the backend rejects them.
      return bytes;
    }
    if (!PNG_METADATA_CHUNKS.has(type)) {
      kept.push(bytes.subarray(offset, end));
    }
    offset = end;
    if (type === "IEND") {
      sawIend = true;
      break;
    }
  }
  if (!sawIend) {
    return bytes;
  }
  let total = 0;
  for (const part of kept) {
    total += part.length;
  }
  const out = new Uint8Array(total);
  let cursor = 0;
  for (const part of kept) {
    out.set(part, cursor);
    cursor += part.length;
  }
  return out;
}

interface ImageDimensions {
  width: number;
  height: number;
}
/** Parse bounded container headers before asking the browser to allocate a decoded bitmap. */
function imageDimensions(mimeType: string, bytes: Uint8Array): ImageDimensions | null {
  if (mimeType === "image/png") {
    if (
      bytes.length < 24 ||
      bytes[0] !== 0x89 ||
      bytes[1] !== 0x50 ||
      bytes[2] !== 0x4e ||
      bytes[3] !== 0x47 ||
      bytes[4] !== 0x0d ||
      bytes[5] !== 0x0a ||
      bytes[6] !== 0x1a ||
      bytes[7] !== 0x0a ||
      String.fromCharCode(...bytes.slice(12, 16)) !== "IHDR"
    ) {
      return null;
    }
    return {
      width:
        (((bytes[16] << 24) >>> 0) |
          (bytes[17] << 16) |
          (bytes[18] << 8) |
          bytes[19]) >>>
        0,
      height:
        (((bytes[20] << 24) >>> 0) |
          (bytes[21] << 16) |
          (bytes[22] << 8) |
          bytes[23]) >>>
        0,
    };
  }

  if (mimeType === "image/jpeg") {
    if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
      return null;
    }
    let offset = 2;
    while (offset + 3 < bytes.length) {
      while (offset < bytes.length && bytes[offset] === 0xff) offset += 1;
      if (offset >= bytes.length) return null;
      const marker = bytes[offset++];
      if (marker === 0xd9 || marker === 0xda) return null;
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
      if (offset + 1 >= bytes.length) return null;
      const length = (bytes[offset] << 8) | bytes[offset + 1];
      if (length < 2 || offset + length > bytes.length) return null;
      if (
        [
          0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf,
        ].includes(marker)
      ) {
        if (length < 7) return null;
        return {
          height: (bytes[offset + 3] << 8) | bytes[offset + 4],
          width: (bytes[offset + 5] << 8) | bytes[offset + 6],
        };
      }
      offset += length;
    }
    return null;
  }

  if (mimeType === "image/webp") {
    if (
      bytes.length < 30 ||
      String.fromCharCode(...bytes.slice(0, 4)) !== "RIFF" ||
      String.fromCharCode(...bytes.slice(8, 12)) !== "WEBP"
    ) {
      return null;
    }
    const kind = String.fromCharCode(...bytes.slice(12, 16));
    if (kind === "VP8X") {
      return {
        width: 1 + bytes[24] + (bytes[25] << 8) + (bytes[26] << 16),
        height: 1 + bytes[27] + (bytes[28] << 8) + (bytes[29] << 16),
      };
    }
    if (kind === "VP8L" && bytes[20] === 0x2f) {
      return {
        width: 1 + bytes[21] + ((bytes[22] & 0x3f) << 8),
        height: 1 + (bytes[22] >> 6) + (bytes[23] << 2) + ((bytes[24] & 0x0f) << 10),
      };
    }
    if (
      kind === "VP8 " &&
      bytes[23] === 0x9d &&
      bytes[24] === 0x01 &&
      bytes[25] === 0x2a
    ) {
      return {
        width: (bytes[26] | (bytes[27] << 8)) & 0x3fff,
        height: (bytes[28] | (bytes[29] << 8)) & 0x3fff,
      };
    }
  }
  return null;
}

export interface LocalImageCandidate {
  blob: Blob;
  fileName: string;
  alt: string;
}

export type AttachmentPasteSegment =
  | { kind: "text"; text: string }
  | { kind: "image"; image: LocalImageCandidate };

export interface AttachmentPastePlan {
  segments: AttachmentPasteSegment[];
  skippedExternalImages: boolean;
  skippedUnsupportedImages: boolean;
  skippedTooManyImages: boolean;
}

export interface PendingAttachment {
  id: string;
  token: string;
  image: LocalImageCandidate;
}

export interface PendingAttachmentPlan {
  markdown: string;
  images: PendingAttachment[];
}

export interface MarkdownEdit {
  value: string;
  selectionStart: number;
  selectionEnd: number;
}

export interface PendingMarkdownEdit extends MarkdownEdit {
  /** False when the user broke the image wrapper; the pending URI was removed, never persisted. */
  canonicalSlot: boolean;
}

/**
 * Insert a block at a textarea selection while keeping paragraph boundaries
 * readable. Returned selection sits immediately after the inserted block.
 */
export function insertMarkdownBlock(
  value: string,
  from: number,
  to: number,
  markdown: string,
): MarkdownEdit {
  const start = Math.max(0, Math.min(from, value.length));
  const end = Math.max(start, Math.min(to, value.length));
  const before = value.slice(0, start);
  const after = value.slice(end);
  const content = markdown.trim();
  const prefix =
    before.length === 0 || before.endsWith("\n\n")
      ? ""
      : before.endsWith("\n")
        ? "\n"
        : "\n\n";
  const suffix =
    after.length === 0 || after.startsWith("\n\n")
      ? ""
      : after.startsWith("\n")
        ? "\n"
        : "\n\n";
  const inserted = prefix + content;
  const next = before + inserted + suffix + after;
  const caret = before.length + inserted.length;
  return { value: next, selectionStart: caret, selectionEnd: caret };
}

/** Replace a unique upload marker without moving a caret elsewhere in the draft. */
export function replaceMarkdownToken(
  value: string,
  token: string,
  replacement: string,
  selectionStart: number,
  selectionEnd: number,
): MarkdownEdit | null {
  const at = value.indexOf(token);
  if (at === -1) {
    return null;
  }
  const next = value.slice(0, at) + replacement + value.slice(at + token.length);
  const delta = replacement.length - token.length;
  const shift = (position: number): number => {
    if (position <= at) {
      return position;
    }
    if (position >= at + token.length) {
      return position + delta;
    }
    return at + replacement.length;
  };
  return {
    value: next,
    selectionStart: shift(selectionStart),
    selectionEnd: shift(selectionEnd),
  };
}

/**
 * Resolve a pending image by its stable URI identity, not by the mutable alt text around it. If the
 * user breaks the Markdown wrapper while decoding runs, remove the orphan URI and report a rejected
 * slot so the caller deletes the just-created SQLCipher row.
 */
export function replacePendingAttachmentUri(
  value: string,
  pendingId: string,
  replacement: string,
  selectionStart: number,
  selectionEnd: number,
): PendingMarkdownEdit | null {
  const uri = `murmur-pending://${pendingId}`;
  const uriAt = value.indexOf(uri);
  if (uriAt === -1) {
    return null;
  }
  const lineStart = value.lastIndexOf("\n", uriAt - 1) + 1;
  const lineEndAt = value.indexOf("\n", uriAt);
  const lineEnd = lineEndAt === -1 ? value.length : lineEndAt;
  const line = value.slice(lineStart, lineEnd);
  const escapedId = pendingId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const marker = new RegExp(
    `!\\[[^\\]\\r\\n]*\\]\\(murmur-pending:\\/\\/${escapedId}\\)`,
  ).exec(line);
  const start = marker ? lineStart + marker.index : uriAt;
  const length = marker ? marker[0].length : uri.length;
  const inserted = marker ? replacement : "";
  const next = value.slice(0, start) + inserted + value.slice(start + length);
  const delta = inserted.length - length;
  const shift = (position: number): number => {
    if (position <= start) return position;
    if (position >= start + length) return position + delta;
    return start + inserted.length;
  };
  return {
    value: next,
    selectionStart: shift(selectionStart),
    selectionEnd: shift(selectionEnd),
    canonicalSlot: marker !== null,
  };
}

/** Keep only attachment DTOs referenced by this exact markdown document. */
export function referencedNoteAttachments(
  markdown: string,
  attachments: readonly NoteAttachmentDto[],
): NoteAttachmentDto[] {
  const ids = new Set<string>();
  ATTACHMENT_REF_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = ATTACHMENT_REF_RE.exec(markdown)) !== null) {
    ids.add(match[1].toLowerCase());
  }
  return attachments.filter((attachment) => ids.has(attachment.id.toLowerCase()));
}

/**
 * Local-only image preparation and attachment IPC. Every accepted input is
 * decoded into a canvas and re-encoded before it crosses IPC — as WebP where the
 * engine can encode it, otherwise as a metadata-free PNG (WebKit's <canvas>
 * cannot encode WebP). Either way the backend never receives the original
 * container or its metadata. No remote URL is fetched by this service.
 */
@Injectable({ providedIn: "root" })
export class NoteAttachmentService {
  private readonly ipc = inject(IpcService);

  /** Extract actual clipboard/drop image blobs and useful text in DOM order. */
  planFromTransfer(
    data: DataTransfer,
    maxImages = MAX_NOTE_ATTACHMENTS,
  ): AttachmentPastePlan {
    const available = Math.max(0, maxImages);
    const filePlan = this.imageFiles(data, available);
    const files = filePlan.images;
    const html = data.getData("text/html");
    if (html && /<\s*img\b/i.test(html)) {
      return this.planFromHtml(html, files, available, filePlan.skippedTooManyImages);
    }

    const segments: AttachmentPasteSegment[] = [];
    const text = data.getData("text/plain").trim();
    if (text && files.length > 0) {
      segments.push({ kind: "text", text });
    }
    for (const file of files) {
      segments.push({ kind: "image", image: file });
    }
    return {
      segments,
      skippedExternalImages: false,
      skippedUnsupportedImages: false,
      skippedTooManyImages: filePlan.skippedTooManyImages,
    };
  }

  /** Build an image-only plan for the explicit hidden file input. */
  planFromFiles(
    files: FileList | readonly File[],
    maxImages = MAX_NOTE_ATTACHMENTS,
  ): AttachmentPastePlan {
    const segments: AttachmentPasteSegment[] = [];
    let skippedUnsupportedImages = false;
    let skippedTooManyImages = false;
    const available = Math.max(0, maxImages);
    for (const file of Array.from(files)) {
      if (!INPUT_IMAGE_TYPES.has(file.type.toLowerCase())) {
        skippedUnsupportedImages = true;
        continue;
      }
      if (segments.length >= available) {
        skippedTooManyImages = true;
        continue;
      }
      segments.push({ kind: "image", image: this.fileCandidate(file) });
    }
    return {
      segments,
      skippedExternalImages: false,
      skippedUnsupportedImages,
      skippedTooManyImages,
    };
  }

  /** Convert image segments to stable, visible upload markers before async work. */
  pendingPlan(plan: AttachmentPastePlan): PendingAttachmentPlan {
    const images: PendingAttachment[] = [];
    const chunks: string[] = [];
    for (const segment of plan.segments) {
      if (segment.kind === "text") {
        const text = this.normalizeClipboardText(segment.text);
        if (text) {
          chunks.push(text);
        }
        continue;
      }
      const id = this.uuid();
      const token = `![Uploading image…](murmur-pending://${id})`;
      images.push({ id, token, image: segment.image });
      chunks.push(token);
    }
    return { markdown: chunks.join("\n\n"), images };
  }

  /** Decode → scale → WebP-or-PNG encode locally, then persist through the typed IPC seam. */
  async importImage(
    ownerKind: NoteAttachmentOwnerKind,
    ownerId: string,
    image: LocalImageCandidate,
  ): Promise<NoteAttachmentDto> {
    const normalized = await this.normalizeForUpload(image.blob);
    const dataBase64 = await this.base64Url(normalized.blob);
    // The declared MIME must match the bytes (the backend re-detects and rejects a mismatch), and
    // the neutral filename keeps person/project names out of Markdown/shareable metadata.
    const fileName =
      normalized.mime === "image/png" ? "note-image.png" : "note-image.webp";
    return this.ipc.addNoteAttachment(
      ownerKind,
      ownerId,
      fileName,
      normalized.mime,
      dataBase64,
    );
  }

  /** Canonical, Obsidian-friendly marker stored in note markdown. */
  attachmentMarkdown(attachment: NoteAttachmentDto, alt: string): string {
    return `![${this.escapeAlt(alt)}](murmur-attachment://${attachment.id})`;
  }

  /** Best-effort orphan cleanup when a pending marker was removed mid-import. */
  deleteAttachment(
    ownerKind: NoteAttachmentOwnerKind,
    ownerId: string,
    attachmentId: string,
  ): Promise<void> {
    return this.ipc.deleteNoteAttachment(ownerKind, ownerId, attachmentId);
  }

  private planFromHtml(
    html: string,
    clipboardFiles: LocalImageCandidate[],
    maxImages: number,
    initiallySkippedTooMany: boolean,
  ): AttachmentPastePlan {
    const parsed = new DOMParser().parseFromString(html, "text/html");
    const remaining = [...clipboardFiles];
    const segments: AttachmentPasteSegment[] = [];
    const blockTags = new Set([
      "ADDRESS",
      "ARTICLE",
      "BLOCKQUOTE",
      "DIV",
      "FIGURE",
      "H1",
      "H2",
      "H3",
      "H4",
      "H5",
      "H6",
      "LI",
      "P",
      "PRE",
      "SECTION",
      "TR",
    ]);
    let text = "";
    let skippedExternalImages = false;
    let skippedUnsupportedImages = false;
    let skippedTooManyImages = initiallySkippedTooMany;
    let acceptedImages = 0;

    const flushText = (): void => {
      const normalized = this.normalizeClipboardText(text);
      if (normalized) {
        segments.push({ kind: "text", text: normalized });
      }
      text = "";
    };
    const lineBreak = (): void => {
      if (text && !text.endsWith("\n")) {
        text += "\n";
      }
    };
    const walk = (node: Node): void => {
      if (node.nodeType === Node.TEXT_NODE) {
        text += node.textContent ?? "";
        return;
      }
      if (!(node instanceof Element)) {
        return;
      }
      const tag = node.tagName;
      if (tag === "BR") {
        lineBreak();
        return;
      }
      if (tag === "IMG") {
        flushText();
        const src = (node.getAttribute("src") ?? "").trim();
        const alt = (node.getAttribute("alt") ?? "Pasted image").trim();
        // Enforce the owner cap before data-URL decoding (`atob`) or any bitmap allocation.
        if (acceptedImages >= maxImages) {
          remaining.shift();
          skippedTooManyImages = true;
          return;
        }
        const embedded = this.dataImageCandidate(src, alt);
        if (embedded) {
          // WebKit may expose the same clipboard image both as a data URL in
          // HTML and as an image File item. Consume that parallel
          // representation so a single pasted image is never duplicated.
          remaining.shift();
          segments.push({ kind: "image", image: embedded });
          acceptedImages += 1;
          return;
        }
        // Some apps expose the selected image bytes as a clipboard File while
        // their HTML still points at the original https URL. Prefer those
        // already-local bytes in DOM order; never fetch the URL ourselves.
        const local = remaining.shift();
        if (local) {
          segments.push({
            kind: "image",
            image: { ...local, alt: alt || local.alt },
          });
          acceptedImages += 1;
          return;
        }
        if (/^https?:\/\//i.test(src) || /^\/\//.test(src)) {
          skippedExternalImages = true;
          return;
        }
        skippedUnsupportedImages = true;
        return;
      }
      for (const child of Array.from(node.childNodes)) {
        walk(child);
      }
      if (blockTags.has(tag)) {
        lineBreak();
      }
    };

    for (const child of Array.from(parsed.body.childNodes)) {
      walk(child);
    }
    flushText();
    for (const image of remaining) {
      if (acceptedImages >= maxImages) {
        skippedTooManyImages = true;
        break;
      }
      segments.push({ kind: "image", image });
      acceptedImages += 1;
    }

    return {
      segments,
      skippedExternalImages,
      skippedUnsupportedImages,
      skippedTooManyImages,
    };
  }

  private imageFiles(
    data: DataTransfer,
    maxImages: number,
  ): { images: LocalImageCandidate[]; skippedTooManyImages: boolean } {
    const out: LocalImageCandidate[] = [];
    let skippedTooManyImages = false;
    for (const item of Array.from(data.items ?? [])) {
      if (item.kind !== "file" || !INPUT_IMAGE_TYPES.has(item.type.toLowerCase())) {
        continue;
      }
      if (out.length >= maxImages) {
        skippedTooManyImages = true;
        continue;
      }
      const file = item.getAsFile();
      if (file) {
        out.push(this.fileCandidate(file, "Screenshot"));
      }
    }
    if (out.length > 0) {
      return { images: out, skippedTooManyImages };
    }
    for (const file of Array.from(data.files ?? [])) {
      if (INPUT_IMAGE_TYPES.has(file.type.toLowerCase())) {
        if (out.length >= maxImages) {
          skippedTooManyImages = true;
          continue;
        }
        out.push(this.fileCandidate(file, "Screenshot"));
      }
    }
    return { images: out, skippedTooManyImages };
  }

  private fileCandidate(file: File, alt = "Image"): LocalImageCandidate {
    return {
      blob: file,
      // Keep the original name out of Markdown/shareable metadata: filenames
      // often contain person/project names. The backend receives a neutral hint.
      fileName: "note-image",
      alt,
    };
  }

  private dataImageCandidate(src: string, alt: string): LocalImageCandidate | null {
    const match = /^data:(image\/(?:png|jpeg|webp));base64,([a-z0-9+/=_-]+)$/i.exec(src);
    if (!match) {
      return null;
    }
    const estimatedBytes = Math.floor((match[2].length * 3) / 4);
    if (estimatedBytes > MAX_SOURCE_BYTES) {
      return null;
    }
    try {
      const standard = match[2].replace(/-/g, "+").replace(/_/g, "/");
      const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, "=");
      const binary = atob(padded);
      const bytes = new Uint8Array(binary.length);
      for (let i = 0; i < binary.length; i += 1) {
        bytes[i] = binary.charCodeAt(i);
      }
      const mimeType = match[1].toLowerCase();
      return {
        blob: new Blob([bytes], { type: mimeType }),
        fileName: `pasted-image.${mimeType === "image/jpeg" ? "jpg" : mimeType.slice(6)}`,
        alt: alt || "Pasted image",
      };
    } catch {
      return null;
    }
  }

  private async normalizeForUpload(
    sourceBlob: Blob,
  ): Promise<{ blob: Blob; mime: "image/webp" | "image/png"; width: number; height: number }> {
    const mimeType = sourceBlob.type.toLowerCase();
    if (!INPUT_IMAGE_TYPES.has(mimeType)) {
      throw new Error("Choose a PNG, JPEG, or WebP image.");
    }
    if (sourceBlob.size > MAX_SOURCE_BYTES) {
      throw new Error("That image is too large to process safely (24 MB maximum).");
    }

    const header = new Uint8Array(
      await sourceBlob.slice(0, MAX_DIMENSION_HEADER_BYTES).arrayBuffer(),
    );
    const expected = imageDimensions(mimeType, header);
    if (!expected || expected.width < 1 || expected.height < 1) {
      throw new Error("That image’s dimensions could not be validated safely.");
    }
    if (
      expected.width > MAX_SOURCE_EDGE ||
      expected.height > MAX_SOURCE_EDGE ||
      expected.width > MAX_SOURCE_PIXELS / expected.height
    ) {
      throw new Error("That image’s dimensions are too large to process safely.");
    }

    const decoded = await this.decodeImage(sourceBlob);
    try {
      if (decoded.width < 1 || decoded.height < 1) {
        throw new Error("That image could not be decoded.");
      }
      const sameOrientation =
        decoded.width === expected.width && decoded.height === expected.height;
      const rotatedOrientation =
        decoded.width === expected.height && decoded.height === expected.width;
      if (!sameOrientation && !rotatedOrientation) {
        throw new Error("That image’s decoded dimensions do not match its container.");
      }
      const initialScale = Math.min(1, MAX_EDGE / Math.max(decoded.width, decoded.height));
      let width = Math.max(1, Math.round(decoded.width * initialScale));
      let height = Math.max(1, Math.round(decoded.height * initialScale));
      const qualities = [0.86, 0.74, 0.62, 0.5, 0.4];
      let producedAnyBlob = false;

      for (let resizeAttempt = 0; resizeAttempt < 7; resizeAttempt += 1) {
        const canvas = document.createElement("canvas");
        canvas.width = width;
        canvas.height = height;
        const context = canvas.getContext("2d", { alpha: true });
        if (!context) {
          throw new Error("Image processing is unavailable in this window.");
        }
        context.drawImage(decoded.source, 0, 0, width, height);

        // Prefer WebP when the engine can genuinely encode it (Chromium today, WebKit later).
        // WebKit's `toBlob("image/webp")` silently returns a PNG-typed blob, so gate on the actual
        // returned type — never trust the requested MIME.
        for (const quality of qualities) {
          const output = await this.encodeCanvas(canvas, "image/webp", quality);
          if (!output) {
            break;
          }
          producedAnyBlob = true;
          if (output.type.toLowerCase() !== "image/webp") {
            // This engine cannot encode WebP (WebKit yields a PNG-typed blob). Lowering the quality
            // will not change that, so stop probing WebP and take the PNG fallback below.
            break;
          }
          if (output.size <= MAX_OUTPUT_BYTES) {
            return { blob: output, mime: "image/webp", width, height };
          }
        }

        // WebP unavailable → fall back to a metadata-free PNG (lossless, alpha-safe). Strip the
        // metadata chunks WebKit's own encoder emits so the backend's reject_png_metadata accepts it.
        const png = await this.encodeCanvas(canvas, "image/png");
        if (png) {
          producedAnyBlob = true;
          const stripped = stripPngMetadata(new Uint8Array(await png.arrayBuffer()));
          if (stripped.length <= MAX_OUTPUT_BYTES) {
            return {
              blob: new Blob([stripped], { type: "image/png" }),
              mime: "image/png",
              width,
              height,
            };
          }
        }

        // One shared scale preserves panoramas/portraits; each axis bottoms at
        // one pixel instead of independently clamping and distorting the image.
        const nextWidth = Math.max(1, Math.floor(width * 0.78));
        const nextHeight = Math.max(1, Math.floor(height * 0.78));
        if (nextWidth === width && nextHeight === height) {
          break;
        }
        width = nextWidth;
        height = nextHeight;
      }
      if (!producedAnyBlob) {
        throw new Error(
          "This browser can’t encode images for upload. Update macOS and try again.",
        );
      }
      throw new Error("That image is too detailed to fit the 3 MB note-image limit.");
    } finally {
      decoded.close();
    }
  }

  private async decodeImage(blob: Blob): Promise<{
    source: CanvasImageSource;
    width: number;
    height: number;
    close: () => void;
  }> {
    if (typeof createImageBitmap === "function") {
      try {
        const bitmap = await createImageBitmap(blob, { imageOrientation: "from-image" });
        return {
          source: bitmap,
          width: bitmap.width,
          height: bitmap.height,
          close: () => bitmap.close(),
        };
      } catch {
        // WKWebView builds without this decoder path fall back to a local blob URL.
      }
    }

    const image = document.createElement("img");
    image.decoding = "async";
    const dataUrl = await this.readAsDataUrl(blob);
    await new Promise<void>((resolve, reject) => {
      image.onload = () => resolve();
      image.onerror = () => reject(new Error("That image could not be decoded."));
      image.src = dataUrl;
    });
    return {
      source: image,
      width: image.naturalWidth,
      height: image.naturalHeight,
      close: () => undefined,
    };
  }

  /** CSP-safe local decoder fallback (`img-src data:` is already allow-listed). */
  private readAsDataUrl(blob: Blob): Promise<string> {
    return new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () =>
        typeof reader.result === "string"
          ? resolve(reader.result)
          : reject(new Error("That image could not be decoded."));
      reader.onerror = () => reject(new Error("That image could not be decoded."));
      reader.readAsDataURL(blob);
    });
  }

  /** Encode a canvas to a blob of the requested type, resolving null when the engine cannot. */
  private encodeCanvas(
    canvas: HTMLCanvasElement,
    type: "image/webp" | "image/png",
    quality?: number,
  ): Promise<Blob | null> {
    return new Promise<Blob | null>((resolve) => {
      canvas.toBlob((blob) => resolve(blob), type, quality);
    });
  }

  private async base64Url(blob: Blob): Promise<string> {
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = "";
    const chunkSize = 0x8000;
    for (let offset = 0; offset < bytes.length; offset += chunkSize) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
    }
    return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
  }

  private normalizeClipboardText(value: string): string {
    return value
      .replace(/\u00a0/g, " ")
      .replace(/[ \t]+\n/g, "\n")
      .replace(/\n{3,}/g, "\n\n")
      .trim();
  }

  private escapeAlt(alt: string): string {
    // Keep generated markers in the backend's exact single-line canonical grammar. Clipboard HTML
    // controls `alt`, so remove Markdown delimiters instead of letting `](` or a newline make the
    // just-imported image unexportable/unshareable.
    const safe = alt
      .replace(/[\r\n\t]+/g, " ")
      .replace(/[\\[\]()]/g, "")
      .replace(/\s{2,}/g, " ")
      .trim()
      .slice(0, 160);
    return safe || "Image";
  }

  private uuid(): string {
    if (typeof crypto.randomUUID === "function") {
      return crypto.randomUUID();
    }
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
  }
}
