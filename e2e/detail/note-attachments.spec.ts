import { expect, test, type Page } from "@playwright/test";
import { mockTauri } from "../settings-ai/mock-invoke";

/** Linux WebKit lacks the codec; this test seam exercises the post-encode UI/IPC flow. */
async function mockCanvasWebpEncoder(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const original = HTMLCanvasElement.prototype.toBlob;
    HTMLCanvasElement.prototype.toBlob = function (
      callback: BlobCallback,
      type?: string,
      quality?: number,
    ): void {
      if (type === "image/webp") {
        callback(new Blob([new Uint8Array([0x52, 0x49, 0x46, 0x46])], { type }));
        return;
      }
      original.call(this, callback, type, quality);
    };
  });
}

test.describe("Detail — meeting note attachments", () => {
  test("Cmd-V keeps an image at the caret, saves its marker, and renders it", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") consoleErrors.push(message.text());
    });
    page.on("pageerror", (error) => consoleErrors.push(String(error)));
    await mockCanvasWebpEncoder(page);
    await mockTauri(page, {
      get_meeting_detail: () => ({
        meeting: {
          id: "m-image",
          startedAt: "2026-07-21T09:00:00Z",
          endedAt: "2026-07-21T09:30:00Z",
          title: "Image meeting",
          durationS: 1800,
          audioPath: null,
          status: "EXPORTED",
          folderId: null,
        },
        note: {
          meetingId: "m-image",
          providerId: "claude_code",
          markdown: "# Meeting note\n\nAlpha beta",
          exportedPath: null,
        },
        segments: [],
        assistantInteractions: [],
        locked: false,
        aiProvider: "claude_code",
        aiModel: null,
        modelServed: null,
      }),
      get_note_receipts: () => [],
      list_note_attachments: (args: { ownerKind: string; ownerId: string }) =>
        ((window as any).__meetingAttachments ?? []).filter(
          (row: any) =>
            row.ownerKind === args.ownerKind && row.ownerId === args.ownerId,
        ),
      add_note_attachment: (args: any) => {
        const standard = String(args.dataBase64)
          .replace(/-/g, "+")
          .replace(/_/g, "/");
        const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, "=");
        const row = {
          id: crypto.randomUUID(),
          ownerKind: args.ownerKind,
          ownerId: args.ownerId,
          mimeType: args.mimeType,
          extension: "webp",
          byteLen: Math.floor((padded.length * 3) / 4),
          width: 1,
          height: 1,
          sha256: "demo",
          dataUrl: `data:image/webp;base64,${padded}`,
        };
        (window as any).__meetingAttachments = [
          ...((window as any).__meetingAttachments ?? []),
          row,
        ];
        (window as any).__meetingAttachmentArgs = args;
        return row;
      },
      delete_note_attachment: (args: any) => {
        (window as any).__meetingAttachments = (
          (window as any).__meetingAttachments ?? []
        ).filter((row: any) => row.id !== args.attachmentId);
        (window as any).__deletedMeetingAttachmentIds = [
          ...((window as any).__deletedMeetingAttachmentIds ?? []),
          args.attachmentId,
        ];
      },
      update_note: (args: any) => {
        (window as any).__savedMeetingMarkdown = args.markdown;
        return {
          meetingId: args.meetingId,
          providerId: "claude_code",
          markdown: args.markdown,
          exportedPath: null,
        };
      },
    });

    await page.goto("/meeting/m-image");
    await page.getByRole("button", { name: "Edit", exact: true }).click();

    const editor = page.locator(".editor-area");
    await editor.evaluate((el: HTMLTextAreaElement) => {
      const at = el.value.indexOf("beta");
      el.focus();
      el.setSelectionRange(at, at);
      const png = Uint8Array.from(
        atob(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
        ),
        (char) => char.charCodeAt(0),
      );
      const transfer = new DataTransfer();
      transfer.items.add(
        new File([png], "confidential-board.png", { type: "image/png" }),
      );
      el.dispatchEvent(
        new ClipboardEvent("paste", {
          bubbles: true,
          cancelable: true,
          clipboardData: transfer,
        }),
      );
    });

    await expect(editor).toHaveValue(
      /!\[Screenshot\]\(murmur-attachment:\/\/[0-9a-f-]{36}\)/i,
    );
    const draft = await editor.inputValue();
    expect(draft).not.toContain("confidential-board");
    expect(draft.indexOf("murmur-attachment://")).toBeLessThan(
      draft.indexOf("beta"),
    );

    const ipcArgs = await page.evaluate(
      () => (window as any).__meetingAttachmentArgs,
    );
    expect(ipcArgs).toMatchObject({
      ownerKind: "meeting",
      ownerId: "m-image",
      fileName: "note-image.webp",
      mimeType: "image/webp",
    });
    expect(ipcArgs.dataBase64).toMatch(/^[A-Za-z0-9_-]+$/);

    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.locator(".meeting-markdown-note .md-attachment img")).toHaveCount(1);

    const saved = await page.evaluate(
      () => (window as any).__savedMeetingMarkdown as string,
    );
    expect(saved).toContain("murmur-attachment://");
    expect(saved).not.toContain("murmur-pending://");
    expect(saved).not.toContain("confidential-board");

    // A second image imported in a later edit is a draft resource. Cancel must delete it instead of
    // silently consuming the per-note cap or leaving an unreferenced SQLCipher/vault attachment.
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await editor.evaluate((el: HTMLTextAreaElement) => {
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
      const png = Uint8Array.from(
        atob(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
        ),
        (char) => char.charCodeAt(0),
      );
      const transfer = new DataTransfer();
      transfer.items.add(new File([png], "discard-me.png", { type: "image/png" }));
      el.dispatchEvent(
        new ClipboardEvent("paste", {
          bubbles: true,
          cancelable: true,
          clipboardData: transfer,
        }),
      );
    });
    await expect(editor).toHaveValue(/murmur-attachment:\/\/[0-9a-f-]{36}[\s\S]*murmur-attachment:\/\//i);
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.locator(".meeting-markdown-note .md-attachment img")).toHaveCount(1);
    expect(
      await page.evaluate(
        () => ((window as any).__deletedMeetingAttachmentIds ?? []).length,
      ),
    ).toBe(1);
    expect(consoleErrors).toEqual([]);
  });
});
