import { expect, test, type Page } from '@playwright/test';
import { mockNotes } from './mock-invoke';
import { mockDestinationPicker } from './destination-picker-mock';

const FOREST = [{id:'current', name:'Current Space', kind:'note', level:'project',
  locked:false, unlocked:false, isRoot:false, emoji:null, tint:null, groups:[], folders:[]},
  {id:'target', name:'Target Space', kind:'note', level:'project', locked:false,
    unlocked:false, isRoot:false, emoji:null, tint:null, groups:[], folders:[
      {id:'nested', name:'Deep folder', kind:'note', level:'folder', locked:false,
        unlocked:false, isRoot:false, emoji:null, tint:null, groups:[], folders:[]},
      {id:'encrypted', name:'Private folder', kind:'note', level:'folder', locked:true,
        unlocked:true, isRoot:false, emoji:null, tint:null, groups:[], folders:[]},
    ]}];

async function boot(page: Page) {
  await mockNotes(page, {
    move_note: (args: unknown) => { ((window as any).__moves ??= []).push(args); return null; },
    move_note_doc: (args: unknown) => { ((window as any).__moves ??= []).push(args); return null; },
  }, [], { list_workspace_tree: FOREST });
  await mockDestinationPicker(page, FOREST);
}
async function selectDeep(page: Page) {
  const dialog = page.getByRole('dialog', {name: /^Move /});
  await expect(dialog.getByRole('tree')).toBeVisible();
  await expect(dialog.getByRole('button', {name:'Choose Target Space', exact:true})).toBeVisible();
  await dialog.getByRole('searchbox', {name:'Search destinations'}).fill('Deep folder');
  await expect(dialog.getByText('Target Space / Deep folder', {exact:true})).toBeVisible();
  await dialog.getByRole('button', {name:'Choose Deep folder', exact:true}).click();
  await dialog.getByRole('button', {name:'Move here', exact:true}).click();
  await expect(dialog).toHaveCount(0);
}

test('meeting detail More opens the full destination hierarchy and writes the selected target', async ({page}) => {
  await boot(page);
  await page.goto('/meeting/m-atlas-roadmap');
  await page.getByTestId('meeting-command-bar').getByRole('button', {name:'More', exact:true}).click();
  await page.getByRole('menuitem', {name:'Move…', exact:true}).click();
  await selectDeep(page);
  expect(await page.evaluate(() => (window as any).__moves)).toEqual([
    {meetingId:'m-atlas-roadmap', folderId:'nested', confirmedEncryptionBoundary:false},
  ]);
});

test('notes list uses the complete searchable hierarchy', async ({page}) => {
  await boot(page);
  await page.goto('/notes');
  await page.getByRole('row').filter({hasText:'My First Note'}).getByRole('button', {name:'Move note', exact:true}).click();
  await selectDeep(page);
  expect(await page.evaluate(() => (window as any).__moves)).toEqual([
    {id:'n1', folderId:'nested', confirmedEncryptionBoundary:false},
  ]);
});

test('note editor root choice preserves the canonical Notes root write contract', async ({page}) => {
  await boot(page);
  await page.goto('/notes/n1');
  const trigger = page.locator('.crumb-btn[aria-haspopup="dialog"]');
  await trigger.click();
  const dialog = page.getByRole('dialog', {name:/^Move /});
  await dialog.getByRole('button', {name:'Choose All notes', exact:true}).click();
  await dialog.getByRole('button', {name:'Move here', exact:true}).click();
  await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__moves)).toEqual([
    {id:'n1', folderId:'notes-root', confirmedEncryptionBoundary:false},
  ]);
  await expect(trigger).toBeFocused();
});

