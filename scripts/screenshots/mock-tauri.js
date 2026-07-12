/*
 * Murmur screenshot harness — mocked Tauri IPC + a privacy-safe demo world.
 *
 * Injected as a Playwright `addInitScript` BEFORE the Angular app boots, this
 * installs a faithful `window.__TAURI_INTERNALS__` (invoke + transformCallback +
 * the event plumbing `@tauri-apps/api` expects) that answers every Tauri command
 * the FE calls with a consistent, fictional dataset — a made-up startup "Sonora"
 * with invented people/projects. NOTHING here touches a real vault, DB, mic, or
 * network: it is the real shipping Angular UI rendered over demo data, so the
 * README screenshots are honest UI, privacy-safe by construction.
 *
 * Emit backend events from the driver via `window.__demoEmit(event, payload)`.
 * Override the config per-shot via `window.__demoConfig` (set before load).
 *
 * See ./README.md for the capture runbook.
 */
(() => {
  "use strict";

  // ── Event plumbing (Tauri v2: transformCallback stores the fn; a listen goes
  //    through invoke('plugin:event|listen', {event, handler:<callbackId>})). ──
  const callbacks = new Map(); // id -> fn
  let nextCallbackId = 1;
  const listeners = new Map(); // eventName -> Set<callbackId>
  let nextEventId = 1;

  function transformCallback(cb) {
    const id = nextCallbackId++;
    callbacks.set(id, cb);
    return id;
  }

  // Driver-facing: fire a backend event to every subscriber.
  window.__demoEmit = (event, payload) => {
    const ids = listeners.get(event);
    if (!ids) return;
    for (const id of ids) {
      const fn = callbacks.get(id);
      if (fn) fn({ event, id, payload });
    }
  };

  // ── The demo world ────────────────────────────────────────────────────────
  const VAULT = "/Users/demo/Obsidian/Sonora";
  const DEMO_MEETING_ID = "m-atlas-roadmap";

  // Deterministic-ish recent timestamps anchored to a fixed day (browser Date is
  // fine here — this runs in the page, not the workflow sandbox).
  const ANCHOR = new Date("2026-07-02T15:00:00");
  const daysAgo = (d, h = 10, m = 0) => {
    const t = new Date(ANCHOR);
    t.setDate(t.getDate() - d);
    t.setHours(h, m, 0, 0);
    return t.toISOString();
  };

  const FOLDERS = [
    {
      id: "f-product",
      name: "Product",
      parentId: null,
      noteCount: 14,
      locked: false,
      unlocked: false,
      children: [
        { id: "f-atlas", name: "Project Atlas", parentId: "f-product", noteCount: 6, locked: false, unlocked: false, children: [] },
        { id: "f-mobile", name: "Mobile Redesign", parentId: "f-product", noteCount: 4, locked: false, unlocked: false, children: [] },
      ],
    },
    {
      id: "f-eng",
      name: "Engineering",
      parentId: null,
      noteCount: 11,
      locked: false,
      unlocked: false,
      children: [],
    },
    {
      id: "f-clients",
      name: "Clients",
      parentId: null,
      noteCount: 9,
      locked: false,
      unlocked: false,
      children: [],
    },
    {
      id: "f-personal",
      name: "Personal",
      parentId: null,
      noteCount: 3,
      locked: true,
      unlocked: false,
      children: [],
    },
  ];

  const M = (id, title, day, durationS, folderId, status = "EXPORTED") => ({
    id,
    startedAt: daysAgo(day, 9 + (day % 6), (day * 7) % 60),
    endedAt: daysAgo(day, 9 + (day % 6), ((day * 7) % 60) + Math.round(durationS / 60)),
    title,
    durationS,
    audioPath: `${VAULT}/.audio/${id}.wav`,
    status,
    folderId,
  });

  const MEETINGS = [
    M(DEMO_MEETING_ID, "Q2 Roadmap Planning", 0, 2820, "f-atlas"),
    M("m-eng-sync", "Eng Sync — Sales Engine", 1, 1560, "f-eng"),
    M("m-design-review", "Design Review — Mobile Redesign", 1, 2280, "f-mobile"),
    M("m-acme", "Customer Call — Acme Corp", 2, 3120, "f-clients"),
    M("m-atlas-kickoff", "Project Atlas — Kickoff", 3, 2460, "f-atlas"),
    M("m-1on1", "1:1 — Sarah & Marcus", 4, 1260, "f-product"),
    M("m-retro", "Sprint 24 Retro", 5, 1980, "f-eng"),
    M("m-northwind", "Discovery — Northwind", 6, 2760, "f-clients"),
    M("m-all-hands", "Weekly All-Hands", 7, 2040, "f-product"),
    M("m-data-review", "Data Review — Activation Funnel", 9, 1740, "f-eng"),
    M("m-pricing", "Pricing Workshop", 11, 2400, "f-product"),
    M("m-support", "Support Escalation — Acme", 13, 900, "f-clients"),
  ];

  const NOTE_MD = `# Q2 Roadmap Planning

## Summary
The team aligned Q2 around three bets: take **Project Atlas** to GA, cut the
mobile redesign to the top-three flows, and stand up the **Sales Engine** pilot
with two design partners. Marcus flagged the Windows loopback capture as the one
remaining release blocker for Atlas; the shared sync-layer dependency that had
deferred it twice is now removed, so it's unblocked for the GA cut.

## Decisions
- Project Atlas ships to GA on May 30 — Marcus owns the release checklist.
- Mobile redesign scope trimmed to onboarding, search, and note detail.
- Sales Engine pilot goes to Acme and Northwind first.
- Pricing changes are deferred to Q3 pending the activation-funnel readout.

## Action items
- [ ] Sarah — circulate the trimmed roadmap deck (due Fri)
- [ ] Marcus — finalize the Atlas GA release checklist
- [ ] Anya — deliver the mobile onboarding mocks
- [ ] Devon — schedule the Acme pilot kickoff

## Notable quotes
> "If Atlas slips, everything behind it slips — it's the keystone." — Marcus
> "Let's not gold-plate the redesign. Ship the three flows people actually use." — Sarah`;

  const noteFor = (id, title) => ({
    meetingId: id,
    providerId: "claude_code",
    markdown: id === DEMO_MEETING_ID ? NOTE_MD : `# ${title}\n\n## Summary\nAuto-generated on-device from the transcript.`,
    exportedPath: `${VAULT}/Meetings/${title.replace(/[^a-z0-9]+/gi, "-")}.md`,
  });

  // Merged Me / Others transcript for the flagship meeting.
  const SEGMENTS = [
    { idx: 0, startS: 0, endS: 6, speaker: "me", text: "Okay, let's lock the Q2 roadmap. Three things: Atlas GA, the mobile redesign, and the Sales Engine pilot." },
    { idx: 1, startS: 6, endS: 14, speaker: "others", text: "On Atlas — the Windows loopback spike is the last blocker. We were waiting on the shared sync layer." },
    { idx: 2, startS: 14, endS: 20, speaker: "me", text: "Is that dependency still in the way? I thought Priya's team landed it last sprint." },
    { idx: 3, startS: 20, endS: 29, speaker: "others", text: "They did — it merged Thursday. So the loopback work is unblocked. I can commit to a May 30 GA if nothing else moves." },
    { idx: 4, startS: 29, endS: 36, speaker: "me", text: "Good. Let's write that down as a decision. Marcus owns the GA checklist." },
    { idx: 5, startS: 36, endS: 45, speaker: "others", text: "On the redesign, I want to cut scope. Onboarding, search, and the note detail view. Everything else waits." },
    { idx: 6, startS: 45, endS: 52, speaker: "me", text: "Agreed. Let's not gold-plate it. Anya, can you get onboarding mocks by Friday?" },
    { idx: 7, startS: 52, endS: 60, speaker: "others", text: "Friday works. I'll share the trimmed flow first so we're aligned before I go high-fidelity." },
  ];

  const TIMELINE = {
    speakers: [
      { speaker: "Sarah", startS: 0, endS: 6 },
      { speaker: "Marcus", startS: 6, endS: 14 },
      { speaker: "Sarah", startS: 14, endS: 20 },
      { speaker: "Marcus", startS: 20, endS: 29 },
      { speaker: "Sarah", startS: 29, endS: 36 },
      { speaker: "Marcus", startS: 36, endS: 45 },
      { speaker: "Sarah", startS: 45, endS: 52 },
      { speaker: "Anya", startS: 52, endS: 60 },
    ],
    topics: [
      { label: "Project Atlas — GA", startS: 0, endS: 35 },
      { label: "Mobile redesign scope", startS: 35, endS: 52 },
      { label: "Onboarding mocks", startS: 52, endS: 60 },
    ],
  };

  // The persisted @brain thread rehydrated on a reopened meeting (record screen).
  const THREAD_ROWS = [
    {
      threadId: "t-atlas-1",
      anchorText: "loopback blocker?",
      command: "what did we decide about the Windows loopback blocker last quarter?",
      answer:
        "Last quarter the Windows loopback spike was deferred **twice** — the blocker was the shared sync layer. **Project Atlas** removed that dependency, so it's unblocked for the Q2 GA cut.",
      citations: ["[[Project Atlas — Kickoff]]", "[[Sprint 24 Retro]]"],
      status: "ok",
      createdAt: daysAgo(0, 10, 5),
    },
  ];

  // brain2 documents + typed notes (kinds split by the /brain source cards).
  const DOCS_BY_FOLDER = {
    "f-atlas": [
      { id: "d1", name: "Atlas — PRD v3.md", kind: "document", createdAt: ANCHOR.getTime() - 6 * 864e5 },
      { id: "d2", name: "Atlas — GA release checklist.md", kind: "document", createdAt: ANCHOR.getTime() - 3 * 864e5 },
      { id: "n1", name: "Keystone dependency — sync layer", kind: "note", createdAt: ANCHOR.getTime() - 2 * 864e5 },
      { id: "n2", name: "Open question: Windows notarization", kind: "note", createdAt: ANCHOR.getTime() - 1 * 864e5 },
    ],
    "f-eng": [
      { id: "d3", name: "Sales Engine — architecture.md", kind: "document", createdAt: ANCHOR.getTime() - 5 * 864e5 },
      { id: "d4", name: "Incident postmortem — 2026-06.md", kind: "document", createdAt: ANCHOR.getTime() - 4 * 864e5 },
      { id: "n3", name: "Retro action: flaky diarization test", kind: "note", createdAt: ANCHOR.getTime() - 3 * 864e5 },
      { id: "n4", name: "Idea: batch the accurate ASR pass", kind: "note", createdAt: ANCHOR.getTime() - 2 * 864e5 },
      { id: "n5", name: "Priya landed the sync layer (Thu)", kind: "note", createdAt: ANCHOR.getTime() - 1 * 864e5 },
    ],
    "f-clients": [
      { id: "d5", name: "Acme — contract redlines.md", kind: "document", createdAt: ANCHOR.getTime() - 7 * 864e5 },
      { id: "d6", name: "Northwind — discovery brief.md", kind: "document", createdAt: ANCHOR.getTime() - 4 * 864e5 },
      { id: "n6", name: "Acme wants SSO by pilot end", kind: "note", createdAt: ANCHOR.getTime() - 3 * 864e5 },
      { id: "n7", name: "Northwind: data residency = EU", kind: "note", createdAt: ANCHOR.getTime() - 2 * 864e5 },
    ],
    "f-mobile": [
      { id: "n8", name: "Trim scope to 3 flows", kind: "note", createdAt: ANCHOR.getTime() - 2 * 864e5 },
      { id: "n9", name: "Onboarding mocks due Friday", kind: "note", createdAt: ANCHOR.getTime() - 1 * 864e5 },
    ],
    "f-product": [
      { id: "n10", name: "Pricing deferred to Q3", kind: "note", createdAt: ANCHOR.getTime() - 5 * 864e5 },
      { id: "n11", name: "Activation funnel readout owner", kind: "note", createdAt: ANCHOR.getTime() - 3 * 864e5 },
      { id: "n12", name: "All-hands: hiring freeze lifts Q3", kind: "note", createdAt: ANCHOR.getTime() - 1 * 864e5 },
    ],
  };

  // Knowledge graph — people + projects, co-occurrence edges.
  const GRAPH = {
    nodes: [
      { id: "p-sarah", name: "Sarah Chen", kind: "person", mentionCount: 31 },
      { id: "p-marcus", name: "Marcus Reid", kind: "person", mentionCount: 28 },
      { id: "p-anya", name: "Anya Petrov", kind: "person", mentionCount: 19 },
      { id: "p-devon", name: "Devon Blake", kind: "person", mentionCount: 15 },
      { id: "p-priya", name: "Priya Nair", kind: "person", mentionCount: 12 },
      { id: "pr-atlas", name: "Project Atlas", kind: "project", mentionCount: 34 },
      { id: "pr-sales", name: "Sales Engine", kind: "project", mentionCount: 22 },
      { id: "pr-mobile", name: "Mobile Redesign", kind: "project", mentionCount: 18 },
      { id: "pr-acme", name: "Acme Corp", kind: "project", mentionCount: 14 },
      { id: "pr-northwind", name: "Northwind", kind: "project", mentionCount: 9 },
    ],
    edges: [
      { source: "p-marcus", target: "pr-atlas", weight: 11 },
      { source: "p-sarah", target: "pr-atlas", weight: 9 },
      { source: "p-priya", target: "pr-atlas", weight: 6 },
      { source: "p-sarah", target: "p-marcus", weight: 8 },
      { source: "p-anya", target: "pr-mobile", weight: 7 },
      { source: "p-sarah", target: "pr-mobile", weight: 5 },
      { source: "p-devon", target: "pr-acme", weight: 6 },
      { source: "p-devon", target: "pr-sales", weight: 5 },
      { source: "p-marcus", target: "pr-sales", weight: 6 },
      { source: "p-devon", target: "pr-northwind", weight: 4 },
      { source: "p-sarah", target: "p-anya", weight: 4 },
      { source: "pr-atlas", target: "pr-sales", weight: 3 },
    ],
    hasHidden: true,
  };

  const ENTITY_DETAIL = {
    "p-marcus": {
      entity: { id: "p-marcus", name: "Marcus Reid", kind: "person", createdAt: daysAgo(30) },
      meetings: [
        { meetingId: DEMO_MEETING_ID, title: "Q2 Roadmap Planning", startedAt: daysAgo(0) },
        { meetingId: "m-atlas-kickoff", title: "Project Atlas — Kickoff", startedAt: daysAgo(3) },
        { meetingId: "m-eng-sync", title: "Eng Sync — Sales Engine", startedAt: daysAgo(1) },
        { meetingId: "m-retro", title: "Sprint 24 Retro", startedAt: daysAgo(5) },
      ],
      neighbors: [
        { id: "pr-atlas", name: "Project Atlas", kind: "project", sharedMeetings: 11 },
        { id: "p-sarah", name: "Sarah Chen", kind: "person", sharedMeetings: 8 },
        { id: "pr-sales", name: "Sales Engine", kind: "project", sharedMeetings: 6 },
      ],
    },
  };

  // Analytics — 30-day activity.
  const perDay = [];
  const counts = [1, 0, 2, 1, 0, 0, 3, 2, 1, 0, 1, 2, 0, 1, 3, 1, 0, 2, 1, 1, 0, 2, 3, 1, 0, 1, 2, 1, 2, 3];
  for (let i = 29; i >= 0; i--) {
    const d = new Date(ANCHOR);
    d.setDate(d.getDate() - i);
    const c = counts[29 - i];
    perDay.push({ date: d.toISOString().slice(0, 10), count: c, durationS: c * 1900 });
  }
  const ANALYTICS = {
    totalMeetings: 47,
    totalDurationS: 95880,
    avgDurationS: 2040,
    longestDurationS: 3720,
    meetings7d: 9,
    duration7dS: 17640,
    notesCount: 44,
    firstMeetingAt: daysAgo(58),
    byStatus: [
      { status: "EXPORTED", count: 41 },
      { status: "SUMMARIZED", count: 3 },
      { status: "TRANSCRIBED", count: 2 },
      { status: "ERROR", count: 1 },
    ],
    perDay,
  };

  const BRAIN_MODELS = [
    { id: "qwen25-3b", name: "Qwen2.5 3B Instruct", sizeLabel: "2.3 GB", bytes: 2469606195, minRamGb: 8, languages: ["en", "pl"], downloaded: true, fitsRam: true, selected: false },
    { id: "bielik-11b", name: "Bielik 11B v2.3", sizeLabel: "6.7 GB", bytes: 7193948160, minRamGb: 16, languages: ["pl", "en"], downloaded: true, fitsRam: true, selected: true },
    { id: "qwen3-14b", name: "Qwen3 14B", sizeLabel: "8.9 GB", bytes: 9556302848, minRamGb: 24, languages: ["en", "pl", "de", "fr"], downloaded: false, fitsRam: true, selected: false },
  ];

  const PROVIDERS = [
    { id: "claude_code", available: true },
    { id: "anthropic", available: true },
    { id: "ollama", available: true },
    { id: "local", available: true },
    { id: "gateway", available: false, reason: "No gateway base URL configured" },
  ];

  const EGRESS = {
    totalCalls: 128,
    totalTokens: 486200,
    byModel: [
      { model: "claude-opus-4-8", calls: 84, tokens: 351400 },
      { model: "claude-haiku-4-5", calls: 44, tokens: 134800 },
    ],
    byDay: perDay.slice(-14).map((d) => ({ day: d.date, tokens: d.count * 12000 })),
    totalRedactions: { email: 37, card: 2, phone: 11, name: 214 },
    recent: [
      { ts: Math.floor(ANCHOR.getTime() / 1000) - 3600, providerId: "anthropic", destination: "api.anthropic.com", modelServed: "claude-opus-4-8", totalTokens: 4180, redactions: { email: 1, card: 0, phone: 0, name: 6 } },
      { ts: Math.floor(ANCHOR.getTime() / 1000) - 7200, providerId: "claude_code", destination: "api.anthropic.com", modelServed: "claude-opus-4-8", totalTokens: 3920, redactions: { email: 0, card: 0, phone: 1, name: 4 } },
    ],
  };

  const DEFAULT_CONFIG = {
    providerId: "claude_code",
    vaultPath: VAULT,
    vaultSubfolder: "Meetings",
    whisperModelPath: null,
    language: null,
    anthropicModel: "claude-opus-4-8",
    providerModel: "",
    providerEffort: "",
    ollamaBaseUrl: "http://localhost:11434",
    ollamaModel: "llama3.1:8b",
    claudeBinary: "claude",
    inputDevice: null,
    captureSystemAudio: true,
    vadEnabled: true,
    keepHiresMasters: false,
    diarizeOthers: true,
    aecEnabled: false,
    postAecEnabled: false,
    modelSize: "large-v3",
    voiceTrigger: true,
    onboarded: true,
    // The demo world has already resolved the first-run sharing choice, so the
    // /welcome gateway never intercepts a screenshot route.
    sharingChoiceMade: true,
    noteStyle: "structured",
    autoOrganize: true,
    noteLanguage: "en",
    mcpRequireToken: true,
    lockRequireBiometric: true,
    relockOnScreenshare: true,
    cloudEgressConsented: true,
    brainBackend: "cloud",
    realtimeReactions: true,
    brainModelId: "bielik-11b",
    brainModelPath: null,
    semanticSearchEnabled: true,
    webSearchEnabled: false,
    webSearchConsented: false,
    claudeCodeInheritEnv: false,
    gatewayBaseUrl: "",
    gatewayModel: "",
    proactiveHintsEnabled: true,
    roleNotesConnection: "",
    roleNotesModel: "",
    roleNotesEffort: "",
    roleAskConnection: "",
    roleAskModel: "",
    roleAskEffort: "",
    roleLiveConnection: "",
    roleLiveModel: "",
    roleLiveEffort: "",
  };

  const ASK_ANSWER = `Across your last quarter, **Project Atlas** is the recurring throughline. The team made sync-layer latency the gating priority (target **p95 < 180 ms**), committed to a **design partner** track by **Aug 15**, and deferred the mobile redesign to Q4 once Atlas removed the shared-sync dependency. Marcus owns the Windows loopback spike; Sarah owns the migration plan.`;

  // ── The command router ──────────────────────────────────────────────────────
  function currentConfig() {
    return Object.assign({}, DEFAULT_CONFIG, window.__demoConfig || {});
  }

  const meetingById = (id) => MEETINGS.find((m) => m.id === id) || MEETINGS[0];

  function detailFor(id) {
    const m = meetingById(id);
    return {
      meeting: m,
      note: noteFor(m.id, m.title),
      segments: m.id === DEMO_MEETING_ID ? SEGMENTS : SEGMENTS.slice(0, 4),
      assistantInteractions:
        m.id === DEMO_MEETING_ID
          ? [
              {
                command: "what's the biggest risk to the GA date?",
                answer: "The Windows loopback spike — now unblocked since the shared sync layer merged. Marcus owns it.",
                citations: ["[[Project Atlas — Kickoff]]"],
                status: "ok",
                sourceLabel: "recall",
                createdAt: daysAgo(0, 10, 12),
              },
            ]
          : [],
      aiProvider: "claude_code",
      aiModel: "claude-opus-4-8",
      modelServed: "claude-opus-4-8",
    };
  }

  const ACTION_ITEMS = [
    { idx: 0, done: false, text: "Circulate the trimmed roadmap deck", owner: "Sarah", dueDate: daysAgo(-2, 17, 0) },
    { idx: 1, done: false, text: "Finalize the Atlas GA release checklist", owner: "Marcus", dueDate: null },
    { idx: 2, done: true, text: "Deliver the mobile onboarding mocks", owner: "Anya", dueDate: daysAgo(-1, 17, 0) },
    { idx: 3, done: false, text: "Schedule the Acme pilot kickoff", owner: "Devon", dueDate: null },
  ];

  const RELATED = [
    { meeting: meetingById("m-atlas-kickoff"), snippet: "…the shared sync layer is the keystone dependency for Atlas GA…", matchedIn: "transcript" },
    { meeting: meetingById("m-eng-sync"), snippet: "…loopback capture on Windows is the last open blocker…", matchedIn: "note" },
    { meeting: meetingById("m-retro"), snippet: "…deferred the loopback spike again this sprint…", matchedIn: "transcript" },
  ];

  const BUILTIN_RECIPES = [
    { id: "email", label: "Follow-up email", prompt: "Write a follow-up email from this meeting." },
    { id: "decisions", label: "Decision log", prompt: "Extract a decision log." },
    { id: "actions", label: "Action items", prompt: "List the action items with owners." },
  ];

  // ── Notes (standalone authored documents — separate from meeting notes) ────
  const NOTE_FOLDERS = [
    { id: "nf-product", name: "Product Notes", path: "Notes/Product", parentId: null, locked: false, kind: "note" },
    { id: "nf-eng", name: "Engineering Notes", path: "Notes/Engineering", parentId: null, locked: false, kind: "note" },
  ];

  const DEMO_NOTE_ID = "n-atlas-prd";

  const NOTE_DOC_MD = `# Atlas — PRD v3

## Overview
**Project Atlas** is the sync-layer rewrite that unblocks the Windows loopback
capture path. This revision folds in Priya's merged sync-layer work and trims
scope to the GA-critical path only.

## Goals
- Ship a stable Windows loopback capture path by the May 30 GA cut.
- Remove the shared sync-layer dependency that deferred the spike twice.
- Keep the merge additive — no destructive schema changes to the capture pipeline.

## Non-goals
- Linux capture support (tracked separately, post-GA).
- Any change to the macOS ScreenCaptureKit path (already stable).

## Open questions
- Windows notarization equivalent — do we need a signed driver?
- Does the Sales Engine pilot depend on this landing first?

## Rollout plan
1. Land the sync-layer merge behind a flag.
2. Dogfood internally for one week.
3. Flip the flag for the GA cut on May 30.`;

  const NOTES = [
    {
      id: DEMO_NOTE_ID,
      title: "Atlas — PRD v3",
      folderId: "nf-product",
      snippet: "The sync-layer rewrite that unblocks the Windows loopback capture path…",
      tags: ["atlas", "prd"],
      updatedAt: ANCHOR.getTime() - 1 * 864e5,
      createdAt: ANCHOR.getTime() - 6 * 864e5,
      locked: false,
      shared: false,
    },
    {
      id: "n-sales-arch",
      title: "Sales Engine — architecture notes",
      folderId: "nf-eng",
      snippet: "Two design partners, Acme and Northwind, pilot the ingestion path first…",
      tags: ["sales-engine"],
      updatedAt: ANCHOR.getTime() - 3 * 864e5,
      createdAt: ANCHOR.getTime() - 9 * 864e5,
      locked: false,
      shared: true,
    },
  ];

  function noteDocFor(id) {
    const summary = NOTES.find((n) => n.id === id);
    if (id === DEMO_NOTE_ID) {
      return {
        id: DEMO_NOTE_ID,
        title: "Atlas — PRD v3",
        folderId: "nf-product",
        markdown: NOTE_DOC_MD,
        tags: ["atlas", "prd"],
        properties: { status: "in-review" },
        updatedAt: summary ? summary.updatedAt : ANCHOR.getTime(),
        createdAt: summary ? summary.createdAt : ANCHOR.getTime(),
        exportedPath: `${VAULT}/Notes/Product/Atlas-PRD-v3.md`,
        locked: false,
        shared: false,
      };
    }
    const title = summary ? summary.title : "Untitled";
    return {
      id,
      title,
      folderId: (summary && summary.folderId) || "nf-product",
      markdown: `# ${title}\n\n## Notes\nAuthored note body.`,
      tags: (summary && summary.tags) || [],
      properties: {},
      updatedAt: summary ? summary.updatedAt : ANCHOR.getTime(),
      createdAt: summary ? summary.createdAt : ANCHOR.getTime(),
      exportedPath: null,
      locked: false,
      shared: (summary && summary.shared) || false,
    };
  }

  // ── Shared Brain (org) ──────────────────────────────────────────────────────
  const ORGS = [
    {
      orgId: "org-sonora",
      name: "Sonora",
      role: "owner",
      memberCount: 5,
      consented: true,
      lastSeq: 12,
      itemCount: 4,
      receivedCount: 9,
      pendingShares: 0,
    },
  ];

  const ORG_ITEMS_BY_ORG = {
    "org-sonora": [
      {
        itemId: "oi-1",
        title: "Acme — contract redlines",
        authorHint: "devon@sonora",
        createdAt: daysAgo(2),
        seq: 9,
        kind: "document",
        ownedSource: null,
      },
      {
        itemId: "oi-2",
        title: "Northwind — discovery brief",
        authorHint: "sarah@sonora",
        createdAt: daysAgo(4),
        seq: 8,
        kind: "document",
        ownedSource: null,
      },
      {
        itemId: "oi-3",
        title: "Sales Engine — architecture notes",
        authorHint: "you",
        createdAt: daysAgo(3),
        seq: 7,
        kind: "document",
        ownedSource: { kind: "document", id: "n-sales-arch" },
      },
      {
        itemId: "oi-4",
        title: "Pricing workshop — takeaways",
        authorHint: "marcus@sonora",
        createdAt: daysAgo(11),
        seq: 6,
        kind: "document",
        ownedSource: null,
      },
    ],
  };

  function handle(cmd, args) {
    switch (cmd) {
      // ── config / product ──
      case "get_config": return currentConfig();
      case "save_config": return null;
      case "app_info": return { name: "Murmur", version: "0.6.3", description: "Local-first meeting notes with an on-device brain.", repository: "https://github.com/murmur-io/murmur" };
      case "check_for_update": return { currentVersion: "0.6.3", latestVersion: "0.6.3", updateAvailable: false, releaseUrl: "https://github.com/murmur-io/murmur/releases/latest", releaseName: null, releaseNotes: null };
      case "provider_statuses": return PROVIDERS;
      case "consent_to_cloud_egress": case "revoke_cloud_egress": case "consent_to_web_search": return null;

      // ── recorder ──
      case "start_recording": return { meetingId: DEMO_MEETING_ID };
      case "stop_recording": return { meetingId: DEMO_MEETING_ID, markdown: NOTE_MD, exportedPath: `${VAULT}/Meetings/Q2-Roadmap-Planning.md` };
      case "recording_level": return 0.28 + Math.random() * 0.55;
      case "set_mic_muted": return null;
      case "is_mic_muted": return false;
      case "list_input_devices": return [{ name: "MacBook Pro Microphone", isDefault: true }, { name: "AirPods Pro", isDefault: false }, { name: "Shure MV7", isDefault: false }];
      case "output_is_builtin_speakers": return false;
      case "detect_meeting_app": return null;
      case "get_last_note": return noteFor(DEMO_MEETING_ID, "Q2 Roadmap Planning");
      case "toggle_bar": return null;
      case "resummarize": return { meetingId: args.meetingId, markdown: NOTE_MD, exportedPath: `${VAULT}/Meetings/Q2-Roadmap-Planning.md` };

      // ── meetings / library ──
      case "list_meetings": return MEETINGS;
      case "list_meetings_by_tag": return MEETINGS.slice(0, 4);
      case "search_meetings": return RELATED;
      case "related_meetings": return RELATED;
      case "list_all_tags": return ["atlas", "roadmap", "clients", "eng", "design", "pricing"];
      case "get_meeting_tags": return ["atlas", "roadmap"];
      case "set_meeting_tags": return null;
      case "get_meeting_detail": return detailFor(args.meetingId);
      case "get_timeline": return TIMELINE;
      case "rename_speaker": return TIMELINE;
      case "get_action_items": return ACTION_ITEMS;
      case "rename_meeting": case "delete_meeting": case "move_note": case "set_meeting_tags2": return null;
      case "get_analytics": return ANALYTICS;

      // ── notes / manual notes / assistant threads ──
      case "get_manual_notes":
        return args.meetingId === DEMO_MEETING_ID
          ? "Kickoff went long — good energy.\nloopback blocker?\nShip the 3 flows people actually use."
          : "";
      case "save_manual_notes": return null;
      case "list_assistant_threads": return args.meetingId === DEMO_MEETING_ID ? THREAD_ROWS : [];
      case "update_note": return noteFor(args.meetingId, "Q2 Roadmap Planning");
      case "patch_note_tasks": return noteFor(args.meetingId, "Q2 Roadmap Planning");

      // ── in-meeting agentic brain ──
      case "ask_assistant_text": return null;
      case "begin_voice_command": return null;
      case "end_voice_command": return null;
      case "ask_assistant_chat":
        return {
          intentKind: "recall",
          status: "ok",
          summary:
            "The mobile redesign was deferred twice — the blocker was the shared sync layer. **Project Atlas** removed that dependency, so it's unblocked for Q4.",
          command: (args.messages && args.messages.length ? args.messages[args.messages.length - 1].text : ""),
          citations: ["[[Q2 Roadmap Planning]]", "[[Eng Sync — Sales Engine]]"],
          proposedNote: null,
          threadId: args.threadId || "t-live-1",
        };

      // ── Ask across the vault ──
      case "ask_vault":
        return {
          answer: ASK_ANSWER,
          sources: [
            { meetingId: DEMO_MEETING_ID, title: "Q2 Roadmap Planning", startedAt: daysAgo(0) },
            { meetingId: "m-eng-sync", title: "Eng Sync — Sales Engine", startedAt: daysAgo(1) },
            { meetingId: "m-atlas-kickoff", title: "Project Atlas — Kickoff", startedAt: daysAgo(3) },
          ],
          citations: ["[[Q2 Roadmap Planning]]", "[[Project Atlas — Kickoff]]"],
        };
      case "list_builtin_recipes": return BUILTIN_RECIPES;
      case "list_saved_recipes": return [];
      case "run_recipe": return "Drafted from the transcript…";
      case "topic_threads": return [];

      // ── graph / entities ──
      case "get_graph": return GRAPH;
      case "get_entity_detail": return ENTITY_DETAIL[args.entityId] || ENTITY_DETAIL["p-marcus"];
      case "link_meeting_entities": return { people: ["Sarah Chen", "Marcus Reid"], projects: ["Project Atlas"] };

      // ── AI & Models settings (posture + the "What runs where" resolved map) ──
      case "brain_posture": return "hybrid";
      case "set_brain_posture": return null;
      case "resolved_ai_map": return [
        { job: "notes", title: "Notes & summaries", engine: "Claude Code", model: "claude-sonnet-4-6", onDevice: false, redacted: true, active: true, routable: true },
        { job: "ask", title: "Ask & chat", engine: "Claude Code", model: "claude-sonnet-4-6", onDevice: false, redacted: true, active: true, routable: true },
        { job: "live", title: "Live @brain", engine: "Claude Code", model: "claude-sonnet-4-6", onDevice: false, redacted: true, active: true, routable: true },
        { job: "reactions", title: "Realtime reactions", engine: "Qwen3 1.7B", model: "qwen3-1.7b", onDevice: true, redacted: false, active: true, routable: false },
        { job: "transcription", title: "Transcription", engine: "Whisper", model: "small", onDevice: true, redacted: false, active: true, routable: false },
        { job: "embeddings", title: "Search index", engine: "Multilingual E5", model: "multilingual-e5-small", onDevice: true, redacted: false, active: true, routable: false },
        { job: "redaction", title: "Name redaction", engine: "On-device NER", model: "", onDevice: true, redacted: false, active: true, routable: false },
      ];

      // ── brain page (brain2) ──
      case "brain_overview": return { meetingCount: 47, documentCount: 6, noteCount: 12, indexedChunkCount: 1284, semanticEnabled: true, embedModelPresent: true };
      case "list_documents": return DOCS_BY_FOLDER[args.folderId] || [];
      case "get_document": return "Demo document body (gated).";
      case "import_document": case "import_text": return "d-new";
      case "delete_document": return null;

      // ── folders / lock ──
      case "list_folders": return FOLDERS;
      case "create_folder": return { id: "f-new", name: args.name, path: `Meetings/${args.name}`, parentId: args.parentId || null, locked: false, createdAt: daysAgo(0) };
      case "rename_folder": return { id: args.folderId, name: args.newName, path: `Meetings/${args.newName}`, parentId: null, locked: false, createdAt: daysAgo(0) };
      case "delete_folder": case "lock_folder": case "relock_folder": case "relock_all": case "remove_lock": return null;
      case "unlock_folder": case "unlock_meeting": return { id: "f-personal", name: "Personal", parentId: null, noteCount: 3, locked: true, unlocked: true, children: [] };

      // ── models / ML presence ──
      case "model_present": return true;
      case "download_model": return "/models/ggml-large-v3.bin";
      case "list_brain_models": return BRAIN_MODELS;
      case "select_brain_model": case "download_brain_model": return null;
      case "embed_model_present": return true;
      case "download_embed_model": return "/models/e5";
      case "reindex_embeddings": return { status: "indexed", indexed: 47, total: 47 };

      // ── secrets presence (never the value) ──
      case "has_anthropic_key": return true;
      case "set_anthropic_key": return null;
      case "has_gateway_key": return false;
      case "set_gateway_key": case "clear_gateway_key": return null;
      case "has_web_search_key": return false;
      case "set_web_search_api_key": return null;

      // ── gateway / egress ──
      case "list_gateway_models": return [];
      case "list_models":
        if (args && args.connection === "ollama") return ["llama3.1:8b", "qwen2.5:7b", "mistral:7b"];
        if (args && args.connection === "gateway") return [];
        return ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"];
      case "gateway_health": return { reachable: false, modelCount: 0 };
      case "get_egress_ledger": return EGRESS;

      // ── calendar (on-device EventKit) ──
      case "next_calendar_event": return null;
      case "list_calendar_events": return [];
      case "calendar_context_for": return null;
      case "pre_meeting_brief": return { markdown: "## Prep\n- Last time: agreed May 30 GA.\n- Open: Windows loopback spike.", sources: [] };

      // ── notes (standalone authored documents) ──
      case "list_note_folders": return NOTE_FOLDERS;
      case "create_note_folder": return { id: "nf-new", name: args.name, path: `Notes/${args.name}`, parentId: args.parentId || null, locked: false, kind: "note" };
      case "rename_note_folder": case "delete_note_folder": case "move_note_folder": return null;
      case "list_notes": return NOTES;
      case "get_note": return noteDocFor(args.id);
      case "create_note": return "n-new";
      case "update_note_doc": return noteDocFor(args.id);
      case "save_note_text": return Date.now();
      case "move_note_doc": case "delete_note": return null;
      case "export_note_doc": return `${VAULT}/Notes/Product/Atlas-PRD-v3.md`;
      case "plan_organize_notes": return { moves: [] };
      case "apply_organize_plan": return null;
      case "note_assistant_action":
        return {
          action: args.req.action,
          shape: args.req.action === "find_related" || args.req.action === "ask" ? "info"
            : args.req.action === "draft_followup" || args.req.action === "spinoff_note" ? "artifact"
            : args.req.action === "enhance" ? "insert"
            : "replace",
          title: args.req.action === "draft_followup" ? "Follow-up: Atlas PRD review" : null,
          suggestion: "Project Atlas removed the shared sync-layer dependency, unblocking the Windows loopback capture path for the May 30 GA cut.",
          citations: [
            { kind: "meeting", id: "m-atlas-kickoff", title: "Project Atlas — Kickoff", snippet: "…the shared sync layer is the keystone dependency…" },
          ],
          modelLabel: "Claude",
          mode: "cloud",
          redacted: true,
        };

      // ── Shared Brain (org) ──
      case "org_refresh": return null;
      case "org_list_statuses": return ORGS;
      case "list_org_items": return ORG_ITEMS_BY_ORG[args.orgId] || [];
      case "org_resolve_source": return null;
      // Per-meeting (Detail header pill) / bulk (Library row badge) share
      // pairings — both array-shaped; unmocked callers must see `[]`, never
      // the generic `null` default (a bare array-returning command doesn't
      // match the `list_`/`get_` fallback prefixes below).
      case "meeting_org_shares": return [];
      case "org_live_shares_for_source": return [];

      // ── exports / misc ──
      case "export_audio": case "export_note": case "export_mic_master": case "export_sys_master": return null;
      case "export_canvas": return `${VAULT}/Canvas/Q2-Roadmap.canvas`;
      case "pin_moment": return { url: "obsidian://open?vault=Sonora", blockId: "^abc123", mmss: "0:36" };
      case "generate_digest": return { markdown: "# Weekly Digest", exportedPath: `${VAULT}/Digests/2026-W27.md` };
      case "add_reminder": return null;
      case "open_release_page": return null;

      default:
        // Unknown/plugin commands — resolve to a benign default so boot never
        // crashes (window plugin calls: getCurrentWindow().show(), etc.).
        if (cmd.startsWith("plugin:")) return null;
        if (/^(list_|get_)/.test(cmd)) return [];
        if (/^(has_|is_)/.test(cmd)) return false;
        console.warn("[demo-mock] unhandled command:", cmd, args);
        return null;
    }
  }

  // ── Install the Tauri v2 internals the app talks to ─────────────────────────
  window.__TAURI_INTERNALS__ = {
    metadata: {
      currentWindow: { label: "main" },
      currentWebview: { windowLabel: "main", label: "main" },
    },
    transformCallback,
    unregisterCallback: (id) => callbacks.delete(id),
    convertFileSrc: (path) => `asset://localhost/${encodeURIComponent(path)}`,
    invoke: (cmd, args) => {
      // Route the event plugin so listen()/unlisten() wire up real subscriptions.
      if (cmd === "plugin:event|listen") {
        const set = listeners.get(args.event) || new Set();
        set.add(args.handler);
        listeners.set(args.event, set);
        return Promise.resolve(nextEventId++);
      }
      if (cmd === "plugin:event|unlisten") {
        return Promise.resolve();
      }
      try {
        return Promise.resolve(handle(cmd, args || {}));
      } catch (e) {
        console.error("[demo-mock] handler threw for", cmd, e);
        return Promise.reject(String(e));
      }
    },
  };

  // Tauri v2 event-plugin internals: the `UnlistenFn` returned by
  // `@tauri-apps/api` `listen()` calls `__TAURI_EVENT_PLUGIN_INTERNALS__
  // .unregisterListener(event, eventId)` on teardown (event.js `_unlisten`).
  // Without this, ANY component that unsubscribes a `listen()` on destroy (e.g.
  // navigating away from a view holding an `onXxx` subscription) throws
  // "Cannot read properties of undefined (reading 'unregisterListener')". We
  // best-effort prune the maps the invoke router uses so a subscribe→unsubscribe
  // round-trip is a clean no-op under the mock.
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (event, eventId) => {
      const set = listeners.get(event);
      if (set) {
        set.delete(eventId);
      }
      callbacks.delete(eventId);
    },
  };

  // Legacy global some code paths probe.
  window.__TAURI__ = window.__TAURI__ || {};

  // Force the dark theme deterministically (the service reads this key at boot;
  // default is "system", which a light OS/browser would render light — trap).
  try {
    localStorage.setItem("murmur-theme", "dark");
  } catch (_) {
    /* private mode — colorScheme:'dark' on the context still covers us */
  }
})();
