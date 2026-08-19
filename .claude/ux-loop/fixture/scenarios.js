// The twelve states, as fixtures.
//
// Runs in the page, after `tauri-stub.js` and before `main.js`. `pre()` fills
// `window.__replies` so the page's load-time `invoke`s answer the way the shell
// would; `post()` drives the page into the state after `main.js` has wired
// itself up. The driver appended after `main.js` awaits `post()` and then sets
// `window.__ready`, which is what the shot script waits on — a fixed sleep
// races the page's own timers and screenshots a half-painted DOM.
//
// Determinism is the whole point: no Math.random, no Date.now, no network. Two
// runs must produce byte-identical DOM or the loop is comparing noise.

const BOTS = [
  { id: "talent-scout", name: "Talent Scout", title: "Screens inbound applications", description: "", hidden: false, messages: 42 },
  { id: "release-notes", name: "Release Notes", title: "Drafts the changelog", description: "", hidden: false, messages: 7 },
  { id: "expense-manager", name: "Expense Manager", title: "Reconciles receipts", description: "", hidden: false, messages: 0 },
  { id: "support-triage", name: "Support Triage", title: "Sorts overnight tickets", description: "", hidden: false, messages: 118 },
];

// A chunk the way the shell emits one. `kind` is one of the SPEAKER keys in
// main.js: user, agent, thought, tool, progress, result.
const c = (kind, text, extra) => Object.assign({ session: "s1", kind, text }, extra || {});

// An approval the way the shell emits one. Shape lifted from `page.rs`'s `ask`
// helper so the two harnesses cannot disagree about what an ask looks like.
function ask(id, tool, fields, options) {
  return {
    id,
    session: "s1",
    tool,
    fields: fields,
    options: options || [
      { id: "allow-once", name: "Allow once", kind: "allow_once", danger: false },
      { id: "allow-session", name: "Allow for the rest of this session", kind: "allow_always", danger: true },
      { id: "reject-once", name: "Not this time", kind: "reject_once", danger: false },
    ],
  };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Wait for a predicate rather than a duration, for the same reason `page.rs`
// does: a fixed sleep passes on this machine and fails on a slower one.
async function until(fn, tries = 120) {
  for (let i = 0; i < tries; i++) {
    if (fn()) return true;
    await sleep(25);
  }
  return false;
}

async function connect() {
  document.getElementById("connect-btn").click();
  await until(() => !document.getElementById("workspace").classList.contains("hidden"));
}

// Connect, then open the first Bot, the way a person reaches a conversation.
async function openFirstBot() {
  await connect();
  await until(() => document.querySelectorAll("#bots .bot").length > 0);
  document.querySelector("#bots .bot").click();
  await until(() => !document.getElementById("composer").classList.contains("hidden"));
}

const S = {};

// ---------------------------------------------------------------- s01
S.s01 = {
  what: "cold start, nothing configured, no model key",
  pre() {
    window.__replies.connected = false;
    window.__replies.default_home = "C:\\Users\\you\\.openbot";
  },
  async post() {},
};

// ---------------------------------------------------------------- s02
S.s02 = {
  what: "connect panel, sidecar not found",
  pre() {
    window.__replies.connected = false;
    window.__replies.default_home = "C:\\Users\\you\\.openbot";
    window.__throw = {
      connect: "no openbot binary at that path, and none on PATH",
    };
  },
  async post() {
    document.getElementById("connect-btn").click();
    await until(() => document.getElementById("connect-error").textContent.length > 0);
  },
};

// ---------------------------------------------------------------- s03
S.s03 = {
  what: "empty roster, zero Bots",
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = [];
  },
  async post() {
    await connect();
  },
};

// ---------------------------------------------------------------- s04
S.s04 = {
  what: "roster with 4 Bots: idle, working, waiting-on-you, paused-by-inactivity-brake",
  // GAP: the roster carries no status. `renderRoster` in main.js renders a
  // mark, a coat, a name and a subtitle, and nothing else — there is no
  // idle/working/waiting/paused concept anywhere in the payload or the DOM.
  // The four Bots below therefore render as four identical rows. This is not
  // faked into the fixture; it is filed as a P0 against rubric line 2, because
  // an invented status field would let the loop "fix" a screen that does not
  // exist in the product.
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
  },
  async post() {
    await connect();
  },
};

