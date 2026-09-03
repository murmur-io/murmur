import type { ContainerNode } from "./models";

/**
 * A word the USER reads for a level of the hierarchy.
 *
 * Deliberately distinct from the code's `level` (`"project" | "folder"`) and from a `kind`
 * (`"space" | "note" | …`): a domain identifier must not typecheck where a sentence is written,
 * which is how the two vocabularies drifted apart in the first place.
 */
export type ContainerNoun = "Workspace" | "folder";

/**
 * The ONE user-facing name for each level of the workspace hierarchy.
 *
 * WHY THIS EXISTS (2026-09-02 audit, F5). The same `level === "project"` test was written in four
 * places and produced four different words for two concepts: `"Workspace" / "Folder"` in the create
 * sheet, `"Workspace" / "folder"` in the share sheet, `"space" / "folder"` in the tree's menus and
 * toasts, and a bare `"container"` in the container view's own empty and error states. A user
 * renaming a thing was told they renamed a "space"; the sheet that made it called it a "Workspace";
 * the view of it called it a "container". Same object, three names, one session.
 *
 * `Workspace` is capitalised because that is what the product already calls it where it names it
 * most — "No Workspaces yet", "Created Workspace" — and `folder` is lowercase for the same reason
 * ("Move to Workspace or folder…"). The choice is which existing word wins, not a new one.
 *
 * The DTO's own term is `"project"`, and `container` is the code's word for "either of these". Both
 * are fine in code and neither belongs in a sentence a user reads; `scripts/check-vocabulary.mjs`
 * now fails a build that puts them there.
 */
export function containerNoun(
  container: Pick<ContainerNode, "level">,
): ContainerNoun {
  return container.level === "project" ? "Workspace" : "folder";
}

/**
 * The same noun at the start of a sentence.
 *
 * Only `folder` changes — `Workspace` is a proper noun and is already capitalised, so a naive
 * `charAt(0).toUpperCase()` at each call site would have been right by accident here and wrong the
 * moment the vocabulary gains a term that is capitalised mid-sentence.
 */
export function containerNounLeading(
  container: Pick<ContainerNode, "level">,
): "Workspace" | "Folder" {
  return container.level === "project" ? "Workspace" : "Folder";
}
