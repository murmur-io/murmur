import type { ContainerNode } from "../../../core/models";

export interface RecordingDestination {
  readonly id: string;
  readonly label: string;
  readonly level: "project" | "folder";
  readonly emoji: string | null;
  /** Active plaintext cannot enter this row or a descendant of a locked row. */
  readonly blocked: boolean;
}

/**
 * Flatten the gated workspace tree for every recording destination picker.
 *
 * The backend remains the write authority. This helper owns the matching render
 * policy so the Record route's post-final picker and the 58px floating bar cannot drift:
 * session-unlocked sealed rows (and their descendants) remain visible but
 * disabled, while descendants of sealed-not-unlocked rows are never disclosed.
 */
export function flattenRecordingDestinations(
  forest: readonly ContainerNode[],
): RecordingDestination[] {
  const rows: RecordingDestination[] = [];

  const visit = (
    node: ContainerNode,
    ancestors: readonly string[],
    lockedAncestor: boolean,
  ): void => {
    const labels = [...ancestors, node.name];
    const blocked = lockedAncestor || node.locked;
    // The reserved Notes home is structural, not a filing destination. Its
    // ordinary children remain valid mixed-content destinations, so keep
    // traversing and preserve the root name in their breadcrumbs.
    if (!node.isRoot) {
      rows.push({
        id: node.id,
        label: labels.join(" / "),
        level: node.level,
        emoji: node.emoji,
        blocked,
      });
    }

    if (node.locked && !node.unlocked) {
      return;
    }
    for (const child of node.folders) {
      visit(child, labels, blocked);
    }
  };

  for (const space of forest) {
    visit(space, [], false);
  }
  return rows;
}
