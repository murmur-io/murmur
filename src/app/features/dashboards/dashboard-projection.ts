import type { ResolvedTile, SourceRef, TileRow } from "../../core/models";

export type DashboardLens =
  | "brief"
  | "overview"
  | "commitments"
  | "sources"
  | "people";

export interface MaterialSourceView {
  id: string;
  kind: "note" | "meeting" | "document";
  label: "Note" | "Recording" | "Document";
  title: string;
  detail: string;
  source: SourceRef;
  sortAt: number | null;
}

export interface CommitmentView {
  id: string;
  text: string;
  meta: string | null;
  status: string | null;
  source: SourceRef | null;
  category: "Promise" | "Reminder";
}

export interface PersonView {
  id: string;
  name: string;
  mentionCount: number;
  openCommitments: number;
}

export interface SupersededStateView {
  id: string;
  entity: string;
  predicate: string;
  value: string;
  meta: string | null;
  source: SourceRef | null;
}

export interface BoardProjection {
  dominant: {
    label: string;
    title: string;
    detail: string;
    answeredAt: string | null;
    source: SourceRef | null;
    answerRefresh: { tileId: string; question: string } | null;
  };
  attention: CommitmentView[];
  evidence: MaterialSourceView[];
  commitments: CommitmentView[];
  sources: MaterialSourceView[];
  people: PersonView[];
  supersededStates: SupersededStateView[];
  readableMaterialCount: number;
  derivedViewCount: number;
  sealedCount: number;
  missingCount: number;
  hasGoodZero: boolean;
}

const MATERIAL_KINDS = new Set(["note", "meeting", "document"]);
const ATTENTION_STATUSES = new Set(["late", "due"]);

function safeText(value: unknown, fallback: string): string {
  return typeof value === "string" && value.trim() ? value.trim() : fallback;
}

function sourceFromTile(tile: ResolvedTile): MaterialSourceView | null {
  const data = tile.data;
  switch (data.kind) {
    case "note":
      if (typeof data.id !== "string" || data.id === "") return null;
      return {
        id: `${data.kind}:${data.id}`,
        kind: data.kind,
        label: "Note",
        title: safeText(data.title, "Untitled note"),
        detail: safeText(data.snippet, "No note text yet."),
        source: { kind: "note", id: data.id },
        sortAt: Number.isFinite(data.updatedAt) ? data.updatedAt : null,
      };
    case "meeting":
      if (typeof data.id !== "string" || data.id === "") return null;
      return {
        id: `${data.kind}:${data.id}`,
        kind: data.kind,
        label: "Recording",
        title: safeText(data.title, "Untitled recording"),
        detail: data.hasAudio
          ? "Audio is available."
          : "Audio is unavailable.",
        source: { kind: "meeting", id: data.id },
        sortAt:
          typeof data.startedAt === "string" && Number.isFinite(Date.parse(data.startedAt))
            ? Date.parse(data.startedAt)
            : null,
      };
    case "document":
      if (typeof data.id !== "string" || data.id === "") return null;
      return {
        id: `${data.kind}:${data.id}`,
        kind: data.kind,
        label: "Document",
        title: safeText(data.title, "Untitled document"),
        detail: safeText(data.snippet, "No extractable text."),
        source: { kind: "document", id: data.id },
        sortAt: null,
      };
    default:
      return null;
  }
}

function rowViews(
  tile: ResolvedTile,
  rows: TileRow[],
  category: CommitmentView["category"],
): CommitmentView[] {
  return rows.map((row, index) => ({
    id: `${tile.id}:${index}`,
    text: row.text,
    meta: row.meta,
    status: row.status,
    source: row.source,
    category,
  }));
}

/**
 * Build every Read lens from the already-resolved, already-gated board payload.
 * A locked tile contributes one number only; no stored chrome or source metadata
 * is copied into the projection, which keeps every Read representation generic.
 */