test('encryption boundary requires confirmation, traps focus, cancels safely and sends a witness', async ({page}) => {
  await boot(page);
  await page.goto('/notes/n1');
  await page.locator('.crumb-btn[aria-haspopup="dialog"]').click();
  const dialog = page.getByRole('dialog', {name:/^Move /});
  await dialog.getByRole('searchbox', {name:'Search destinations'}).fill('Private folder');
  await dialog.getByRole('button', {name:'Choose Private folder', exact:true}).click();
  await dialog.getByRole('button', {name:'Move here', exact:true}).click();
  const confirm = page.getByRole('alertdialog', {name:'Move into a locked folder?'});
  await expect(confirm).toContainText('removes its plaintext Markdown');
  await expect(confirm.getByRole('button', {name:'Cancel', exact:true})).toBeFocused();
  await page.keyboard.press('Shift+Tab');
  await expect(confirm.getByRole('button', {name:'Encrypt & move', exact:true})).toBeFocused();
  await page.keyboard.press('Tab');
  await expect(confirm.getByRole('button', {name:'Cancel', exact:true})).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(confirm).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__moves ?? [])).toEqual([]);
  await dialog.getByRole('button', {name:'Move here', exact:true}).click();
  await confirm.getByRole('button', {name:'Encrypt & move', exact:true}).click();
  await expect(dialog).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__moves)).toEqual([
    {id:'n1', folderId:'encrypted', confirmedEncryptionBoundary:true},
  ]);
});

test('destination leaves stay visible and inert while sibling pages load lazily', async ({page}) => {
  await mockNotes(page, {
    get_related_picker_bootstrap: () => ({
      spaces: [{id:'nf1', name:'Notes', level:'project', emoji:null, locked:false,
        unlocked:false, linkable:true, groups:[{kind:'note', total:50}], folders:[],
        availability:{selectable:false, here:true, self:false, descendant:false, locked:false, confirm:false, incompatible:false}}],
      unclassified:[],
      anchor:{kind:'note', containerId:'nf1', path:['nf1'], index:0, offset:0,
        items:Array.from({length:24}, (_,i) => ({kind:'note', id:i === 0 ? 'n1' : `n-${i}`, title:`Context ${i}`})), total:50},
      destination:{sourceKind:'note', sourceLocked:false, currentContainerId:'nf1', currentPath:['nf1'], container:null,
        root:{kind:'notesRoot', label:'All notes', containerId:'notes-root',
          availability:{selectable:true, here:false, self:false, descendant:false, locked:false, confirm:false, incompatible:false}}},
    }),
    list_related_picker_items: (args: {offset:number; limit:number; mode:string}) => {
      ((window as any).__pages ??= []).push(args);
      return {kind:'note', offset:args.offset, total:50,
        items:Array.from({length:Math.min(args.limit, 50 - args.offset)},(_,i)=>({kind:'note',id:`n-${args.offset+i}`,title:`Context ${args.offset+i}`}))};
    },
  });
  await page.goto('/notes/n1');
  await page.locator('.crumb-btn[aria-haspopup="dialog"]').click();
  const dialog = page.getByRole('dialog', {name:/^Move /});
  const leaf = dialog.locator('[data-row="i:note:n1"] .rhp-row-main');
  await expect(leaf).toHaveAttribute('aria-disabled', 'true');
  await expect(leaf).toHaveAttribute('aria-posinset', '1');
  await expect(leaf).toHaveAttribute('aria-setsize', '50');
  await leaf.focus();
  await leaf.press('Enter');
  await expect(dialog.getByRole('button', {name:'Move here', exact:true})).toBeDisabled();
  await dialog.getByRole('button', {name:'Load more', exact:true}).click();
  await expect(dialog.getByText('Context 47', {exact:true})).toBeVisible();
  await dialog.getByRole('button', {name:'Load more', exact:true}).click();
  await expect(dialog.getByText('Context 49', {exact:true})).toBeVisible();
  const lastLeaf = dialog.locator('[data-row="i:note:n-49"] .rhp-row-main');
  await expect(lastLeaf).toHaveAttribute('aria-posinset', '50');
  await expect(lastLeaf).toHaveAttribute('aria-setsize', '50');
  expect(await page.evaluate(() => (window as any).__pages.map((p: any) => ({offset:p.offset, limit:p.limit, mode:p.mode})))).toEqual([
    {offset:24, limit:24, mode:'destination'}, {offset:48, limit:24, mode:'destination'},
  ]);
});

