import type { Page } from '@playwright/test';

/** Transport fixture for the producer DTO in commands/related_picker.rs.
 * The tree here is test data only; the shipping picker must never call list_workspace_tree.
 */
export async function mockDestinationPicker(page: Page, fixtureForest?: unknown[]): Promise<void> {
  await page.addInitScript((fixtureForest) => {
    const host = window as unknown as {
      __TAURI_INTERNALS__: { invoke: (cmd: string, args?: any) => Promise<any> };
      __destinationReads?: any[];
    };
    const original = host.__TAURI_INTERNALS__.invoke.bind(host.__TAURI_INTERNALS__);
    const availability = (values: Record<string, boolean> = {}) => ({
      selectable: true, here: false, self: false, descendant: false,
      locked: false, confirm: false, incompatible: false, ...values,
    });
    const bootstrap = async (args: any) => {
      const forest = fixtureForest ?? await original('list_workspace_tree');
      let current: string | null = null;
      let currentPath: string[] = [];
      let source: any = null;
      const subtree: string[] = [];
      const locate = (nodes: any[], path: string[]) => {
        for (const node of nodes) {
          if (node.locked && !node.unlocked) continue;
          if (args.anchorKind === 'container' && node.id === args.anchorId) {
            current = path.at(-1) ?? null;
            currentPath = path;
            source = node;
            const collect = (n: any) => { subtree.push(n.id); n.folders.forEach(collect); };
            collect(node);
          }
          for (const group of node.groups ?? []) {
            if (group.items?.some((item: any) => item.id === args.anchorId)) {
              current = node.id;
              currentPath = [...path, node.id];
            }
          }
          locate(node.folders ?? [], [...path, node.id]);
        }
      };
      locate(forest, []);
      if (!source && !current && args.anchorKind === 'note') {
        const note = await original('get_note', { id: args.anchorId });
        current = note?.folderId ?? null;
      }
      const map = (node: any, parentLocked = false): any => {
        const sealed = node.locked && !node.unlocked;
        const encrypted = parentLocked || node.locked;
        const self = node.id === args.anchorId && args.anchorKind === 'container';
        const descendant = subtree.includes(node.id) && !self;
        const here = node.id === current;
        const incompatible = !!encrypted && ["org", "sharedContainer", "task", "dashboard", "container"].includes(args.anchorKind);
        return {
          id: node.id, name: node.name, level: node.level, emoji: node.emoji ?? null,
          locked: !!node.locked, unlocked: !!node.unlocked, linkable: !sealed,
          groups: sealed ? [] : (node.groups ?? []).map((g: any) => ({kind: g.kind, total: g.total})),
          folders: sealed ? [] : (node.folders ?? []).map((n: any) => map(n, encrypted)),
          availability: availability({selectable: !sealed && !here && !self && !descendant && !incompatible,
            here, self, descendant, incompatible, locked: sealed, confirm: !!encrypted}),
        };
      };
      return {
        spaces: forest.map((n: any) => map(n)), unclassified: [], anchor: null,
        destination: {
          sourceKind: args.anchorKind, sourceLocked: false, currentContainerId: current,
          currentPath, root: {
            kind: args.anchorKind === 'note' || args.anchorKind === 'document' ? 'notesRoot' : 'unfiled',
            label: 'All notes',
            containerId: args.anchorKind === 'note' || args.anchorKind === 'document' ? 'notes-root' : null,
            availability: availability({selectable: current !== null, here: current === null}),
          },
          container: source ? {id: source.id, level: source.level, parentId: current,
            pathIds: [...currentPath, source.id], subtreeIds: subtree} : null,
        },
      };
    };
    host.__TAURI_INTERNALS__.invoke = async (cmd, args) => {
      if (!['get_related_picker_bootstrap', 'list_related_picker_items', 'search_related_picker'].includes(cmd) || args?.mode !== 'destination') {
        return original(cmd, args);
      }
      (host.__destinationReads ??= []).push({cmd, args});
      if (cmd === 'get_related_picker_bootstrap') return bootstrap(args);
      if (cmd === 'list_related_picker_items') return {kind: args.kind, offset: args.offset, total: 0, items: []};
      const data = await bootstrap(args);
      const matches: any[] = [];
      const walk = (nodes: any[], path: string[]) => {
        for (const node of nodes) {
          const breadcrumb = [...path, node.name];
          if (breadcrumb.join(' / ').toLowerCase().includes(args.query.toLowerCase())) {
            matches.push({id: node.id, name: node.name, level: node.level, breadcrumb, availability: node.availability});
          }
          walk(node.folders, breadcrumb);
        }
      };
      walk(data.spaces, []);
      return {offset: args.offset, hits: [], containers: matches.slice(args.offset, args.offset + args.limit), total: matches.length};
    };
  }, fixtureForest);
}