export function projectDashboard(tiles: readonly ResolvedTile[]): BoardProjection {
  const sources: MaterialSourceView[] = [];
  const seenSources = new Set<string>();
  const commitments: CommitmentView[] = [];
  const people: PersonView[] = [];
  const supersededStates: SupersededStateView[] = [];
  let sealedCount = 0;
  let missingCount = 0;
  let derivedViewCount = 0;
  let hasGoodZero = false;
  let livingAnswer: {
    tileId: string;
    title: string;
    detail: string;
    answeredAt: string | null;
  } | null = null;

  for (const tile of tiles) {
    const data = tile.data;
    if (data.kind === "locked") {
      sealedCount += 1;
      continue;
    }
    if (data.kind === "missing" || data.kind === "unconfigured") {
      missingCount += 1;
      continue;
    }

    const material = sourceFromTile(tile);
    if (material) {
      if (!seenSources.has(material.id)) {
        seenSources.add(material.id);
        sources.push(material);
      }
      continue;
    }

    derivedViewCount += 1;
    switch (data.kind) {
      case "promises":
        commitments.push(...rowViews(tile, data.rows, "Promise"));
        hasGoodZero ||= data.rows.length === 0;
        break;
      case "reminders":
        commitments.push(...rowViews(tile, data.rows, "Reminder"));
        hasGoodZero ||= data.rows.length === 0;
        break;
      case "person":
        people.push({
          id: data.id,
          name: data.name,
          mentionCount: data.mentionCount,
          openCommitments: data.openCommitments,
        });
        break;
      case "livingAnswer":
        if (!livingAnswer && !data.withheld && data.question.trim()) {
          livingAnswer = {
            tileId: tile.id,
            title: data.question,
            detail:
              data.answer ??
              "No saved answer yet. Refresh it from this board's current readable scope.",
            answeredAt: data.answeredAt,
          };
        }
        break;
      case "drift":
        for (const [index, row] of data.rows.entries()) {
          // `old` is an explicit backend-resolved bitemporal state: this value
          // was superseded. It is the wire's honest past-state signal, distinct from
          // a missing source, sealed payload, or useful zero.
          if (row.status === "old") {
            supersededStates.push({
              id: `${tile.id}:${index}`,
              entity: data.entity,
              predicate: data.predicate,
              value: row.text,
              meta: row.meta,
              source: row.source,
            });
          }
        }
        break;
      default:
        break;
    }
  }

  const attention = commitments
    .filter((row) => row.status !== null && ATTENTION_STATUSES.has(row.status))
    .slice(0, 5);
  const evidence = [...sources]
    .sort((a, b) => {
      if (a.sortAt === null && b.sortAt === null) return 0;
      if (a.sortAt === null) return 1;
      if (b.sortAt === null) return -1;
      return b.sortAt - a.sortAt;
    })
    .slice(0, 4);
  const firstSource = sources[0] ?? null;
  const dominant = livingAnswer
    ? {
        label: "Cached answer",
        title: livingAnswer.title || "Living answer",
        detail: livingAnswer.detail,
        answeredAt: livingAnswer.answeredAt,
        source: null,
        answerRefresh: {
          tileId: livingAnswer.tileId,
          question: livingAnswer.title,
        },
      }
    : firstSource
      ? {
          label: "Current source",
          title: firstSource.title,
          detail: firstSource.detail,
          answeredAt: null,
          source: firstSource.source,
          answerRefresh: null,
        }
      : commitments.length === 0 && hasGoodZero
        ? {
            label: "Current state",
            title: "Nothing open",
            detail:
              "The board's configured commitment views have no open items.",
            answeredAt: null,
            source: null,
            answerRefresh: null,
          }
        : {
            label: "Current state",
            title: "No readable material yet",
            detail:
              "Add a note, recording or document, or unlock a sealed source.",
            answeredAt: null,
            source: null,
            answerRefresh: null,
          };

  return {
    dominant,
    attention,
    evidence,
    commitments,
    sources,
    people,
    supersededStates,
    readableMaterialCount: sources.length,
    derivedViewCount,
    sealedCount,
    missingCount,
    hasGoodZero,
  };
}

export function isMaterialTile(tile: ResolvedTile): boolean {
  return MATERIAL_KINDS.has(tile.data.kind);
}
