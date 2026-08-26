import type { ContainerNode } from "../../core/models";

export interface WorkspaceDestination {
  readonly container: ContainerNode;
  readonly label: string;
}

/** Flatten the rendered forest while preserving a full, duplicate-safe breadcrumb for each row. */
export function workspaceDestinations(
  forest: readonly ContainerNode[],
): WorkspaceDestination[] {
  const rawDestinations: WorkspaceDestination[] = [];
  const visit = (container: ContainerNode, ancestors: readonly string[]): void => {
    // Treat a sealed, not-session-unlocked container as an intrinsic leaf even
    // when a stale payload still carries descendants. The tree renderer applies
    // the same privacy boundary; destination pickers must not leak child names
    // or breadcrumbs that the user is no longer authorised to see.
    if (container.locked && !container.unlocked) {
      return;
    }
    const labels = [...ancestors, container.name];
    rawDestinations.push({ container, label: labels.join(" / ") });
    container.folders.forEach((child) => visit(child, labels));
  };
  forest.forEach((root) => visit(root, []));

  const labelCounts = new Map<string, number>();
  rawDestinations.forEach(({ label }) => {
    labelCounts.set(label, (labelCounts.get(label) ?? 0) + 1);
  });
  const occurrences = new Map<string, number>();

  return rawDestinations.map((destination) => {
    if ((labelCounts.get(destination.label) ?? 0) < 2) {
      return destination;
    }
    const occurrence = (occurrences.get(destination.label) ?? 0) + 1;
    occurrences.set(destination.label, occurrence);
    return { ...destination, label: `${destination.label} (${occurrence})` };
  });
}