// ---------------------------------------------------------------- s05
S.s05 = {
  what: "thread mid-run, tool steps streaming",
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
    window.__replies.open_bot = {
      session: "s1",
      name: "Talent Scout",
      history: [
        c("user", "find three candidates for the Rust role"),
        c("agent", "I'll start from the applications already in the workspace."),
        c("tool", "fs.list .", { args: { path: "." } }),
        c("result", "\u2713 {\"entries\":[\"applications\",\"notes.md\",\"roles\"]}"),
        c("tool", "fs.read applications/2026-08.jsonl", { args: { path: "applications/2026-08.jsonl" } }),
        c("result", "\u2713 {\"contents\":\"" + "x".repeat(120) + "\"}"),
      ],
    };
  },
  async post() {
    await openFirstBot();
    // One step left open, so the screen shows something happening *now* as
    // distinct from what already happened — which is exactly rubric line 4.
    window.__fire("chunk", c("tool", "shell.exec cargo test --workspace", { args: { command: "cargo test --workspace" } }));
    await sleep(60);
    window.__fire("chunk", c("progress", "running"));
    await sleep(60);
  },
};

// ---------------------------------------------------------------- s06
S.s06 = {
  what: "thread with a pending shell.exec approval",
  pre() {
    S.s05.pre();
  },
  async post() {
    await openFirstBot();
    window.__fire("chunk", c("tool", "shell.exec cargo test --workspace", { args: { command: "cargo test --workspace" } }));
    await sleep(40);
    window.__fire(
      "permission-request",
      ask("a1", "shell.exec: runs a command on the computer", [
        { name: "command", value: "cargo test --workspace", long: false },
        { name: "cwd", value: "/home/you/.openbot/volumes/openbot-workspace", long: false },
        { name: "timeout", value: "600s", long: false },
      ])
    );
    await until(() => !document.getElementById("dialog").classList.contains("hidden"));
  },
};

// ---------------------------------------------------------------- s07
S.s07 = {
  what: "approval denied by policy, cannot be overridden by the client",
  // PARTIAL: a hub `deny` never reaches the window as an ask — the hub refuses
  // the call and the turn carries a refusal result instead. So this is the
  // refusal as the thread renders it, which is the surface a person actually
  // sees. There is no client-side "denied by policy" dialog to shoot, and
  // inventing one would be designing a screen the product does not have.
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
    window.__replies.open_bot = {
      session: "s1",
      name: "Talent Scout",
      history: [
        c("user", "clear out the old application files"),
        c("agent", "I'll remove the archived batch."),
        c("tool", "fs.delete applications/2025-archive.jsonl", { args: { path: "applications/2025-archive.jsonl" } }),
        c("result", "\u2717 {\"error\":\"refused by policy: fs.delete is denied for this account\",\"appealable\":false}"),
        c("agent", "That path is denied by a policy rule, so I have stopped rather than working around it."),
      ],
    };
  },
  async post() {
    await openFirstBot();
  },
};

// ---------------------------------------------------------------- s08
S.s08 = {
  what: "long thread, 200+ steps, scrollback and jump-to-latest",
  pre() {
    const history = [c("user", "run the full migration and report what changed")];
    // Deterministic, and long enough that scrollback is the only way through.
    for (let i = 0; i < 68; i++) {
      history.push(c("tool", "fs.read notes/" + i + ".md", { args: { path: "notes/" + i + ".md" } }));
      history.push(c("result", "\u2713 {\"contents\":\"entry " + i + "\"}"));
      if (i % 17 === 0) history.push(c("agent", "Checked batch " + i + "; nothing unexpected so far."));
    }
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
    window.__replies.open_bot = { session: "s1", name: "Talent Scout", history };
  },
  async post() {
    await openFirstBot();
    const log = document.getElementById("log");
    if (log) log.scrollTop = log.scrollHeight;
    await sleep(80);
  },
};

// ---------------------------------------------------------------- s09
S.s09 = {
  what: "group thread, three Bots, an @mention handoff",
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
    window.__replies.groups = [
      {
        id: "hiring",
        name: "hiring",
        members: [
          { id: "talent-scout", name: "Talent Scout" },
          { id: "release-notes", name: "Release Notes" },
          { id: "expense-manager", name: "Expense Manager" },
        ],
        messages: 6,
      },
    ];
    window.__replies.open_group = {
      session: "s1",
      name: "hiring",
      history: [
        c("user", "@talent-scout what did you find?"),
        c("agent", "Three candidates clear the bar; two need a take-home."),
        c("tool", "bot.send expense-manager", { args: { to: "expense-manager" } }),
        c("result", "\u2713 {\"delivered\":true}"),
        c("agent", "@expense-manager can you price the take-home vouchers?"),
      ],
    };
  },
  async post() {
    await connect();
    await until(() => document.querySelectorAll("#groups .group, #groups button").length > 0);
    const g = document.querySelector("#groups .group, #groups button");
    if (g) g.click();
    await sleep(120);
  },
};

