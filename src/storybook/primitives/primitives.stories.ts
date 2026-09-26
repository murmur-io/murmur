import { type Meta, type StoryObj, moduleMetadata } from "@storybook/angular";

import { MurIconComponent } from "../../app/design-system/icon/icon.component";

/**
 * The class-based primitives in `src/app/design-system/primitives.css`
 * (imported globally by `styles.css`). These are the control language every
 * component and feature shares — reach for one of these before re-declaring a
 * button or a menu in component SCSS.
 */
const meta: Meta = {
  title: "Primitives",
  tags: ["autodocs"],
  decorators: [moduleMetadata({ imports: [MurIconComponent] })],
  parameters: {
    controls: { disable: true },
    docs: {
      description: {
        component:
          "Class-based primitives from **`src/app/design-system/primitives.css`**, loaded globally " +
          "through `styles.css`. They are the shared control language: lean on them instead of " +
          "re-declaring a button, field or menu per component (it also keeps each component under the " +
          "16 kB style budget). Where a `mur-*` component exists (toggle, select, input, pill, banner, " +
          "card…) prefer the component — it adds the behaviour and accessibility on top of the class.",
      },
    },
  },
};
export default meta;
type Story = StoryObj;

/** `.btn` + `.btn-primary` (the flat, opaque accent tint), `.btn-ghost`, `.btn-danger`. */
export const Buttons: Story = {
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-4)">
        <div style="display: flex; flex-wrap: wrap; gap: var(--space-2)">
          <button type="button" class="btn btn-primary">Start recording</button>
          <button type="button" class="btn">Export</button>
          <button type="button" class="btn btn-ghost">Cancel</button>
          <button type="button" class="btn btn-danger">Delete meeting</button>
        </div>
        <div style="display: flex; flex-wrap: wrap; gap: var(--space-2)">
          <button type="button" class="btn btn-primary" disabled>Start recording</button>
          <button type="button" class="btn" disabled>Export</button>
          <button type="button" class="btn btn-ghost" disabled>Cancel</button>
          <button type="button" class="btn btn-danger" disabled>Delete meeting</button>
        </div>
        <div style="display: flex; flex-wrap: wrap; gap: var(--space-2)">
          <button type="button" class="btn btn-primary"><mur-icon icon="record" /> Record</button>
          <button type="button" class="btn btn-ghost" aria-label="Settings"><mur-icon icon="settings" /></button>
        </div>
      </div>`,
  }),
};

/** Native fields are styled globally: text-like inputs, `select`, `textarea`, `checkbox`, plus `.field-help`. */
export const Fields: Story = {
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-4); max-width: 420px">
        <label style="display: grid; gap: var(--space-2)">
          <span class="section-label">Meeting title</span>
          <input type="text" placeholder="Weekly sync" />
          <span class="field-help">Shown in the vault file name and the note’s front-matter.</span>
        </label>
        <label style="display: grid; gap: var(--space-2)">
          <span class="section-label">Vault folder</span>
          <select>
            <option>Meetings</option>
            <option>Meetings/Product</option>
          </select>
        </label>
        <label style="display: grid; gap: var(--space-2)">
          <span class="section-label">Agenda</span>
          <textarea rows="3" placeholder="What should this meeting decide?"></textarea>
        </label>
        <label style="display: flex; align-items: center; gap: var(--space-2)">
          <input type="checkbox" checked /> Open the note when recording stops
        </label>
        <input type="text" value="Disabled field" disabled aria-label="Disabled field" />
      </div>`,
  }),
};

/** `.switch` — the raw ON/OFF switch that `<mur-toggle>` wraps. */
export const Switch: Story = {
  render: () => ({
    template: `
      <div style="display: flex; gap: var(--space-4); align-items: center">
        <input type="checkbox" class="switch" checked aria-label="On" />
        <input type="checkbox" class="switch" aria-label="Off" />
        <input type="checkbox" class="switch" checked disabled aria-label="Disabled on" />
      </div>`,
  }),
};