test('a failed destination search can retry without losing its query', async ({page}) => {
  await boot(page);
  await page.addInitScript(() => {
    const host = window as any;
    const invoke = host.__TAURI_INTERNALS__.invoke.bind(host.__TAURI_INTERNALS__);
    let failed = false;
    host.__TAURI_INTERNALS__.invoke = (cmd: string, args: any) => {
      if (cmd === 'search_related_picker' && args.mode === 'destination' && !failed) {
        failed = true;
        return Promise.reject(new Error('temporary search failure'));
      }
      return invoke(cmd,args);
    };
  });
  await page.goto('/notes/n1');
  await page.locator('.crumb-btn[aria-haspopup="dialog"]').click();
  const dialog = page.getByRole('dialog', {name:/^Move /});
  const search = dialog.getByRole('searchbox', {name:'Search destinations'});
  await search.fill('Deep folder');
  await expect(dialog.getByRole('alert')).toContainText('Couldn’t search destinations');
  await dialog.getByRole('button', {name:'Retry search', exact:true}).click();
  await expect(search).toHaveValue('Deep folder');
  await expect(dialog.getByText('Target Space / Deep folder', {exact:true})).toBeVisible();
  await dialog.getByRole('button', {name:'Choose Deep folder', exact:true}).click();
  await dialog.getByRole('button', {name:'Move here', exact:true}).click();
  await expect(dialog).toHaveCount(0);
});

test('closing a meeting move returns focus to the surviving More trigger', async ({page}) => {
  await boot(page);
  await page.goto('/meeting/m-atlas-roadmap');
  const more = page.getByTestId('meeting-command-bar').getByRole('button', {name:'More', exact:true});
  await more.click();
  await page.getByRole('menuitem', {name:'Move…', exact:true}).click();
  await expect(page.getByRole('dialog', {name:/^Move /})).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog', {name:/^Move /})).toHaveCount(0);
  await expect(more).toBeFocused();
});

test('a session-readable document with a refused destination gate never offers Move', async ({page}) => {
  await mockNotes(page, {
    list_links: () => [{id:81,direction:'out',otherKind:'document',otherId:'session-doc',
      otherTitle:'Session readable source',edgeType:'manual',createdBy:'user',status:'active',
      score:1,createdAt:1720000000,manual:true}],
    get_backlinks: () => [],
    get_document: () => 'Session-readable document content',
    get_related_picker_bootstrap: (args: unknown) => {
      ((window as any).__sourceGates ??= []).push(args);
      return Promise.reject(new Error('locked'));
    },
  });
  await page.goto('/notes/n1');
  const expand = page.getByRole('button', {name:'Show related items and suggestions', exact:true});
  await expect(expand).toBeVisible();
  await expand.click();
  await page.getByRole('button', {name:'Open document Session readable source', exact:true}).click();
  const preview = page.getByRole('dialog', {name:'Preview of Session readable source'});
  await expect(preview).toContainText('Session-readable document content');
  await expect(preview.getByRole('button', {name:'Move…', exact:true})).toHaveCount(0);
  expect(await page.evaluate(() => (window as any).__sourceGates)).toContainEqual({anchorKind:'document',anchorId:'session-doc',mode:'destination'});
});

test('a narrow destination picker keeps its selected path and commit visible', async ({page}) => {
  await page.setViewportSize({width:390,height:640});
  await boot(page);
  await page.goto('/notes/n1');
  await page.locator('.crumb-btn[aria-haspopup="dialog"]').click();
  const dialog = page.getByRole('dialog', {name:/^Move /});
  await dialog.getByRole('searchbox', {name:'Search destinations'}).fill('Deep folder');
  await dialog.getByRole('button', {name:'Choose Deep folder',exact:true}).click();
  await expect(dialog.locator('.rhp-foot-copy')).toContainText('Target Space / Deep folder');
  await expect(dialog.locator('.rhp-foot-copy')).toBeVisible();
  const commit = dialog.getByRole('button', {name:'Move here',exact:true});
  const box = await commit.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(390);
  await commit.click();
  await expect(dialog).toHaveCount(0);
});
