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
  // Injected by capture.mjs from package.json; the literal is only a fallback for
  // a manual page load.
  const VERSION = (typeof window !== "undefined" && window.__demoVersion) || "2.0.0";
  const DEMO_MEETING_ID = "m-atlas-roadmap";

  // Deterministic-ish recent timestamps anchored to a fixed day (browser Date is
  // fine here — this runs in the page, not the workflow sandbox).
  const ANCHOR = new Date("2026-08-26T15:00:00");
  const daysAgo = (d, h = 10, m = 0) => {
    const t = new Date(ANCHOR);
    t.setDate(t.getDate() - d);
    t.setHours(h, m, 0, 0);
    return t.toISOString();
  };

  /** A plain calendar day — the shape `find_date` produces for an action item. */
  const ymd = (d) => daysAgo(d).slice(0, 10);

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
  // The demo meeting is 47 minutes long (`durationS: 2820`), so segment and
  // timeline times must be spread across it. They used to sit inside the first
  // 60 seconds, which rendered the timeline bands as an invisible sliver at the
  // far left — the shot looked like the feature was broken.
  const SEGMENTS = [
    { idx: 0, startS: 12, endS: 21, speaker: "me", text: "Okay, let's lock the Q2 roadmap. Three things: Atlas GA, the mobile redesign, and the Sales Engine pilot." },
    { idx: 1, startS: 21, endS: 34, speaker: "others", text: "On Atlas — the Windows loopback spike is the last blocker. We were waiting on the shared sync layer." },
    { idx: 2, startS: 214, endS: 231, speaker: "others", text: "That dependency is gone. Priya's team merged the sync-layer rewrite on Thursday, so the loopback work is unblocked." },
    { idx: 3, startS: 231, endS: 244, speaker: "me", text: "Then let's write it down as a decision. Atlas ships to GA on May 30 and Marcus owns the release checklist." },
    { idx: 4, startS: 612, endS: 627, speaker: "me", text: "On the redesign — I want to cut scope. Onboarding, search, and the note detail view. Everything else waits." },
    { idx: 5, startS: 638, endS: 659, speaker: "others", text: "Agreed, let's not gold-plate it. I'll share the trimmed flow before I go high-fidelity, so we're aligned first." },
    { idx: 6, startS: 1102, endS: 1126, speaker: "others", text: "For the Sales Engine pilot we have two design partners in the pipeline, but only one has actually signed." },
    { idx: 7, startS: 1126, endS: 1141, speaker: "me", text: "Devon, can you confirm the second one this week? The Aug 15 commitment assumes both." },
    { idx: 8, startS: 1744, endS: 1771, speaker: "others", text: "The activation funnel data is in — p95 sync latency is down to 168 milliseconds, and activation is up eleven percent." },
    { idx: 9, startS: 1771, endS: 1783, speaker: "me", text: "That's comfortably under the 180 target. Good. Let's put it in the GA brief." },
    { idx: 10, startS: 2208, endS: 2227, speaker: "others", text: "One risk: if Windows loopback slips, do we cut the platform from GA or move the date?" },
    { idx: 11, startS: 2227, endS: 2246, speaker: "me", text: "We cut the platform. The date is the commitment we made to the partners; the platform isn't." },
    { idx: 12, startS: 2640, endS: 2661, speaker: "others", text: "Then I'll circulate the trimmed roadmap deck, and Sarah takes the migration plan to the partners." },
    { idx: 13, startS: 2661, endS: 2680, speaker: "me", text: "Perfect. Same time next week, and we do a go/no-go two days before the cut." },
  ];

  const TIMELINE = {
    speakers: [
      { speaker: "Sarah", startS: 0, endS: 210 },
      { speaker: "Marcus", startS: 210, endS: 430 },
      { speaker: "Sarah", startS: 430, endS: 640 },
      { speaker: "Anya", startS: 640, endS: 980 },
      { speaker: "Marcus", startS: 980, endS: 1240 },
      { speaker: "Devon", startS: 1240, endS: 1520 },
      { speaker: "Sarah", startS: 1520, endS: 1800 },
      { speaker: "Marcus", startS: 1800, endS: 2200 },
      { speaker: "Sarah", startS: 2200, endS: 2520 },
      { speaker: "Marcus", startS: 2520, endS: 2820 },
    ],
    topics: [
      { label: "Project Atlas — GA date", startS: 0, endS: 520 },
      { label: "Mobile redesign scope", startS: 520, endS: 1020 },
      { label: "Sales Engine — design partners", startS: 1020, endS: 1560 },
      { label: "Activation funnel data", startS: 1560, endS: 2080 },
      { label: "Open risks", startS: 2080, endS: 2520 },
      { label: "Next steps", startS: 2520, endS: 2820 },
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
    totalVisibleEntities: 10,
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
    { id: "codex_cli", available: true },
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

  // Durable Ask-history demo store. Page-local only: it mirrors the SQLite IPC
  // shape for screenshots/E2E without writing chat plaintext to browser storage.
  const ASK_CONVERSATIONS = new Map();
  const ASK_SOURCE_TITLES = new Map();
  const askScopeKey = (scope) => `${scope.kind}:${scope.refId || ""}`;
  const askSourceKey = (source) => `${source.kind}:${source.id}`;
  // Test/demo parity with production: chat rows persist source identity only.
  // Titles are remembered from a backend-owned, visibility-gated source read and
  // hydrated only when a conversation is loaded again.
  window.__demoRememberAskSourceTitles = (sources) => {
    for (const source of sources || []) {
      if (source && source.kind && source.id && source.title) {
        ASK_SOURCE_TITLES.set(askSourceKey(source), source.title);
      }
    }
  };
  const hydrateAskSources = (sources) =>
    (sources || []).flatMap((source) => {
      const title = ASK_SOURCE_TITLES.get(askSourceKey(source));
      return title ? [{ kind: source.kind, id: source.id, title }] : [];
    });
  const askTitle = (question) => {
    const normalized = String(question).trim().split(/\s+/u).join(" ");
    const chars = Array.from(normalized);
    return chars.length > 56 ? `${chars.slice(0, 56).join("").trimEnd()}…` : chars.join("");
  };
  /** `VaultSource[]` — the grounding the assistant answer renders as chips. */
  const ASK_SOURCES = [
    { meetingId: DEMO_MEETING_ID, title: "Q2 Roadmap Planning", startedAt: daysAgo(0), origin: null },
    { meetingId: "m-atlas-kickoff", title: "Project Atlas — Kickoff", startedAt: daysAgo(3), origin: null },
    { meetingId: "m-data-review", title: "Data Review — Activation Funnel", startedAt: daysAgo(9), origin: null },
    { meetingId: "m-eng-sync", title: "Eng Sync — Sales Engine", startedAt: daysAgo(1), origin: null },
  ];

  const ASK_CITATIONS = ["[[Q2 Roadmap Planning]]", "[[Project Atlas — Kickoff]]"];

  const sendPersistedAsk = (scope, args, answer = ASK_ANSWER) => {
    const now = new Date().toISOString();
    const id = args.conversationId || crypto.randomUUID();
    const existing = ASK_CONVERSATIONS.get(id);
    if (existing && askScopeKey(existing.scope) !== askScopeKey(scope)) {
      throw new Error("conversation is unavailable");
    }
    const conversation = existing || {
      id,
      scope,
      title: askTitle(args.question),
      selectedSources: [],
      messages: [],
      createdAt: now,
      updatedAt: now,
    };
    const ordinal = conversation.messages.length;
    const userMessageId = crypto.randomUUID();
    const assistantMessageId = crypto.randomUUID();
    conversation.selectedSources = (args.explicitSources || []).map((source) => ({
      kind: source.kind,
      id: source.id,
    }));
    conversation.updatedAt = now;
    conversation.messages.push(
      {
        id: userMessageId,
        ordinal,
        role: "user",
        content: args.question,
        sources: [],
        citations: [],
        createdAt: now,
      },
      {
        id: assistantMessageId,
        ordinal: ordinal + 1,
        role: "assistant",
        content: answer,
        sources: RICH() ? ASK_SOURCES : [],
        citations: RICH() ? ASK_CITATIONS : [],
        createdAt: now,
      },
    );
    ASK_CONVERSATIONS.set(id, conversation);
    return {
      conversationId: id,
      userMessageId,
      assistantMessageId,
      answer,
      sources: RICH() ? ASK_SOURCES : [],
      citations: RICH() ? ASK_CITATIONS : [],
    };
  };

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
    { idx: 0, done: false, text: "Circulate the trimmed roadmap deck", owner: "Sarah", dueDate: ymd(-2) },
    { idx: 1, done: false, text: "Finalize the Atlas GA release checklist", owner: "Marcus", dueDate: null },
    { idx: 2, done: true, text: "Deliver the mobile onboarding mocks", owner: "Anya", dueDate: ymd(-1) },
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

  const COMPANION_NOTE_ID = "n-companion-atlas";
  const COMPANION_MD = `Kickoff ran long — good energy on the GA date.

- Sync-layer dependency is gone → Atlas unblocked for GA
- Ship the three flows people actually use; the rest waits for Q4
- Marcus: Windows loopback is the last blocker

> Ask Brain: what did we promise Acme on the renewal call?
`;

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
    if (id === COMPANION_NOTE_ID) {
      return {
        id,
        title: "Q2 Roadmap Planning — notes",
        folderId: "f-atlas",
        markdown: COMPANION_MD,
        tags: [],
        properties: {},
        updatedAt: ANCHOR.getTime(),
        createdAt: ANCHOR.getTime(),
        exportedPath: null,
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
      // Per-instance active/inactive toggle (origin/murmur#273 follow-up) —
      // `true` by default on real orgs (see OrgStatus.contextEnabled in
      // models.ts); a fixture missing this field renders the demo's own
      // healthy example org as "Disabled on this device" out of the box.
      contextEnabled: true,
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

  /*
   * SHARED FIXTURE WARNING.
   *
   * This file is not private to the screenshot harness. `e2e/settings-ai/mock-invoke.ts`
   * loads it as the BASE layer for the whole Playwright suite (~450 tests), and each spec
   * then overrides only the commands it asserts on. So adding data to a command that used
   * to fall through to the benign `[]` default silently changes the world every one of
   * those tests boots into.
   *
   * That is not hypothetical: giving `list_dashboards` three boards put a board at the top
   * of every `mur-source-picker`, and four `e2e/ask` specs that click ".sp-row first" and
   * expect a MEETING went red.
   *
   * So the aggregate 2.0 lists that the screenshots need are OPT-IN. `capture.mjs` sets
   * `window.__demoRich`; without it these commands return exactly what they returned
   * before, and the e2e baseline is unchanged. Per-surface data that no spec could have
   * depended on (there was nothing there to depend on) stays unconditional.
   */
  const RICH = () => typeof window !== "undefined" && !!window.__demoRich;

  // ── 2.0 surfaces: Spaces, boards, tasks, reminders, people, receipts ───────
  //
  // Shapes here are taken from the RUST DTOs, not invented: `ContainerNode` /
  // `TypeGroup` / `ItemRow` (storage/models.rs), `TileData` (commands/dashboards.rs),
  // `OrgTask` / `RemindersSnapshot` / `PeopleList` (core/models.ts mirrors). A
  // hand-written mock DEFINES a contract rather than verifying one (angular-zoneless
  // T6), so the only defence against drift is copying the producer's field names —
  // camelCase on the wire, `snake_case` nowhere.

  const ms = (d, h = 10, m = 0) => new Date(daysAgo(d, h, m)).getTime();

  /** One `ItemRow`. `durationS` is meetings-only; every other kind sends null. */
  const IR = (kind, id, title, day, durationS = null) => ({
    kind,
    id,
    title,
    durationS,
    sortAt: ms(day, 9 + (day % 6), (day * 7) % 60),
  });

  const G = (kind, total, items) => ({ kind, total, items });

  // Every container's FULL contents, keyed by container id then kind. The tree's
  // type groups are DERIVED from this the way the reader derives them — the newest
  // few plus the true total — so the sidebar preview and the container page can
  // never disagree, which is exactly the drift a hand-written mock invites.
  const CONTAINER_ITEMS = {
    // containerId `null` is the UNFILED inbox — recordings that belong to no
    // lockable container yet. It renders as the first section of the Spaces tree,
    // above "File recordings with Brain", which is what that button acts on.
    unfiled: {
      meeting: [
        IR("meeting", "m-unfiled-standup", "Monday standup", 0, 900),
        IR("meeting", "m-unfiled-call", "Intro call — Redwood Labs", 1, 1620),
        IR("meeting", "m-unfiled-1on1", "1:1 — Priya", 2, 1380),
      ],
    },
    "f-product": {
      meeting: [
        IR("meeting", "m-all-hands", "Weekly All-Hands", 7, 2040),
        IR("meeting", "m-pricing", "Pricing Workshop", 11, 2400),
        IR("meeting", "m-1on1", "1:1 — Sarah & Marcus", 4, 1260),
        IR("meeting", "m-support", "Support Escalation — Acme", 13, 900),
      ],
      dashboard: [IR("dashboard", "d-atlas", "Atlas — GA readiness", 0)],
    },
    "f-atlas": {
      meeting: [
        IR("meeting", DEMO_MEETING_ID, "Q2 Roadmap Planning", 0, 2820),
        IR("meeting", "m-atlas-kickoff", "Project Atlas — Kickoff", 3, 2460),
        IR("meeting", "m-atlas-ga-review", "Atlas — GA go/no-go", 2, 1680),
        IR("meeting", "m-atlas-partners", "Design partners — intro call", 5, 1980),
        IR("meeting", "m-atlas-sync", "Atlas — weekly sync", 8, 1500),
        IR("meeting", "m-atlas-scope", "Atlas — scope cut", 12, 2160),
      ],
      note: [
        IR("note", DEMO_NOTE_ID, "Atlas — PRD v3", 1),
        IR("note", "n-atlas-ga", "Atlas GA — release checklist", 2),
        IR("note", "n-atlas-risks", "Atlas — open risks", 4),
      ],
    },
    "f-mobile": {
      meeting: [
        IR("meeting", "m-design-review", "Design Review — Mobile Redesign", 1, 2280),
        IR("meeting", "m-mobile-scope", "Mobile — top three flows", 6, 1740),
        IR("meeting", "m-mobile-q4", "Mobile — deferred to Q4", 14, 1320),
      ],
      note: [IR("note", "n-mobile-flows", "Mobile — flow inventory", 3)],
    },
    "f-eng": {
      meeting: [
        IR("meeting", "m-eng-sync", "Eng Sync — Sales Engine", 1, 1560),
        IR("meeting", "m-retro", "Sprint 24 Retro", 5, 1980),
        IR("meeting", "m-data-review", "Data Review — Activation Funnel", 9, 1740),
        IR("meeting", "m-eng-oncall", "On-call handover", 10, 720),
      ],
      note: [
        IR("note", "n-sync-latency", "Sync-layer latency — findings", 2),
        IR("note", "n-eng-runbook", "Release runbook", 7),
      ],
    },
    "f-clients": {
      meeting: [
        IR("meeting", "m-acme", "Customer Call — Acme Corp", 2, 3120),
        IR("meeting", "m-northwind", "Discovery — Northwind", 6, 2760),
        IR("meeting", "m-acme-renewal", "Acme — renewal review", 4, 1860),
      ],
      task: [
        IR("task", "t-acme-redlines", "Acme — return contract redlines", 1),
        IR("task", "t-northwind-followup", "Northwind — send the pilot brief", 3),
      ],
    },
  };

  /** The reader's group shape: the newest few, plus the container's true total. */
  const groupsFor = (id) => {
    const byKind = CONTAINER_ITEMS[id] || {};
    // ItemKind::ORDER — a fixed presentation order; an EMPTY group is ABSENT.
    return ["meeting", "note", "task", "dashboard"]
      .filter((k) => (byKind[k] || []).length)
      .map((k) => G(k, byKind[k].length, byKind[k].slice(0, 3)));
  };

  const CN = (id, name, level, extra = {}) => ({
    id,
    name,
    kind: "meeting",
    level,
    emoji: null,
    tint: null,
    locked: false,
    unlocked: false,
    isRoot: false,
    folders: [],
    groups: groupsFor(id),
    ...extra,
  });

  // The Spaces tree: Spaces (projects) › folders › items, exactly what
  // `list_workspace_tree` returns. "Personal" is SEALED and not session-unlocked,
  // so it carries NO groups at all — not even totals. That is the lock model on
  // screen, and it is why a marketing shot of the sidebar is honest.
  const WORKSPACE_TREE = [
    CN("f-product", "Product", "project", {
      folders: [CN("f-atlas", "Project Atlas", "folder"), CN("f-mobile", "Mobile Redesign", "folder")],
    }),
    CN("f-eng", "Engineering", "project"),
    CN("f-clients", "Clients", "project"),
    CN("f-personal", "Personal", "project", { locked: true, groups: [] }),
  ];

  // ── The "Add related" hierarchy picker's own gated reader ──
  //
  // Deliberately its OWN fixture rather than a reshuffle of WORKSPACE_TREE above:
  // the picker's DTOs are a separate family (three linkable leaf kinds, never
  // task/dashboard; metadata-only containers whose leaves page lazily), and a
  // fixture that pretended otherwise would let the frontend agree with itself.
  // Every command has an explicit case below, so the router's `default:` arm —
  // which answers any unknown `list_*`/`get_*` with `[]` — can never make a
  // missing command look like an empty vault.
  const PICKER_GROUP = (kind, total) => ({ kind, total });

  const PICKER_NODE = (id, name, level, extra = {}) => ({
    id,
    name,
    level,
    emoji: null,
    locked: false,
    unlocked: false,
    linkable: true,
    groups: [PICKER_GROUP("meeting", 2)],
    folders: [],
    ...extra,
  });

  const PICKER_ROW = (kind, id, title) => ({ kind, id, title });

  /** Leaves per `${containerId ?? "u"}|${kind}` — what the lazy pager walks. */
  const PICKER_ITEMS = {
    "u|meeting": [
      PICKER_ROW("meeting", "m-voice", "Voice note — launch thought"),
      PICKER_ROW("meeting", "m-catchup", "Catch-up with Tomasz"),
    ],
    "u|note": [PICKER_ROW("note", "n-loose", "Loose idea: smaller beta")],
    "f-atlas|meeting": [
      PICKER_ROW("meeting", "m-research", "Research kickoff"),
      PICKER_ROW("meeting", DEMO_MEETING_ID, "Q2 Roadmap Planning"),
      PICKER_ROW("meeting", "m-retro", "Launch retro"),
    ],
    "f-atlas|note": [PICKER_ROW("note", "n-brief", "Research brief")],
    "f-atlas|document": [PICKER_ROW("document", "d-plan", "Launch plan.pdf")],
    "f-product|meeting": [PICKER_ROW("meeting", "m-pricing", "Pricing workshop")],
  };

  const PICKER_SPACES = [
    PICKER_NODE("f-product", "Product", "project", {
      groups: [PICKER_GROUP("meeting", 1)],
      folders: [
        PICKER_NODE("f-atlas", "Project Atlas", "folder", {
          groups: [
            PICKER_GROUP("meeting", 3),
            PICKER_GROUP("note", 1),
            PICKER_GROUP("document", 1),
          ],
        }),
        PICKER_NODE("f-mobile", "Mobile Redesign", "folder", { groups: [] }),
      ],
    }),
    // Sealed and not session-unlocked: a NAME, and nothing else — no groups, not
    // even a zero, and `linkable: false` because the write gate would refuse it.
    PICKER_NODE("f-personal", "Personal", "project", {
      locked: true,
      linkable: false,
      groups: [],
    }),
  ];

  const PICKER_BREADCRUMB = {
    "f-product": ["Product"],
    "f-atlas": ["Product", "Project Atlas"],
    "f-mobile": ["Product", "Mobile Redesign"],
  };

  const PICKER_HIT_CONTAINER = {
    "m-research": "f-atlas",
    "m-retro": "f-atlas",
    "n-brief": "f-atlas",
    "d-plan": "f-atlas",
    "m-pricing": "f-product",
  };

  // The RECEIVED forest — containers and items other members shared with this
  // user. Shapes come from the Rust DTOs (`SharedWorkspace` / `SharedContainerNode`
  // / `SharedItemRow` in commands/org_containers.rs), not from the frontend's own
  // interface: a hand-written mock DEFINES a contract rather than verifying one,
  // and the camelCase wire assertion lives on the producing side in
  // `commands/tests/container_share_tests.rs`.
  const SHARED_ITEM = (itemId, title, kind, extra = {}) => ({
    itemId,
    docId: `doc-${itemId}`,
    title,
    kind,
    authorHint: "kgm004a",
    createdAt: "2026-08-20T09:00:00Z",
    orgId: "org-siema",
    orgName: "Siema",
    access: "view",
    position: 0,
    ...extra,
  });

  const SHARED_NODE = (containerId, name, level, extra = {}) => ({
    containerId,
    orgId: "org-siema",
    orgName: "Siema",
    name,
    level,
    emoji: null,
    tint: null,
    access: "view",
    authorHint: "kgm004a",
    folders: [],
    items: [],
    localParentId: null,
    position: 0,
    ...extra,
  });

  const SHARED_WORKSPACE = {
    spaces: [
      SHARED_NODE("c-partners", "Partners", "space", {
        folders: [
          SHARED_NODE("c-contracts", "Contracts", "folder", {
            items: [SHARED_ITEM("si-contract", "Reseller agreement", "document")],
          }),
        ],
        items: [SHARED_ITEM("si-kickoff", "Partner kickoff", "meeting")],
      }),
    ],
    sharedBrains: {
      ...SHARED_NODE(null, "Shared Brains", "virtual", {
        folders: [
          SHARED_NODE("c-loose", "Research", "folder", {
            items: [SHARED_ITEM("si-research", "Market scan", "document")],
          }),
        ],
        items: [SHARED_ITEM("si-loose", "Pricing thoughts", "document")],
      }),
      orgId: "",
      orgName: "",
      authorHint: "",
    },
  };

  const EMPTY_SHARED_WORKSPACE = {
    spaces: [],
    sharedBrains: {
      ...SHARED_NODE(null, "Shared Brains", "virtual"),
      orgId: "",
      orgName: "",
      authorHint: "",
    },
  };

  /** Containers THIS user publishes — drives the owner-side shared marker. */
  const CONTAINER_SHARES = [
    {
      orgId: "org-siema",
      orgName: "Siema",
      folderId: "f-clients",
      containerId: "c-clients",
      access: "view",
      isRoot: true,
      state: "published",
    },
  ];

  /** Flat index so `get_container` and the item pager can answer by id. */
  const CONTAINERS_BY_ID = new Map();
  (function indexContainers(nodes, parent) {
    for (const n of nodes) {
      CONTAINERS_BY_ID.set(n.id, {
        node: n,
        parentId: parent ? parent.id : null,
        parentName: parent ? parent.name : null,
      });
      indexContainers(n.folders || [], n);
    }
  })(WORKSPACE_TREE, null);

  const containerDto = (id) => {
    const hit = CONTAINERS_BY_ID.get(id);
    if (!hit) return null;
    const { node, parentId, parentName } = hit;
    return {
      id: node.id,
      name: node.name,
      level: node.level,
      emoji: node.emoji,
      tint: node.tint,
      locked: node.locked,
      unlocked: node.unlocked,
      isRoot: node.isRoot,
      parentId,
      parentName,
    };
  };

  // ── Boards ────────────────────────────────────────────────────────────────
  const TILE_ROW = (text, meta, status = null) => ({ text, meta, status, source: null });

  const BOARD_TILES = [
    {
      id: "tl-answer",
      dashboardId: "d-atlas",
      kind: "living_answer",
      refId: null,
      title: "What still blocks the Atlas GA cut?",
      span: 2,
      position: 0,
      config: null,
      createdAt: daysAgo(6),
      data: {
        kind: "livingAnswer",
        question: "What still blocks the Atlas GA cut?",
        answer:
          "One blocker is open: Windows loopback capture, owned by Marcus. The shared sync-layer dependency that deferred GA twice was removed on the 24th, and the design-partner track is committed for Aug 15.",
        answeredAt: daysAgo(0, 8, 40),
        withheld: false,
      },
    },
    {
      id: "tl-promises",
      dashboardId: "d-atlas",
      kind: "promises",
      refId: null,
      title: null,
      span: 1,
      position: 1,
      config: null,
      createdAt: daysAgo(6),
      data: {
        kind: "promises",
        owner: null,
        rows: [
          TILE_ROW("Marcus — close the Windows loopback spike", "Marcus Reid · due Sep 2", "open"),
          TILE_ROW("Sarah — circulate the migration plan", "Sarah Chen · due Aug 29", "open"),
          TILE_ROW("Priya — sign off on the GA checklist", "Priya Nair · due Sep 4", "open"),
          TILE_ROW("Devon — confirm the second design partner", "Devon Blake", "late"),
        ],
      },
    },
    {
      id: "tl-meeting",
      dashboardId: "d-atlas",
      kind: "meeting",
      refId: DEMO_MEETING_ID,
      title: null,
      span: 1,
      position: 2,
      config: null,
      createdAt: daysAgo(5),
      data: {
        kind: "meeting",
        id: DEMO_MEETING_ID,
        title: "Q2 Roadmap Planning",
        startedAt: daysAgo(0, 9, 0),
        durationS: 2820,
        hasAudio: true,
      },
    },
    {
      id: "tl-note",
      dashboardId: "d-atlas",
      kind: "note",
      refId: DEMO_NOTE_ID,
      title: null,
      span: 1,
      position: 3,
      config: null,
      createdAt: daysAgo(5),
      data: {
        kind: "note",
        id: DEMO_NOTE_ID,
        title: "Atlas — PRD v3",
        snippet:
          "Atlas removes the shared sync layer. Success is p95 under 180 ms on the activation path, measured on the funnel we reviewed on the 19th.",
        updatedAt: ms(1, 16, 20),
      },
    },
    {
      id: "tl-person",
      dashboardId: "d-atlas",
      kind: "person",
      refId: "p-marcus",
      title: null,
      span: 1,
      position: 4,
      config: null,
      createdAt: daysAgo(4),
      data: { kind: "person", id: "p-marcus", name: "Marcus Reid", mentionCount: 28, openCommitments: 2 },
    },
    {
      id: "tl-pulse",
      dashboardId: "d-atlas",
      kind: "pulse",
      refId: "pr-atlas",
      title: null,
      span: 1,
      position: 5,
      config: null,
      createdAt: daysAgo(4),
      data: { kind: "pulse", entity: "Project Atlas", weekly: [2, 3, 1, 4, 3, 5, 4, 6], total: 34, quietDays: null },
    },
    {
      id: "tl-numbers",
      dashboardId: "d-atlas",
      kind: "numbers",
      refId: "pr-atlas",
      title: null,
      span: 1,
      position: 6,
      config: null,
      createdAt: daysAgo(3),
      data: {
        kind: "numbers",
        entity: "Project Atlas",
        rows: [
          TILE_ROW("p95 sync latency — 168 ms", "Data Review, Aug 19"),
          TILE_ROW("Design partners committed — 2", "Pricing Workshop, Aug 15"),
          TILE_ROW("Activation lift — 11%", "Data Review, Aug 19"),
        ],
      },
    },
    {
      id: "tl-reminders",
      dashboardId: "d-atlas",
      kind: "reminders",
      refId: null,
      title: null,
      span: 1,
      position: 7,
      config: null,
      createdAt: daysAgo(3),
      data: {
        kind: "reminders",
        dueCount: 2,
        rows: [
          TILE_ROW("Send Acme the redlined contract", "due today"),
          TILE_ROW("Book the GA go/no-go review", "due tomorrow"),
        ],
      },
    },
  ];

  const BOARDS = [
    {
      id: "d-atlas",
      title: "Atlas — GA readiness",
      emoji: null,
      tint: "indigo",
      pinned: true,
      position: 0,
      createdAt: daysAgo(21),
      updatedAt: daysAgo(0, 8, 40),
      tileCount: BOARD_TILES.length,
      tileKinds: BOARD_TILES.map((t) => ({ kind: t.kind, span: t.span })),
    },
    {
      id: "d-clients",
      title: "Client pipeline",
      emoji: null,
      tint: "azure",
      pinned: false,
      position: 1,
      createdAt: daysAgo(30),
      updatedAt: daysAgo(2, 11, 10),
      tileCount: 4,
      tileKinds: [
        { kind: "promises", span: 1 },
        { kind: "meeting", span: 1 },
        { kind: "person", span: 1 },
        { kind: "numbers", span: 1 },
      ],
    },
    {
      id: "d-weekly",
      title: "This week",
      emoji: null,
      tint: "mint",
      pinned: false,
      position: 2,
      createdAt: daysAgo(45),
      updatedAt: daysAgo(1, 9, 5),
      tileCount: 3,
      tileKinds: [
        { kind: "reminders", span: 1 },
        { kind: "pulse", span: 1 },
        { kind: "living_answer", span: 2 },
      ],
    },
  ];

  // ── Tasks (org-owned, E2EE) ───────────────────────────────────────────────
  const TASKS = [
    {
      id: "t-acme-redlines",
      docId: "doc-t1",
      itemId: "item-t1",
      sourceDocumentId: null,
      version: 3,
      createdAt: daysAgo(4),
      updatedAt: daysAgo(0, 9, 20),
      canEdit: true,
      canManage: true,
      localRefs: [{ kind: "meeting", refId: "m-acme" }],
      orgId: "org-sonora",
      title: "Acme — return contract redlines",
      description: "Legal flagged the liability cap in §7. Turn it around before the renewal call.",
      status: "inProgress",
      dueAt: daysAgo(-2, 17, 0),
      assigneeUserId: "u-devon",
      subtasks: [
        { id: "s1", title: "Collect legal's comments", done: true },
        { id: "s2", title: "Redline §7 liability cap", done: true },
        { id: "s3", title: "Send back to Acme", done: false },
      ],
      orgRefs: [],
      images: [],
      access: "edit",
    },
    {
      id: "t-loopback",
      docId: "doc-t2",
      itemId: "item-t2",
      sourceDocumentId: null,
      version: 2,
      createdAt: daysAgo(6),
      updatedAt: daysAgo(1, 14, 0),
      canEdit: true,
      canManage: true,
      localRefs: [{ kind: "meeting", refId: DEMO_MEETING_ID }],
      orgId: "org-sonora",
      title: "Close the Windows loopback spike",
      description: "Last blocker on the Atlas GA cut. Decide capture path or cut the platform from GA.",
      status: "todo",
      dueAt: daysAgo(-5, 17, 0),
      assigneeUserId: "u-marcus",
      subtasks: [{ id: "s1", title: "Benchmark WASAPI loopback", done: false }],
      orgRefs: [],
      images: [],
      access: "edit",
    },
    {
      id: "t-migration",
      docId: "doc-t3",
      itemId: "item-t3",
      sourceDocumentId: null,
      version: 5,
      createdAt: daysAgo(9),
      updatedAt: daysAgo(2, 10, 30),
      canEdit: true,
      canManage: false,
      localRefs: [{ kind: "note", refId: DEMO_NOTE_ID }],
      orgId: "org-sonora",
      title: "Circulate the Atlas migration plan",
      description: "One page, for the design partners. Sarah owns it.",
      status: "done",
      dueAt: daysAgo(1, 17, 0),
      assigneeUserId: "u-sarah",
      subtasks: [],
      orgRefs: [],
      images: [],
      access: "view",
    },
  ];

  // ── Reminders ─────────────────────────────────────────────────────────────
  const REM = (id, title, details, day, hour, state, origin, sources) => ({
    id,
    title,
    details,
    dueAt: ms(day, hour, 0),
    repeatEvery: null,
    repeatUnit: null,
    state,
    origin,
    createdAt: ms(day + 5, 10, 0),
    updatedAt: ms(day + 1, 10, 0),
    completedAt: state === "completed" ? ms(day + 1, 12, 0) : null,
    sources,
  });

  const REMINDERS = {
    dueInboxCount: 2,
    inbox: [
      {
        occurrenceId: "occ-1",
        dueAt: ms(0, 9, 0),
        reminder: REM("r-acme", "Send Acme the redlined contract", "Agreed on the call — before the renewal review.", 0, 9, "active", "smart", [
          { kind: "meeting", id: "m-acme", title: "Customer Call — Acme Corp" },
        ]),
      },
      {
        occurrenceId: "occ-2",
        dueAt: ms(0, 14, 30),
        reminder: REM("r-gonogo", "Book the GA go/no-go review", null, 0, 14, "active", "manual", [
          { kind: "meeting", id: DEMO_MEETING_ID, title: "Q2 Roadmap Planning" },
        ]),
      },
    ],
    upcoming: [
      REM("r-partner", "Confirm the second design partner", "Devon was chasing two — only one signed.", -2, 11, "active", "smart", [
        { kind: "meeting", id: "m-northwind", title: "Discovery — Northwind" },
      ]),
      REM("r-migration", "Follow up on the migration plan", null, -4, 10, "active", "manual", [
        { kind: "note", id: DEMO_NOTE_ID, title: "Atlas — PRD v3" },
      ]),
    ],
    completed: [
      REM("r-retro", "Share the Sprint 24 retro actions", null, 3, 16, "completed", "smart", [
        { kind: "meeting", id: "m-retro", title: "Sprint 24 Retro" },
      ]),
    ],
  };

  // ── People ────────────────────────────────────────────────────────────────
  const PEOPLE = {
    totalVisiblePeople: 5,
    people: [
      { id: "p-sarah", name: "Sarah Chen", meetingCount: 31, lastTalked: daysAgo(0, 9, 0), openCommitmentCount: 1, currentFactCount: 7 },
      { id: "p-marcus", name: "Marcus Reid", meetingCount: 28, lastTalked: daysAgo(0, 9, 0), openCommitmentCount: 2, currentFactCount: 9 },
      { id: "p-anya", name: "Anya Petrov", meetingCount: 19, lastTalked: daysAgo(1, 13, 0), openCommitmentCount: 0, currentFactCount: 4 },
      { id: "p-devon", name: "Devon Blake", meetingCount: 15, lastTalked: daysAgo(2, 10, 0), openCommitmentCount: 1, currentFactCount: 5 },
      { id: "p-priya", name: "Priya Nair", meetingCount: 12, lastTalked: daysAgo(5, 15, 0), openCommitmentCount: 1, currentFactCount: 3 },
    ],
  };

  // ── Full brain graph ──────────────────────────────────────────────────────
  // `FullGraphData` — every kind of node in one map (entities, meetings, notes,
  // documents) with typed edges. Built from the existing demo world so the map
  // and the rest of the shots describe the same vault.
  const FG_NODE = (id, kind, label, degree, date = null) => ({ id, kind, label, date, degree });
  const FG_EDGE = (src, dst, srcKind, dstKind, kind, score = 0.8, status = "active") => ({
    src, dst, srcKind, dstKind, kind, score, status,
  });

  const FULL_GRAPH = (() => {
    const nodes = [
      FG_NODE("p-sarah", "entity", "Sarah Chen", 9),
      FG_NODE("p-marcus", "entity", "Marcus Reid", 8),
      FG_NODE("p-anya", "entity", "Anya Petrov", 5),
      FG_NODE("p-devon", "entity", "Devon Blake", 5),
      FG_NODE("p-priya", "entity", "Priya Nair", 4),
      FG_NODE("pr-atlas", "entity", "Project Atlas", 10),
      FG_NODE("pr-sales", "entity", "Sales Engine", 6),
      FG_NODE("pr-mobile", "entity", "Mobile Redesign", 5),
      FG_NODE("pr-acme", "entity", "Acme Corp", 4),
      FG_NODE("pr-northwind", "entity", "Northwind", 3),
      FG_NODE(DEMO_MEETING_ID, "meeting", "Q2 Roadmap Planning", 7, daysAgo(0)),
      FG_NODE("m-atlas-kickoff", "meeting", "Project Atlas — Kickoff", 5, daysAgo(3)),
      FG_NODE("m-atlas-ga-review", "meeting", "Atlas — GA go/no-go", 4, daysAgo(2)),
      FG_NODE("m-eng-sync", "meeting", "Eng Sync — Sales Engine", 4, daysAgo(1)),
      FG_NODE("m-design-review", "meeting", "Design Review — Mobile Redesign", 3, daysAgo(1)),
      FG_NODE("m-acme", "meeting", "Customer Call — Acme Corp", 4, daysAgo(2)),
      FG_NODE("m-northwind", "meeting", "Discovery — Northwind", 3, daysAgo(6)),
      FG_NODE("m-data-review", "meeting", "Data Review — Activation Funnel", 4, daysAgo(9)),
      FG_NODE("m-retro", "meeting", "Sprint 24 Retro", 3, daysAgo(5)),
      FG_NODE("m-pricing", "meeting", "Pricing Workshop", 3, daysAgo(11)),
      FG_NODE(DEMO_NOTE_ID, "note", "Atlas — PRD v3", 6, daysAgo(1)),
      FG_NODE("n-atlas-ga", "note", "Atlas GA — release checklist", 4, daysAgo(2)),
      FG_NODE("n-atlas-risks", "note", "Atlas — open risks", 3, daysAgo(4)),
      FG_NODE("n-sync-latency", "note", "Sync-layer latency — findings", 3, daysAgo(2)),
      FG_NODE("n-mobile-flows", "note", "Mobile — flow inventory", 2, daysAgo(3)),
      FG_NODE("n-eng-runbook", "note", "Release runbook", 2, daysAgo(7)),
      FG_NODE("d-atlas-brief", "document", "Atlas — design-partner brief.pdf", 3, daysAgo(4)),
      FG_NODE("d-acme-msa", "document", "Acme — MSA redlines.docx", 2, daysAgo(2)),
      FG_NODE("d-funnel", "document", "Activation funnel — Q2.xlsx", 2, daysAgo(9)),
    ];
    const E = [
      // people/projects ↔ the meetings they were mentioned in
      ["p-marcus", DEMO_MEETING_ID, "entity", "meeting", "mention"],
      ["p-sarah", DEMO_MEETING_ID, "entity", "meeting", "mention"],
      ["pr-atlas", DEMO_MEETING_ID, "entity", "meeting", "mention"],
      ["pr-atlas", "m-atlas-kickoff", "entity", "meeting", "mention"],
      ["pr-atlas", "m-atlas-ga-review", "entity", "meeting", "mention"],
      ["p-priya", "m-atlas-kickoff", "entity", "meeting", "mention"],
      ["p-anya", "m-design-review", "entity", "meeting", "mention"],
      ["pr-mobile", "m-design-review", "entity", "meeting", "mention"],
      ["p-devon", "m-acme", "entity", "meeting", "mention"],
      ["pr-acme", "m-acme", "entity", "meeting", "mention"],
      ["p-devon", "m-northwind", "entity", "meeting", "mention"],
      ["pr-northwind", "m-northwind", "entity", "meeting", "mention"],
      ["pr-sales", "m-eng-sync", "entity", "meeting", "mention"],
      ["p-marcus", "m-eng-sync", "entity", "meeting", "mention"],
      ["pr-atlas", "m-data-review", "entity", "meeting", "mention"],
      ["p-sarah", "m-pricing", "entity", "meeting", "mention"],
      ["p-marcus", "m-retro", "entity", "meeting", "mention"],
      // people co-occurring
      ["p-sarah", "p-marcus", "entity", "entity", "co_occurrence"],
      ["p-sarah", "p-anya", "entity", "entity", "co_occurrence"],
      ["p-marcus", "p-priya", "entity", "entity", "co_occurrence"],
      ["pr-atlas", "pr-sales", "entity", "entity", "co_occurrence"],
      // notes ↔ meetings and notes ↔ notes
      [DEMO_NOTE_ID, DEMO_MEETING_ID, "note", "meeting", "manual"],
      [DEMO_NOTE_ID, "n-atlas-ga", "note", "note", "wikilink"],
      [DEMO_NOTE_ID, "n-atlas-risks", "note", "note", "wikilink"],
      ["n-atlas-ga", "m-atlas-ga-review", "note", "meeting", "companion"],
      ["n-sync-latency", "m-data-review", "note", "meeting", "semantic"],
      ["n-mobile-flows", "m-design-review", "note", "meeting", "companion"],
      ["n-eng-runbook", "m-retro", "note", "meeting", "semantic"],
      [DEMO_NOTE_ID, "pr-atlas", "note", "entity", "mention"],
      ["n-sync-latency", "pr-atlas", "note", "entity", "mention"],
      // documents
      ["d-atlas-brief", DEMO_NOTE_ID, "document", "note", "wikilink"],
      ["d-atlas-brief", "pr-atlas", "document", "entity", "mention"],
      ["d-acme-msa", "m-acme", "document", "meeting", "semantic"],
      ["d-funnel", "m-data-review", "document", "meeting", "semantic"],
      ["d-funnel", "pr-atlas", "document", "entity", "mention"],
    ].map(([a, b, ak, bk, k]) => FG_EDGE(a, b, ak, bk, k));
    // A couple of SUGGESTED edges — the graph proposes links it hasn't been told about.
    E.push(FG_EDGE("n-atlas-risks", "m-atlas-ga-review", "note", "meeting", "semantic", 0.66, "suggested"));
    E.push(FG_EDGE("pr-mobile", "m-pricing", "entity", "meeting", "co_occurrence", 0.58, "suggested"));
    return {
      nodes,
      edges: E,
      hasHidden: true,
      totalVisibleNodes: nodes.length,
      edgesTruncated: false,
    };
  })();

  // ── Links ("Related") ─────────────────────────────────────────────────────
  // `LinkEdge[]` from `list_links`. Mixed `edgeType`/`createdBy` on purpose: the
  // claim is that the graph builds itself (semantic + wikilink + companion) with
  // manual links alongside, so a one-kind shot would undersell it.
  const LE = (id, otherKind, otherId, otherTitle, edgeType, createdBy, status, score, day) => ({
    id,
    direction: "out",
    otherKind,
    otherId,
    navigationId: null,
    otherTitle,
    edgeType,
    createdBy,
    status,
    score,
    createdAt: ms(day, 10, 0),
  });

  const LINKS_BY_KEY = {
    [`meeting:${DEMO_MEETING_ID}`]: [
      LE(1, "note", DEMO_NOTE_ID, "Atlas — PRD v3", "manual", "user", "active", 1, 1),
      LE(2, "meeting", "m-atlas-kickoff", "Project Atlas — Kickoff", "semantic", "accepted", "active", 0.86, 3),
      LE(3, "document", "d-atlas-brief", "Atlas — design-partner brief.pdf", "wikilink", "auto", "active", 0.79, 4),
      LE(4, "meeting", "m-data-review", "Data Review — Activation Funnel", "semantic", "auto", "suggested", 0.71, 9),
    ],
    [`note:${DEMO_NOTE_ID}`]: [
      LE(5, "meeting", DEMO_MEETING_ID, "Q2 Roadmap Planning", "manual", "user", "active", 1, 0),
      LE(6, "note", "n-atlas-ga", "Atlas GA — release checklist", "wikilink", "auto", "active", 0.9, 2),
      LE(7, "meeting", "m-atlas-ga-review", "Atlas — GA go/no-go", "semantic", "auto", "suggested", 0.68, 2),
    ],
  };

  const BACKLINKS_BY_KEY = {
    [`meeting:${DEMO_MEETING_ID}`]: [
      { id: "n-atlas-ga", kind: "note", title: "Atlas GA — release checklist", timestamp: daysAgo(2) },
    ],
    [`note:${DEMO_NOTE_ID}`]: [
      { id: "d-atlas", kind: "dashboard", title: "Atlas — GA readiness", timestamp: daysAgo(0) },
    ],
  };

  // ── Receipts (ClaimAlignment[]) ───────────────────────────────────────────
  // A grounded note line carries a receipt back to the second of audio it came
  // from; a paraphrased line carries none. That asymmetry is the feature, so the
  // demo data has to show both.
  // `claimIndex` is a RAW line index into the note markdown, so these must point
  // at prose and decision lines — pointing one at line 0 put a receipt on the
  // note's own H1, and on "## Summary", which is not what a receipt means.
  const RECEIPTS = [
    { claimIndex: 3, segmentId: 2, startS: 214, endS: 231, speaker: "others", confidence: 0.94, overlap: 0.81 },
    { claimIndex: 11, segmentId: 3, startS: 231, endS: 244, speaker: "me", confidence: 0.9, overlap: 0.77 },
    { claimIndex: 12, segmentId: 5, startS: 638, endS: 659, speaker: "others", confidence: 0.88, overlap: 0.74 },
    { claimIndex: 13, segmentId: 6, startS: 1102, endS: 1126, speaker: "others", confidence: 0.83, overlap: 0.69 },
  ];


  function handle(cmd, args) {
    switch (cmd) {
      // ── config / product ──
      case "get_config": return currentConfig();
      case "save_config": return null;
      // The version is injected by the driver from package.json (`__demoVersion`),
      // so a release bump can never leave a stale number in the About shot — this
      // mock shipped "0.6.3" into 2.0-era captures before that.
      case "app_info": return { name: "Murmur", version: VERSION, description: "Local-first meeting notes with an on-device brain.", repository: "https://github.com/murmur-io/murmur" };
      case "check_for_update": return { currentVersion: VERSION, latestVersion: VERSION, updateAvailable: false, releaseUrl: "https://github.com/murmur-io/murmur/releases/latest", releaseName: null, releaseNotes: null };
      case "provider_statuses": return PROVIDERS;
      case "consent_to_cloud_egress": case "revoke_cloud_egress": case "consent_to_web_search": return null;

      // ── 2.0: the recording surface is document-first ──
      // It resolves a COMPANION note per meeting and renders it as the live
      // notepad. Unmocked, the record screen sits on "Loading note…" forever.
      case "get_or_create_companion_note":
      case "append_to_companion_note":
        return { noteId: COMPANION_NOTE_ID, meetingWikilink: "[[Q2 Roadmap Planning]]" };
      case "brain_reactions_shadow_count": return 0;
      case "set_brain_contradiction_cards": return null;
      case "brain_model_present": return true;
      case "brain_live_ram_ok": return true;

      // ── 2.0: Spaces — the single workspace hierarchy ──
      case "list_workspace_tree": return RICH() ? WORKSPACE_TREE : [];
      case "get_container": return containerDto(args.id);
      case "create_space":
        return { id: "f-new", name: args.name, path: `Spaces/${args.name}`, parentId: null, locked: false, createdAt: daysAgo(0) };
      case "get_filing_recovery_status":
      case "retry_filing_recovery":
      case "keep_existing_filing_file":
        return { degraded: false, attemptCount: 0, projectionCount: 0, sourceSnapshotCount: 0, issueToken: null, issueKind: null, canKeepExisting: false };

      // ── 2.0: boards ──
      case "list_dashboards": return RICH() ? BOARDS : [];
      case "get_dashboard": {
        const board = BOARDS.find((b) => b.id === args.id) || BOARDS[0];
        // Only the demo board is populated; the others exist to fill the list.
        const tiles = board.id === "d-atlas" ? BOARD_TILES : [];
        return { ...board, tiles, work: TASKS.slice(0, 2) };
      }
      case "get_dashboard_sources": return [];
      case "create_dashboard":
        return { id: "d-new", title: args.title || "New board", emoji: null, tint: null, pinned: false, position: BOARDS.length, createdAt: daysAgo(0), updatedAt: daysAgo(0) };
      case "add_dashboard_tile": case "update_dashboard": case "update_dashboard_tile":
      case "delete_dashboard": case "delete_dashboard_tile": case "reorder_dashboard_tiles":
        return null;

      // ── 2.0: tasks (org-owned) ──
      case "list_tasks": return RICH() ? TASKS : [];
      case "get_task": return TASKS.find((t) => t.id === args.id) || null;
      case "create_task": case "update_task": return TASKS[0];
      case "delete_task": case "set_task_container": return null;

      // ── 2.0: reminders ──
      case "list_reminders": return REMINDERS;
      case "get_reminder_summary": return { dueInboxCount: REMINDERS.dueInboxCount };
      case "audit_reminder_suggestions": return [];
      case "create_reminder": case "update_reminder": return REMINDERS.upcoming[0];
      case "complete_reminder": case "delete_reminder": case "dismiss_reminder_occurrence":
      case "accept_reminder_suggestion": case "dismiss_reminder_suggestion":
        return null;

      // ── 2.0: people ──
      case "list_people": return PEOPLE;

      case "get_full_graph": return FULL_GRAPH;
      // ── 2.0: links ("Related") ──
      case "list_links": return LINKS_BY_KEY[`${args.kind}:${args.id}`] || [];
      case "get_backlinks": return BACKLINKS_BY_KEY[`${args.kind}:${args.id}`] || [];
      case "accept_link": case "dismiss_link": case "link_items": case "unlink_items": return null;

      // ── the "Add related" hierarchy picker's gated reader ──
      // Explicit cases, NOT the `default:` arm: that arm answers any unknown
      // `list_*`/`get_*` with `[]`, which would make a command this app does not
      // have look exactly like a vault with nothing in it.
      case "get_related_picker_bootstrap": {
        // The anchor is `Q2 Roadmap Planning`, filed in Product / Project Atlas —
        // so the modal opens with exactly that path expanded and the anchor row
        // inside a bounded window that contains it.
        const anchorItems = PICKER_ITEMS["f-atlas|meeting"];
        const anchorIndex = anchorItems.findIndex((row) => row.id === args?.anchorId);
        return {
          spaces: RICH() ? PICKER_SPACES : [],
          unclassified: RICH()
            ? [PICKER_GROUP("meeting", 2), PICKER_GROUP("note", 1)]
            : [],
          anchor:
            RICH() && anchorIndex >= 0
              ? {
                  kind: "meeting",
                  containerId: "f-atlas",
                  path: ["f-product", "f-atlas"],
                  index: anchorIndex,
                  offset: 0,
                  items: anchorItems,
                  total: anchorItems.length,
                }
              : null,
        };
      }
      case "list_related_picker_items": {
        const key = `${args?.containerId ?? "u"}|${args?.kind}`;
        const items = RICH() ? PICKER_ITEMS[key] || [] : [];
        const offset = args?.offset ?? 0;
        return {
          kind: args?.kind,
          offset,
          items: items.slice(offset, offset + (args?.limit ?? 24)),
          total: items.length,
        };
      }
      case "search_related_picker": {
        const q = String(args?.query ?? "").trim().toLowerCase();
        const all = RICH()
          ? Object.values(PICKER_ITEMS).flat()
          : [];
        const hits = all
          .filter((row) => row.title.toLowerCase().includes(q))
          .map((row) => ({
            kind: row.kind,
            id: row.id,
            title: row.title,
            breadcrumb: PICKER_BREADCRUMB[PICKER_HIT_CONTAINER[row.id]] || [
              "Not classified",
            ],
          }));
        return { offset: args?.offset ?? 0, hits, total: hits.length };
      }

      // ── 2.0: receipts — grounded note lines trace back to the tape ──
      case "get_note_receipts": return RECEIPTS;

      // ── recorder ──
      case "start_recording": return { meetingId: DEMO_MEETING_ID };
      case "stop_recording": return { meetingId: DEMO_MEETING_ID, markdown: NOTE_MD, exportedPath: `${VAULT}/Meetings/Q2-Roadmap-Planning.md` };
      case "recording_level": return 0.28 + Math.random() * 0.55;
      // Backend truth for "is a recording in flight" — the fresh-webview stage resync
      // (RecorderStore.reconcileStage). Idle by default; tests override per scenario.
      case "recording_status": return { recording: false, meetingId: null, startedAt: null };
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
      // Voiceprint-derived speaker-name suggestions. Returns a LIST; `null` here
      // makes the timeline's `suggestionByLabel` computed throw and blanks the
      // speaker lanes + topic bands.
      case "suggest_speaker_labels":
        return [
          { speaker: "Sarah", suggestedLabel: "Sarah Chen", score: 0.93 },
          { speaker: "Marcus", suggestedLabel: "Marcus Reid", score: 0.9 },
        ];

      // The transcript is loaded LAZILY when the Audio tab opens (it used to ride
      // along on every detail read). Unmocked, that read fell through the generic
      // `get_*` fallback to `[]` and the panel said "No transcript."
      case "get_meeting_segments": return SEGMENTS;
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
            "On the renewal call you committed to two things: redlined contract terms back before the renewal review, and the Atlas migration plan as a one-pager for their team. Devon owns the redlines; Sarah owns the migration plan.",
          command: (args.messages && args.messages.length ? args.messages[args.messages.length - 1].text : ""),
          citations: ["[[Customer Call — Acme Corp]]", "[[Acme — renewal review]]"],
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
      case "list_ask_conversations":
        return Array.from(ASK_CONVERSATIONS.values())
          .filter((c) => askScopeKey(c.scope) === askScopeKey(args.scope))
          .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
          .map((c) => ({
            id: c.id,
            scope: c.scope,
            title: c.title,
            createdAt: c.createdAt,
            updatedAt: c.updatedAt,
            messageCount: c.messages.length,
          }));
      case "load_ask_conversation": {
        const conversation = ASK_CONVERSATIONS.get(args.conversationId);
        if (
          !conversation ||
          askScopeKey(conversation.scope) !== askScopeKey(args.scope)
        ) {
          throw new Error("conversation is unavailable");
        }
        return structuredClone({
          ...conversation,
          selectedSources: hydrateAskSources(conversation.selectedSources),
        });
      }
      case "ask_vault_persisted":
        return sendPersistedAsk(args.scope, args);
      case "chat_meeting_persisted":
        return sendPersistedAsk(
          { kind: "meeting", refId: args.meetingId },
          args,
          "The transcript confirms the decision and its owner.",
        );
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
      // `list_models` returns ModelCatalog { source, options }. Provenance is on the CATALOG so an
      // empty live catalog — a gateway answering with zero models — still says it was fetched, and
      // Refresh stays offered. A bundled catalog is a HINT: an id absent from it is a valid
      // custom id.
      case "list_models": {
        const live = (ids) => ({
          source: "live",
          options: ids.map((id) => ({ id, label: id, source: "live" })),
        });
        const bundled = (rows) => ({
          source: "bundled",
          options: rows.map(([id, label]) => ({ id, label, source: "bundled" })),
        });
        if (args && args.connection === "ollama")
          return live(["llama3.1:8b", "qwen2.5:7b", "mistral:7b"]);
        if (args && args.connection === "gateway") return live([]);
        if (args && args.connection === "codex_cli")
          return bundled([
            ["gpt-5.6-sol", "GPT-5.6 Sol — highest quality"],
            ["gpt-5.6-terra", "GPT-5.6 Terra — balanced"],
            ["gpt-5.6-luna", "GPT-5.6 Luna — fastest"],
          ]);
        return bundled([
          ["claude-opus-5", "Claude Opus 5 — highest quality"],
          ["claude-sonnet-5", "Claude Sonnet 5 — balanced"],
          ["claude-haiku-4-5", "Claude Haiku 4.5 — fastest"],
        ]);
      }
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

      // ── M3-CLIENT: sharing account (Settings → Account section) ──
      // Signed-out shape (matches Rust `commands::AccountStatus`, camelCase over
      // IPC): the demo world has no sharing-server account, so the section shows
      // its normal signed-out "Create or sign in" affordance rather than falling
      // through to the generic `default:` handler (a bare `account_status` name
      // doesn't match the `list_`/`get_`/`has_`/`is_` fallback prefixes below, so
      // an unhandled case would previously resolve `null` — which the FE now
      // treats as a real signed-out status, never a permanent "Loading…" state).
      // The demo world is a HEALTHY app: the local server for Claude is up. Specs that care
      // about a failed bind override this explicitly (`e2e/settings/mcp-status.spec.ts`).
      // Without it the unknown-command fallback returns `[]`, and Settings correctly — but
      // unhelpfully for every unrelated spec — renders the "not running" branch.
      case "get_mcp_status":
        return { state: "listening", port: 8765 };
      case "account_status":
        return {
          loggedIn: false,
          email: null,
          unlockedForSharing: false,
          shareConsented: false,
          serverConfigured: false,
          biometricUnlockAvailable: false,
        };

      // ── Shared Brain (org) ──
      case "org_refresh": return null;
      case "org_list_statuses": return ORGS;
      case "org_list_cached_statuses": return ORGS;
      // Shared containers. Gated on RICH() for the same reason every other
      // aggregate 2.0 list is: this file is the base fixture ~460 specs boot
      // into, and new rows appearing unconditionally would move what "the first
      // row" means for specs that never asked for them.
      case "list_shared_workspace":
        return RICH() ? SHARED_WORKSPACE : EMPTY_SHARED_WORKSPACE;
      case "list_container_share_status": return RICH() ? CONTAINER_SHARES : [];
      case "list_org_share_targets": return [];
      case "preview_container_share": return {
        folderId: args?.folderId ?? "f-clients",
        name: "Clients",
        level: "space",
        noteCount: 3,
        meetingCount: 2,
        folderCount: 1,
        skippedSealed: 1,
        skippedDashboards: 1,
        totalItems: 6,
      };
      case "share_container_to_org":
        return { containerId: "c-new", published: 6, failed: 0 };
      case "unshare_container": return null;
      case "set_container_share_access": return null;
      case "sync_container_shares": return 0;
      case "set_shared_placement": return null;
      case "clear_shared_placement": return null;
      case "list_org_items": return ORG_ITEMS_BY_ORG[args.orgId] || [];
      case "org_resolve_source": return null;
      // Per-meeting (Detail header pill) / bulk (Library row badge) share
      // pairings — both array-shaped; unmocked callers must see `[]`, never
      // the generic `null` default (a bare array-returning command doesn't
      // match the `list_`/`get_` fallback prefixes below).
      case "meeting_org_shares": return [];
      case "org_live_shares_for_source": return [];

      // A typed ItemPage, not the generic array fallback other `list_*` demo
      // commands use. Reads the same CONTAINER_ITEMS the tree derives its groups
      // from, so a container page and its sidebar preview always agree.
      case "list_container_items": {
        const kind = args.kind || "meeting";
        // A null containerId is the unfiled inbox. It rides the same RICH() gate as
        // the tree it renders inside of, so the e2e baseline stays untouched.
        const key = args.containerId == null ? (RICH() ? "unfiled" : "__none__") : args.containerId;
        const all = (CONTAINER_ITEMS[key] || {})[kind] || [];
        const offset = args.offset || 0;
        const limit = args.limit || all.length;
        return { kind, total: all.length, items: all.slice(offset, offset + limit) };
      }

      // ── Vault audit — weekly schedule + AI explain ──
      // Object-shaped (a bare `get_` name would fall through to the `[]`
      // fallback below — the FE expects an AuditSchedule, not an array).
      case "get_audit_schedule": return { enabled: false, lastRunAt: null, nextDueAt: null };
      case "set_audit_schedule": return { enabled: !!args.enabled, lastRunAt: null, nextDueAt: null };
      case "explain_audit_finding":
        return {
          findingId: args.id,
          explanationMd: "This finding flags content worth a second look — review the evidence above and accept or dismiss.",
          provider: "On-device",
        };

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