/** `.seg` / `.seg-btn` (+ `.is-active`), the icon variant `.seg--icon`, and the full-width `.tabbar`. */
export const SegmentedAndTabBar: Story = {
  name: "Segmented & tab bar",
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-4); max-width: 520px">
        <div class="seg" role="group" aria-label="Range">
          <button type="button" class="seg-btn">7d</button>
          <button type="button" class="seg-btn is-active">30d</button>
          <button type="button" class="seg-btn">90d</button>
        </div>
        <div class="seg seg--icon" role="group" aria-label="View">
          <button type="button" class="seg-btn is-active" aria-label="List"><mur-icon icon="browse" /></button>
          <button type="button" class="seg-btn" aria-label="Board"><mur-icon icon="dashboards" /></button>
          <button type="button" class="seg-btn" aria-label="Graph"><mur-icon icon="graph" /></button>
        </div>
        <div class="seg tabbar" role="tablist" aria-label="Meeting">
          <button type="button" class="seg-btn is-active" role="tab" aria-selected="true">Note</button>
          <button type="button" class="seg-btn" role="tab" aria-selected="false">Transcript</button>
          <button type="button" class="seg-btn" role="tab" aria-selected="false">Timeline</button>
        </div>
      </div>`,
  }),
};

/** `.menu` + `.menu-group` / `.menu-group-label` / `.menu-item` / `.menu-item-danger` — OPAQUE (`--surface-overlay`, T3). */
export const Menu: Story = {
  render: () => ({
    template: `
      <div class="menu" role="menu" aria-label="More" style="position: static; width: 240px">
        <div class="menu-group">
          <span class="menu-group-label">Export</span>
          <button type="button" class="menu-item" role="menuitem">Markdown</button>
          <button type="button" class="menu-item" role="menuitem">PDF</button>
          <button type="button" class="menu-item" role="menuitem" disabled>Audio (locked)</button>
        </div>
        <div class="menu-group">
          <button type="button" class="menu-item menu-item-danger" role="menuitem">Move to trash</button>
        </div>
      </div>`,
  }),
};

/** `.card` (frosted, in-flow), `.panel-card` (calm L1: hairline, no blur), `.state-card` + `.empty-state` wells. */
export const Surfaces: Story = {
  render: () => ({
    template: `
      <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: var(--space-4)">
        <div class="card">
          <span class="section-label">.card</span>
          <p style="margin: 0; color: var(--text-secondary)">Frosted glass for in-flow panels.</p>
        </div>
        <div class="panel-card">
          <span class="section-label">.panel-card</span>
          <p style="margin: 0; color: var(--text-secondary)">Calm raised surface, hairline border, no blur.</p>
        </div>
        <div class="card state-card">
          <div class="empty-state">
            <div class="empty-mark" aria-hidden="true"></div>
            <p class="empty-title">Nothing here</p>
            <p class="empty">.state-card › .empty-state</p>
          </div>
        </div>
      </div>`,
  }),
};

/** `.pill` (+ `.is-*`), `.banner` (+ `.is-*`), `.count`, `.section-label` and the text helpers. */
export const StatusAndText: Story = {
  name: "Status & text",
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-4)">
        <div style="display: flex; flex-wrap: wrap; gap: var(--space-2); align-items: center">
          <span class="pill"><span class="pill-dot"></span>Neutral</span>
          <span class="pill is-accent"><span class="pill-dot"></span>Accent</span>
          <span class="pill is-success"><span class="pill-dot"></span>Success</span>
          <span class="pill is-warning"><span class="pill-dot"></span>Warning</span>
          <span class="pill is-danger"><span class="pill-dot"></span>Danger</span>
          <span class="count">12</span>
        </div>
        <div class="banner is-warning" role="status">Microphone permission is missing.</div>
        <div>
          <span class="section-label">Section label</span>
          <p style="margin: 0">Primary text</p>
          <p class="text-secondary" style="margin: 0">.text-secondary</p>
          <p class="text-muted" style="margin: 0">.text-muted</p>
          <p class="text-success" style="margin: 0">.text-success</p>
          <p class="text-danger" style="margin: 0">.text-danger</p>
        </div>
      </div>`,
  }),
};

/** `.mur-range` — the ONE track + thumb shared by `<mur-slider>` and `<mur-power-slider>`. */
export const Range: Story = {
  render: () => ({
    template: `
      <div style="display: grid; gap: var(--space-3); max-width: 320px">
        <input type="range" class="mur-range" min="0" max="100" value="35" aria-label="Range" />
        <input type="range" class="mur-range" min="0" max="100" value="70" disabled aria-label="Disabled range" />
      </div>`,
  }),
};