// ---------------------------------------------------------------- s10
S.s10 = {
  what: "computer viewer live, then human takeover holding the lock",
  // PARTIAL: the pane is an <iframe> whose src comes from `open_computer` and
  // is served by the hub's viewer. With no hub there is nothing to paint
  // inside the frame, so what this captures is the pane, its chrome and its
  // error line — not a live page. Takeover is a hub-enforced lock with no
  // window-side surface at all today. Both are filed as gaps rather than
  // mocked, because a fake viewer would let the loop score a screen that
  // does not exist.
  pre() {
    S.s05.pre();
    window.__replies.open_computer = "about:blank";
    window.__replies.computer_alive = true;
  },
  async post() {
    await openFirstBot();
    const btn = document.getElementById("computer");
    if (btn) btn.click();
    await until(() => !document.getElementById("computer-panel").classList.contains("hidden"));
    await sleep(120);
  },
};

// ---------------------------------------------------------------- s11
S.s11 = {
  what: "hard failure: guest disconnected mid-run, token budget exhausted",
  pre() {
    S.s05.pre();
    window.__replies.computer_alive = false;
  },
  async post() {
    await openFirstBot();
    window.__fire("chunk", c("tool", "shell.exec cargo build", { args: { command: "cargo build" } }));
    await sleep(40);
    window.__fire("chunk", c("result", "\u2717 {\"error\":\"the computer disconnected mid-call\"}"));
    window.__fire("chunk", c("agent", "The run stopped: the token budget for this turn is exhausted (24,000 of 24,000 used) and the computer disconnected while a command was open."));
    // The window's own "no computer" banner, which is the surface the product
    // actually shows when the guest goes away.
    const banner = document.getElementById("no-computer");
    const why = document.getElementById("no-computer-why");
    if (why) why.textContent = "the runtime stopped answering on ws://127.0.0.1:8443";
    if (banner) banner.classList.remove("hidden");
    await sleep(80);
  },
};

// ---------------------------------------------------------------- s12
S.s12 = {
  what: "routines list, one firing, one paused, one erroring",
  // GAP: a routine reports `{bot, bot_name, id, enabled, trigger, next}`. There
  // are exactly two expressible states — enabled and paused — and no error
  // field anywhere in the payload or the row. "One erroring" is therefore not
  // reachable, and rubric line 13 asks for three distinguishable states over a
  // model that carries two. Filed rather than faked.
  pre() {
    window.__replies.connected = true;
    window.__replies.roster = BOTS;
    window.__replies.connectors = [
      { id: "linear", secrets: ["linear-token"] },
      { id: "calendar", secrets: [] },
    ];
    window.__replies.routines = [
      { bot: "talent-scout", bot_name: "Talent Scout", id: "morning", enabled: true, trigger: "cron 0 9 * * *", next: "2026-08-20T09:00:00Z" },
      { bot: "release-notes", bot_name: "Release Notes", id: "weekly", enabled: false, trigger: "cron 0 17 * * 5", next: null },
      { bot: "support-triage", bot_name: "Support Triage", id: "hourly", enabled: true, trigger: "cron 0 * * * *", next: "2026-08-20T04:00:00Z" },
    ];
  },
  async post() {
    await connect();
    // Routines live in the rules/wiring panel; open it the way a person would
    // rather than un-hiding the node, so what is captured is the real surface
    // with its real chrome.
    const btn = document.getElementById("rules-btn");
    if (btn) btn.click();
    await until(() => document.querySelectorAll("#routines-list dd, #routines-list dt").length > 0, 80);
    await sleep(120);
  },
};

const chosen = new URLSearchParams(location.search).get("s") || "s01";
window.__scenarioId = chosen;
window.__scenario = S[chosen];
if (!window.__scenario) throw new Error("no such scenario: " + chosen);
window.__scenario.pre();
