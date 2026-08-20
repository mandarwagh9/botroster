"use strict";

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const $ = (id) => document.getElementById(id);

const connectPanel = $("connect");
const connectBtn = $("connect-btn");
const connectError = $("connect-error");
const workspace = $("workspace");
const botsList = $("bots");
const rosterEmpty = $("roster-empty");
const rosterError = $("roster-error");
const toggleHidden = $("toggle-hidden");
const botName = $("bot-name");
const botTitle = $("bot-title");
const botMark = $("bot-mark");
const status = $("status");
const log = $("log");
const noBot = $("no-bot");
const composer = $("composer");
const input = $("input");
const sendBtn = $("send");
const cancelBtn = $("cancel");
const dialog = $("dialog");
const dialogTool = $("dialog-tool");
const dialogFields = $("dialog-fields");
const dialogOptions = $("dialog-options");
const dialogQueue = $("dialog-queue");
const dialogError = $("dialog-error");
const dialogHeading = $("dialog-heading");
const dialogSecret = $("dialog-secret");
const secretLabel = $("dialog-secret-label");
const secretWhy = $("dialog-secret-why");
const secretValueInput = $("dialog-secret-value");
const attachBtn = $("attach");
const attachedList = $("attached");

/// Files attached to the next message: `{name, path}` from `attach_file`.
///
/// The file is already in the workspace by the time it lands here: attaching
/// copies immediately rather than at send, so a person finds out straight away
/// if it cannot be read, instead of after writing a message.
const attached = [];

// Enter submits the credential, because a single-field form where Enter does
// nothing is a form people retype. Escape is not bound here: the approval
// dialog already refuses to close on Escape, and a credential prompt is the
// last place to add an accidental way out that looks like a refusal but
// might be read as one.
secretValueInput.addEventListener("keydown", (e) => {
  if (e.key !== "Enter") return;
  e.preventDefault();
  const ask = asks[0];
  if (ask && ask.secret) supplySecret(ask.id);
});
const nameDialog = $("name-dialog");
const newName = $("new-name");
const nameError = $("name-error");
const computerPanel = $("computer-panel");
const computerFrame = $("computer-frame");
const computerError = $("computer-error");

/** The conversation on screen, or null before one is chosen. */
let session = null;
/** Which Bot that conversation belongs to, for the header and the roster. */
let openName = null;
/** A turn is running: the composer is busy and Stop is live. */
let busy = false;
/** Whether the roster is showing hidden Bots: the docs' "Show hidden chats". */
let showHidden = false;

// Approvals waiting on a person, oldest first. A queue rather than one live
// dialog: the agent may ask about two tools at once, and overwriting the first
// ask's buttons leaves it answerable by nobody until it times out.
const asks = [];

function setStatus(text, kind) {
  status.textContent = text;
  status.className = "status" + (kind ? " " + kind : "");
}

/// Every modal in the window, and the one that outranks the rest.
///
/// An explicit list, not a query for `[role="dialog"]`. A query would tie
/// containment to an ARIA attribute somebody could remove for an unrelated
/// reason, and it would leave nothing for a test to quantify over; the query
/// would be the assertion. Ids rather than elements because several of these
/// are declared much further down this file than `show` is, and reading a
/// `const` in its temporal dead zone throws.
///
/// The list is joined to the page, or it is just a list. A dialog present in
/// the markup but missing from here is measurably not modal (the composer
/// takes focus straight back out of it), and a join between this list and a
/// table in a test does not catch that. `every_modal_in_the_page_is_in_the_list`
/// reads `index.html` and is the assertion that does.
///
/// `dialog` outranks the rest for the same reason it is drawn above them: it
/// cannot be dismissed and a Bot is blocked until it is answered.
const MODAL_IDS = [
  "dialog",
  "palette",
  "rules-dialog",
  "secrets-dialog",
  "name-dialog",
  "edit-dialog",
];
const TOP_MODAL = "dialog";

/// Where focus goes when the last modal closes.
let returnFocusTo = null;

/// Make the window's modality true, whichever box is open.
///
/// Every one of these carries `aria-modal="true"`, and the attribute alone
/// does nothing: an overlay stops a pointer and never stops a keyboard, so
/// without this a panel opens with focus still on the composer and the
/// composer can take focus back from behind it.
///
/// Focus lands on the box, never on a control inside it, except where a
/// dialog then focuses its own field, which New Bot and Edit do and should.
/// For the approval box that rule is load-bearing: its options come in the
/// agent's order with the permitting one normally first, so focusing a
/// button would put one Return between a keyboard and an approval nobody
/// read.
///
/// Containment is `inert` on everything but the top box, so the browser owns
/// the edge cases (Shift+Tab off the first control, controls added later, a
/// second modal underneath) instead of a list of focusable selectors here
/// going quietly out of date.
function applyModality() {
  const app = document.getElementById("app");
  const modals = MODAL_IDS.map((id) => document.getElementById(id)).filter(Boolean);
  const open = modals.filter((m) => !m.classList.contains("hidden"));
  const top = open.find((m) => m.id === TOP_MODAL) || open[open.length - 1] || null;

  // Recorded once, on the way in from outside every modal, so opening a
  // second one over the first still hands focus back to where a person was.
  if (top && !returnFocusTo && !modals.some((m) => m.contains(document.activeElement))) {
    returnFocusTo = document.activeElement;
  }
  for (const child of app.children) {
    child.toggleAttribute("inert", Boolean(top) && child !== top);
  }
  // Not on every re-render: a queue advancing must not yank focus off a
  // control somebody has already tabbed to.
  if (top && !top.contains(document.activeElement)) top.focus();
  if (!top && returnFocusTo) {
    const back = returnFocusTo;
    returnFocusTo = null;
    // After the `inert` above is cleared, or the element receiving focus
    // cannot take it. `isConnected` because it may have been re-rendered away.
    if (back.isConnected) back.focus();
  }
}

function show(el, on) {
  el.classList.toggle("hidden", !on);
  // One hook, so the many `show(...)` calls that open and close these boxes
  // cannot each forget.
  if (MODAL_IDS.includes(el.id)) applyModality();
}

// ------------------------------------------------------------- attachments

/// Draw the chips. Named for the file somebody picked, not for the path it
/// landed at: `attachments/notes-2.md` is what the Bot is told, and showing it
/// here would answer a question nobody asked with a name they did not choose.
function renderAttached() {
  attachedList.innerHTML = "";
  // Two files of one name must not look identical. Attaching `quarterly.md`
  // from two folders is the ordinary case (it is the case the store's `-2`
  // suffix exists for), and two chips reading `quarterly.md` make the remove
  // buttons a coin flip. The tooltip carries the path, and a tooltip is
  // something you have to already know is there.
  //
  // Numbered by the order they were attached, which is the order they are on
  // screen and the order they land in, so `(2)` is the second one either way.
  const seen = new Map();
  for (const item of attached) {
    const n = (seen.get(item.name) || 0) + 1;
    seen.set(item.name, n);
    item.nth = n;
  }
  const total = new Map();
  for (const item of attached) {
    total.set(item.name, (total.get(item.name) || 0) + 1);
  }
  for (const item of attached) {
    const li = document.createElement("li");
    const label = document.createElement("span");
    label.textContent =
      total.get(item.name) > 1 ? `${item.name} (${item.nth})` : item.name;
    // The path is available without taking room, for the case where two
    // files of one name make the chips ambiguous.
    li.title = item.path;
    const drop = document.createElement("button");
    drop.type = "button";
    drop.setAttribute("aria-label", `Remove ${item.name}`);
    drop.textContent = "×";
    drop.addEventListener("click", () => {
      const at = attached.indexOf(item);
      if (at >= 0) attached.splice(at, 1);
      renderAttached();
    });
    li.appendChild(label);
    li.appendChild(drop);
    attachedList.appendChild(li);
  }
  show(attachedList, attached.length > 0);
}

attachBtn.addEventListener("click", async () => {
  if (!session) return;
  try {
    attached.push(await invoke("attach_file"));
    renderAttached();
  } catch (err) {
    // Dismissing the picker is not a failure and must not raise a message.
    if (String(err).includes("cancelled")) return;
    setStatus(String(err), "error");
  }
});

// ---------------------------------------------------------------- transcript

/// Who a transcript line is from, for anyone not reading the colours.
///
/// Without these, the six kinds (the person's own words, the Bot's, its
/// reasoning, a tool call, a progress note and a result) are told apart by
/// styling and by nothing else: all `<div class="msg …">` with text in them.
/// `every_message_kind_is_styled` proves each one is decorated, which is also
/// a proof that the distinction is decoration: alignment and background,
/// both invisible to a screen reader, which would hear one undifferentiated
/// stream and could not tell a Bot's private reasoning from what it actually
/// said.
///
/// Labels differ at the first word. "Bot" and "Bot thinking" are told apart
/// only after the listener has already committed to hearing speech, which is
/// the audible version of allow and deny looking alike.
///
/// No fallback entry. A kind missing from here renders with no prefix, which
/// fails `every_message_kind_says_who_it_is_from`; a default label would
/// make that test unfalsifiable.
const SPEAKER = {
  user: "You",
  agent: "Bot",
  thought: "Reasoning",
  tool: "Tool call",
  progress: "Progress",
  result: "Result",
};

/// What a tool step says about itself, as a sentence a person reads.
///
/// The shell sends the tool's name with its raw arguments, and later the
/// result as `✓ {json}` or `✗ {json}`. A thread that shows both verbatim is
/// a debug log. For the tools this page knows, the pair is summarised into one
/// phrase ("Wrote notes.md · 93 bytes"); for anything else the raw line stands,
/// so a new tool is shown rather than hidden. The full text is always on the
/// row's title, so nothing is lost, only tidied.
/// `1 entry` rather than `1 entries`. A count of one is common enough in a
/// workspace that getting it wrong is visible in the ordinary case.
function plural(n, one, many) {
  return `${n} ${n === 1 ? one : many}`;
}

const STEP_SUMMARY = {
  "fs.write": (a, r) => [
    "Wrote",
    a.path,
    r && r.bytes_written != null ? plural(r.bytes_written, "byte", "bytes") : null,
  ],
  "fs.read": (a, r) => [
    "Read",
    a.path,
    r && r.contents != null ? plural(r.contents.length, "character", "characters") : null,
  ],
  "fs.list": (a, r) => [
    "Listed",
    // "." is the workspace root, and a lone full stop reads as a typo.
    !a.path || a.path === "." ? "the workspace" : a.path,
    r && Array.isArray(r.entries) ? plural(r.entries.length, "entry", "entries") : null,
  ],
  "fs.delete": (a) => ["Deleted", a.path, null],
  "shell.exec": (a, r) => ["Ran", a.command, r && r.exit_code != null ? (r.exit_code === 0 ? "ok" : `exit ${r.exit_code}`) : null],
  "browser.open": (a, r) => ["Opened", a.url, r && r.title ? r.title : null],
  "browser.read": () => ["Read the page", null, null],
  "browser.click": (a) => ["Clicked", a.selector || a.text, null],
  "browser.fill": (a) => ["Filled", a.selector, null],
  "browser.screenshot": () => ["Took a screenshot", null, null],
  "bot.send": (a) => ["Handed off to", a.to || a.bot, null],
  "bot.list": () => ["Listed the team", null, null],
};

/// Try to parse the JSON half of a tool or result line. `null` when it is not
/// JSON, which is the ordinary case for a free-text result.
function jsonTail(text, from) {
  const t = text.slice(from).trim();
  if (!t.startsWith("{") && !t.startsWith("[")) return null;
  try { return JSON.parse(t); } catch { return null; }
}

/// The tool step currently open: the row the next result should complete.
let openStep = null;

function appendChunk(chunk) {
  if (!chunk || !chunk.text) return;

  // A result completes the open step in place rather than adding a row.
  if (chunk.kind === "result" && openStep) {
    completeStep(openStep, chunk);
    openStep = null;
    log.scrollTop = log.scrollHeight;
    return;
  }
  // Progress belongs to the open step and is shown as its state, not as rows.
  if (chunk.kind === "progress" && openStep) {
    openStep.dataset.state = chunk.text;
    const st = openStep.querySelector(".step-state");
    if (st) st.textContent = chunk.text;
    return;
  }

  const el = document.createElement("div");
  el.className = "msg " + chunk.kind;
  const who = SPEAKER[chunk.kind];
  if (who) {
    const label = document.createElement("span");
    label.className = "sr-only";
    // The colon and space are read as a pause, and matter: without them a
    // screen reader runs the label into the first word.
    label.textContent = who + ": ";
    el.appendChild(label);
  }

  if (chunk.kind === "tool") {
    const sp = chunk.text.indexOf(" ");
    const name = sp > 0 ? chunk.text.slice(0, sp) : chunk.text;
    // From the shell as data. `text` is truncated to a readable length, so the
    // JSON inside it does not parse once an argument is long: an `fs.write`
    // carrying a real file printed raw JSON while a short `fs.read` beside it
    // read as a sentence. Falling back to the text keeps older chunks working.
    const args = chunk.args ?? (sp > 0 ? jsonTail(chunk.text, sp) : null);
    el.dataset.tool = name;
    el.dataset.args = args ? JSON.stringify(args) : "";
    el.appendChild(stepMark("running"));
    const text = document.createElement("span");
    text.className = "step-text";
    const summary = STEP_SUMMARY[name];
    const [verb, object] = summary && args ? summary(args, null) : [null, null];
    if (verb) {
      const v = document.createElement("span");
      v.className = "step-verb";
      v.textContent = verb;
      text.appendChild(v);
      if (object) {
        const o = document.createElement("span");
        o.className = "step-object";
        o.textContent = object;
        text.appendChild(o);
      }
    } else {
      const v = document.createElement("span");
      v.className = "tool-name";
      v.textContent = name;
      text.appendChild(v);
      if (sp > 0) text.appendChild(document.createTextNode(" " + chunk.text.slice(sp + 1)));
    }
    el.appendChild(text);
    const state = document.createElement("span");
    state.className = "step-state";
    el.appendChild(state);
    el.title = chunk.text;
    openStep = el;
  } else {
    const body = document.createElement("span");
    body.textContent = chunk.text;
    el.appendChild(body);
    if (chunk.kind === "result") el.title = chunk.text;
  }
  log.appendChild(el);
  log.scrollTop = log.scrollHeight;
}

/// The tick or cross at the head of a step row.
function stepMark(state) {
  const m = document.createElement("span");
  m.className = "step-mark";
  m.dataset.state = state;
  m.setAttribute("aria-hidden", "true");
  return m;
}

/// Fill a step row in with its result: the mark becomes a tick or a cross,
/// and the summary gains its detail ("· 93 bytes") when the tool is known.
function completeStep(step, chunk) {
  const ok = chunk.text.startsWith("✓");
  const mark = step.querySelector(".step-mark");
  if (mark) mark.dataset.state = ok ? "ok" : "failed";
  step.classList.toggle("failed", !ok);
  const st = step.querySelector(".step-state");
  if (st) st.textContent = "";
  const summary = STEP_SUMMARY[step.dataset.tool];
  let args = null;
  try { args = step.dataset.args ? JSON.parse(step.dataset.args) : null; } catch { args = null; }
  const result = jsonTail(chunk.text, 1);
  if (summary && args && ok) {
    const [, , detail] = summary(args, result);
    if (detail) {
      const d = document.createElement("span");
      d.className = "step-detail";
      d.textContent = detail;
      step.querySelector(".step-text").appendChild(d);
    }
  } else if (!ok) {
    // A failure shows its reason in full; it is the one time the raw record is
    // the thing a person needs to read.
    const d = document.createElement("span");
    d.className = "step-detail";
    d.textContent = chunk.text.slice(1).trim();
    step.querySelector(".step-text").appendChild(d);
  }
  step.title = `${step.title}\n${chunk.text}`;
  // Said to a screen reader: the outcome, with the record behind it.
  const sr = document.createElement("span");
  sr.className = "sr-only";
  sr.textContent = (ok ? " Result: " : " Failed: ") + chunk.text.slice(1).trim();
  step.appendChild(sr);
}
// ------------------------------------------------------------------- marks

/// A Bot's mark: one letter on a colour of its own.
///
/// Keyed on the id, never the name. `openbot bot set --rename` keeps the id
/// precisely so a rename carries the conversation, the groups and the
/// routines with it; a mark that changed colour on rename would be the one
/// thing about the Bot that did not survive being renamed, and it is the part
/// a person recognises fastest.
///
/// Generated rather than uploaded. For a tool you run yourself, a picture to
/// manage per Bot is a worse default than a colour that is simply always
/// there, and the job an avatar does in a sidebar of ten Bots is to be
/// distinguishable, which this does without anybody deciding anything.
function markOf(id, name) {
  // FNV-1a: tiny, deterministic, and spreads adjacent ids. `bot-1` and
  // `bot-2` must not come out the same colour, which is the case a sum over
  // characters gets wrong.
  let h = 0x811c9dc5;
  for (const ch of String(id)) {
    h ^= ch.codePointAt(0);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  // The first letter or digit, so "  ledger" and "@ledger" both read L and
  // not a space or a symbol. Falls back to the id, then to a dot, because a
  // mark is never blank.
  const from = [...String(name)].find((c) => /\p{L}|\p{N}/u.test(c));
  const glyph = (from || [...String(id)].find((c) => /\p{L}|\p{N}/u.test(c)) || "•")
    .toUpperCase();
  // `hue` is one of eight coats, not a degree. A 360-stop wheel at one
  // lightness produces some Bots at 2.09:1 that no roster can show; eight
  // named coats, each checked for both themes, is a set a test can walk in
  // full. The field keeps its name because everything that reads it only
  // asks "same as before?".
  return { glyph, hue: h % COATS };
}

/// How many coats a Bot can wear. Matches `--coat-0` … `--coat-7` in
/// `styles.css`, and `every_coat_a_bot_can_wear_is_legible` reads it from here.
const COATS = 8;

/// Displayed name to id, filled wherever the roster is drawn.
///
/// `open_bot` and `open_group` answer with a name, because that is what a
/// person picked and what the header shows. The mark needs the id, so it is
/// looked up here rather than added to the protocol: this is the one place
/// both are already known, and the palette opens Bots from the same roster.
///
/// Falls back to the name when a lookup misses, so a mark is always drawn:
/// wrong-coloured beats absent, and the only way to miss is a conversation
/// opened before the roster arrived.
const idOf = new Map();

/// Build the element, so the roster and the header cannot disagree about how
/// a Bot looks.
function markEl(id, name) {
  const { glyph, hue } = markOf(id, name);
  const el = document.createElement("span");
  el.className = "mark bot-mark";
  el.setAttribute("aria-hidden", "true");
  el.style.setProperty("--mark-hue", String(hue));
  // The colour itself, from the coat token, set here rather than matched
  // by an attribute selector in CSS, which is easy to get subtly wrong.
  el.style.setProperty("--coat", `var(--coat-${hue})`);
  el.textContent = glyph;
  return el;
}

/// Put a Bot's coat on a container, so a row or the whole thread can wear the
/// same colour its mark does. One place, because the roster row, the header
/// and the log all have to agree or the coat means nothing.
function wearCoat(el, id, name) {
  el.style.setProperty("--coat", `var(--coat-${markOf(id, name).hue})`);
}

// ------------------------------------------------------------------- roster

/// Draw the roster. Nothing here mutates state; it renders what it is given,
/// so a failed refresh cannot half-update the list.
function renderRoster(bots) {
  botsList.innerHTML = "";
  for (const bot of bots) idOf.set(bot.name, bot.id);
  for (const bot of bots) {
    const li = document.createElement("li");
    const btn = document.createElement("button");
    btn.className = "bot" + (bot.name === openName ? " open" : "");
    btn.title = bot.description || bot.name;

    btn.appendChild(markEl(bot.id, bot.name));
    wearCoat(btn, bot.id, bot.name);

    const name = document.createElement("span");
    name.className = "bot-name";
    name.textContent = bot.name;
    btn.appendChild(name);

    // The job, or how much conversation there is to come back to. One line
    // either way: a sidebar that reflows as Bots gain history is a sidebar
    // whose entries move under the cursor.
    const sub = document.createElement("span");
    sub.className = "bot-sub";
    sub.textContent = bot.title || (bot.messages ? bot.messages + " messages" : "no messages yet");
    btn.appendChild(sub);

    if (bot.hidden) {
      const tag = document.createElement("span");
      tag.className = "bot-hidden";
      tag.textContent = "hidden";
      btn.appendChild(tag);
    }

    // What `@` has to insert: the id, not the name shown here.
    // `Group::owner_for` matches a mention against `BotId::as_str()`, and
    // `openbot_bots::mentions` stops at the first character outside
    // `[a-z0-9_-]`, so `@Talent Scout` reaches it as `talent` and resolves
    // to nobody. See `mentionEntries`.
    btn.dataset.mention = bot.id;
    btn.addEventListener("click", () => openBot(bot.name));
    li.appendChild(btn);
    botsList.appendChild(li);
  }
  show(rosterEmpty, bots.length === 0 && !showHidden);

  // The empty conversation pane has to say something true. With an empty
  // roster "pick one on the left" points at nothing, so the copy and the
  // button both change with the count rather than being written once for the
  // populated case and left to go wrong in the case a new install actually
  // starts in.
  const none = bots.length === 0;
  $("no-bot-copy").textContent = none
    ? "There are none yet."
    : "Pick one on the left, or make another.";
  $("no-bot-new").textContent = none ? "Make your first Bot" : "New Bot";
}

/// Groups, under the Bots. Clicking one opens it as a session like a Bot's:
/// the thread comes back with it and the composer stays live, because the
/// agent resolves which member answers from the `@mention` in each message.
async function refreshGroups() {
  let all;
  try {
    all = await invoke("groups");
  } catch {
    return;
  }
  const list = $("groups");
  list.innerHTML = "";
  for (const group of all) idOf.set(group.name, group.id);
  for (const group of all) {
    const li = document.createElement("li");
    const btn = document.createElement("button");
    btn.className = "bot" + (group.name === openName ? " open" : "");
    // A group gets a mark too, and not only for consistency with the Bots
    // directly above it. `.bot` is a two-column grid whose first column is the
    // mark; a row built without one puts the name in that column and the
    // members' names beside it on the same line (`LaunchTalent Scout, Ledger`)
    // because the subtitle then lands in row 1 instead of row 2. The mark
    // keeps the shape the grid was written for.
    btn.appendChild(markEl(group.id, group.name));
    wearCoat(btn, group.id, group.name);
    const name = document.createElement("span");
    name.className = "bot-name";
    name.textContent = group.name;
    const sub = document.createElement("span");
    sub.className = "bot-sub";
    // Names, not ids. A group stores ids and a rename keeps them, so
    // joining the raw list would put `talent-scout` under a sidebar entry
    // reading "Recruiting": the same window saying two things about one Bot.
    sub.textContent = group.members.map((m) => m.name).join(", ");
    btn.appendChild(name);
    btn.appendChild(sub);
    // The id here too, though for a different reason than a Bot's. No
    // resolver reads a group mention out of a prompt (`resolve_group` is
    // reached from ACP's `_meta` and from command arguments, never from
    // message text), so this is a name for the model rather than an address.
    // The id is still the better one to insert: `openbot_bots::mentions` is the
    // only mention tokenizer here and it stops at a space, so a group called
    // "Website Launch" would arrive as `website`. A slug survives whatever
    // reads it later, and reads as the name anyway.
    btn.dataset.mention = group.id;
    btn.addEventListener("click", () => openGroup(group.name));
    li.appendChild(btn);
    list.appendChild(li);
  }
  show($("groups-head"), all.length > 0);
}

/// Put the window back to having no conversation open.
///
/// Used when opening one fails. Without it the failure would leave the header
/// naming the previous Bot over an empty transcript, with the composer still
/// live and `session` null, so Send would do nothing at all, silently, which
/// is the worst of the three states available. Either show a conversation or
/// show none; never show the frame of one that is not there.
function closeConversation() {
  session = null;
  openName = null;
  // Nothing to edit with no conversation open; a live button over an empty
  // header is an affirmative state that is not true.
  show($("edit-bot"), false);
  botName.textContent = "";
  botTitle.textContent = "";
  botMark.replaceChildren();
  log.style.removeProperty("--coat");
  // Attachments belong to the message being written, not to the window.
  // Without this, picking a file, changing your mind and opening a different
  // Bot sends it to that one instead: the same "one conversation eating
  // another's" the inbox exists to prevent, on a shorter path. The copy stays
  // in the workspace; only the intent to mention it is dropped.
  attached.length = 0;
  renderAttached();
  log.innerHTML = "";
  openStep = null;
  show(log, false);
  show(composer, false);
  show(noBot, true);
}

async function openGroup(name) {
  if (busy) return;
  await refuseAsks();
  closeConversation();
  setStatus("opening…", "busy");
  try {
    // A group session, like a Bot's: the thread comes back with it, and which
    // member answers is decided per message by who is @mentioned.
    const opened = await invoke("open_group", { name });
    session = opened.session;
    openName = opened.name;
    botName.textContent = opened.name;
    botMark.replaceChildren(markEl(idOf.get(opened.name) || opened.name, opened.name));
    for (const chunk of opened.history) appendChunk(chunk);
    show(noBot, false);
    show(log, true);
    show(composer, true);
    setStatus("connected", "connected");
    await refreshRoster();
    input.focus();
  } catch (err) {
    closeConversation();
    setStatus(String(err), "error");
    refreshRoster();
  }
}

async function refreshRoster() {
  try {
    renderRoster(await invoke("roster", { hidden: showHidden }));
    show(rosterError, false);
    refreshGroups();
  } catch (err) {
    // Never an empty list on failure: that is indistinguishable from having
    // no Bots, and the person would go looking for their work, not the error.
    rosterError.textContent = String(err);
    show(rosterError, true);
  }
}

async function openBot(name) {
  if (busy) return;
  // Awaited, like `openGroup` does: the refusals are on their way out before
  // this window stops being the one that could answer them.
  await refuseAsks();
  closeConversation();
  setStatus("opening…", "busy");
  try {
    const opened = await invoke("open_bot", { name });
    session = opened.session;
    openName = opened.name;
    botName.textContent = opened.name;
    botMark.replaceChildren(markEl(idOf.get(opened.name) || opened.name, opened.name));
    wearCoat(log, idOf.get(opened.name) || opened.name, opened.name);
    // What the Bot already remembers. This arrives in the reply rather than as
    // events, so there is no window in which the page is filtering chunks
    // against a session id it has not been told yet.
    for (const chunk of opened.history) appendChunk(chunk);
    show(noBot, false);
    show(log, true);
    show(composer, true);
    // Bots only. A group has a name and members, not a title and a
    // description, so the form would be three boxes that go nowhere.
    show($("edit-bot"), true);
    setStatus("connected", "connected");
    await refreshRoster();
    input.focus();
  } catch (err) {
    // Nothing opened, so nothing is shown. The roster is redrawn too, or the
    // sidebar would keep its accent edge on a conversation that is not there.
    closeConversation();
    setStatus(String(err), "error");
    refreshRoster();
  }
}

// ------------------------------------------------------------------ approvals

/// Send a decision, and only take the question down once it has landed.
///
/// If the dialog closed first and invoked second, with a failure reported as
/// a status pill in the corner, a decision that never reached the Bot (the
/// request had already settled, by timeout or by the turn ending) would look
/// exactly like one that had: the question vanishes, the person believes
/// they allowed it, and nothing happened. The question stays until the
/// answer is known to have arrived, and when it has not, this says so where
/// the question was rather than beside it.
async function answerAsk(id, optionId) {
  try {
    await invoke("answer_permission", { id, optionId });
  } catch (err) {
    const ask = asks.find((a) => a.id === id);
    if (ask) {
      ask.dead = String(err);
      renderDialog();
      return;
    }
    setStatus(String(err), "error");
    return;
  }
  removeAsk(id);
  renderDialog();
}

function removeAsk(id) {
  const at = asks.findIndex((ask) => ask.id === id);
  if (at >= 0) asks.splice(at, 1);
}

/// Hand over a credential, and only take the question down once it has landed.
///
/// Same rule as `answerAsk` and for a sharper reason: somebody has just typed
/// a secret, and a box that closes on a failure tells them it was stored when
/// it was not. Separate from `answerAsk` because these are different acts
/// (an approval is a choice among offered options, this is a value) and the
/// shell refuses to attach a value to a request that did not ask for one.
///
/// The value is read from the input at the moment of sending and never put
/// anywhere else. Not into `asks`, not into a variable that outlives this
/// call. The input is cleared on both paths, including the failure path, so a
/// credential is not left sitting in the DOM behind a dismissed error.
async function supplySecret(id) {
  const value = secretValueInput.value;
  try {
    await invoke("supply_secret", { id, value });
  } catch (err) {
    secretValueInput.value = "";
    const ask = asks.find((a) => a.id === id);
    if (ask) {
      // "nothing was entered" is not a settled request: the shell keeps it
      // parked so an accidental Enter does not strand the turn. Anything
      // else means it will never be answered.
      if (String(err).includes("nothing was entered")) {
        dialogError.textContent = "Enter the credential, or choose Not this time.";
        show(dialogError, true);
        secretValueInput.focus();
        return;
      }
      ask.dead = String(err);
      renderDialog();
      return;
    }
    setStatus(String(err), "error");
    return;
  }
  secretValueInput.value = "";
  removeAsk(id);
  renderDialog();
}

function enqueueAsk(ask) {
  // An ask for a conversation this window is no longer showing has nobody to
  // answer it. Refuse it now rather than leaving it parked: fail closed, and
  // do it at the speed of a click instead of a timeout.
  if (ask.session !== session) {
    invoke("answer_permission", { id: ask.id, optionId: "" }).catch(() => {});
    return;
  }
  asks.push(ask);
  renderDialog();
}

function renderDialog() {
  const ask = asks[0];
  if (!ask) {
    show(dialog, false);
    return;
  }
  // A credential request is a different question from an approval (it wants
  // a value, not a choice), so it gets a different heading, an input, and a
  // different pair of buttons. Everything below still runs: the name and the
  // reason are rendered as ordinary fields, because they are exactly what a
  // person needs in order to answer.
  const secret = ask.secret || null;
  dialogHeading.textContent = secret ? "A credential is needed" : "Approval needed";
  secretValueInput.value = "";
  show(dialogSecret, Boolean(secret));
  if (secret) {
    secretLabel.textContent = secret.name;
    secretWhy.textContent = secret.why;
    secretValueInput.setAttribute("aria-label", `value for ${secret.name}`);
  }

  // Say the credential's name once. Left alone, the generic approval
  // rendering shows it three times over: the tool line (`credential needed:
  // demo-token`), the argument table (`name  demo-token`), and the input's
  // label (`demo-token`). A person reading the same identifier three times is
  // a person being given no help at all. The label above the box is the one
  // that survives, because it is the one attached to the thing they are
  // about to type into.
  show(dialogTool, !secret);
  show(dialogFields, !secret);

  // The ACP title arrives as `fs.write: writes a file into the workspace`,
  // a name and a sentence in one string. Split at the first `: ` so the name
  // can be set as data and the sentence as prose; monospacing a sentence
  // makes a reason look like a payload, which is the opposite of its job.
  // A title with no `: ` is shown whole, as data.
  dialogTool.replaceChildren();
  const sep = ask.tool.indexOf(": ");
  const toolName = document.createElement("code");
  toolName.textContent = sep > 0 ? ask.tool.slice(0, sep) : ask.tool;
  dialogTool.appendChild(toolName);
  if (sep > 0) {
    dialogTool.appendChild(document.createTextNode(" " + ask.tool.slice(sep + 2)));
  }
  // Named fields, not a JSON blob. The docs ask a person to review the target,
  // the scope and the values, and a pretty-printed object buries the target in
  // the payload: for an `fs.write`, the filename sits above however many
  // lines of file contents.
  dialogFields.innerHTML = "";
  for (const field of ask.fields) {
    const dt = document.createElement("dt");
    dt.textContent = field.name;
    const dd = document.createElement("dd");
    dd.textContent = field.value;
    // Long values get their own scrollable block. Scrollable, not truncated:
    // this is the surface that decides whether the thing runs, so nothing in
    // it is allowed to be out of reach.
    if (field.long) dd.className = "long";
    dialogFields.appendChild(dt);
    dialogFields.appendChild(dd);
  }
  dialogQueue.textContent = asks.length > 1 ? asks.length - 1 + " more waiting" : "";

  dialogOptions.innerHTML = "";
  // The decision did not arrive and cannot be retried: the request settled
  // without it. The only control left is one that acknowledges that, so the
  // buttons are replaced rather than left there implying another click would
  // work.
  if (ask.dead) {
    dialogError.textContent = ask.dead;
    show(dialogError, true);
    const ok = document.createElement("button");
    ok.textContent = "Dismiss";
    ok.addEventListener("click", () => {
      removeAsk(ask.id);
      renderDialog();
    });
    dialogOptions.appendChild(ok);
    show(dialog, true);
    return;
  }
  show(dialogError, false);
  if (secret) {
    // Two buttons, because there are two answers: hand it over, or do not.
    // The agent's other options are not offered: "allow always" has no
    // meaning for a credential, and showing it would invite a click that
    // supplies nothing.
    const store = document.createElement("button");
    store.className = "primary";
    store.textContent = "Store and continue";
    store.addEventListener("click", () => supplySecret(ask.id));
    const not = document.createElement("button");
    not.className = "danger";
    not.textContent = "Not this time";
    not.addEventListener("click", () => answerAsk(ask.id, ""));
    dialogOptions.appendChild(store);
    dialogOptions.appendChild(not);
    show(dialog, true);
    // Focus after the dialog is shown, or the input is not focusable yet.
    secretValueInput.focus();
    return;
  }
  for (const option of ask.options) {
    const btn = document.createElement("button");
    // Allow and deny must not look the same in a security dialog. The shell
    // decides this, not the page. `kind` is ACP's vocabulary and
    // `PermissionOptionKind` is `#[non_exhaustive]`: a prefix match here would
    // dress an unclassifiable option in the accent styling reserved for the
    // permitted choice. `refuses` answers it in Rust and fails closed.
    // One accent button per dialog. The shell orders options narrowest first
    // and `danger` marks the refusals, so the first non-refusing option is the
    // smallest grant and gets the accent; any further grant (allow for the
    // session) is the larger commitment and reads quieter. Positional on
    // purpose: a kind-string match would have to know every kind ACP may add.
    const isFirstGrant = !option.danger && !dialogOptions.querySelector(".primary");
    btn.className = option.danger ? "danger" : isFirstGrant ? "primary" : "quiet";
    btn.textContent = option.name;
    btn.addEventListener("click", () => answerAsk(ask.id, option.id));
    dialogOptions.appendChild(btn);
  }
  // Only if the agent offered no way to decline. It normally does, and adding
  // one anyway gives a person two buttons that both mean no: one a refusal
  // the hub records, the other a `Cancelled` meaning "nobody was asked".
  if (!ask.options.some((o) => o.danger)) {
    const refuse = document.createElement("button");
    refuse.className = "danger";
    refuse.textContent = "Refuse";
    refuse.addEventListener("click", () => answerAsk(ask.id, ""));
    dialogOptions.appendChild(refuse);
  }
  show(dialog, true);
}

/// Take down dialogs something else has already answered: `cancel` refuses
/// what it was waiting on, `disconnect` refuses everything. Only for those:
/// dropping an unanswered ask leaves it parked at the other end until it
/// times out, with the turn stalled behind it.
function forgetAsks(ids) {
  if (ids) {
    for (const id of ids) removeAsk(id);
  } else {
    asks.length = 0;
  }
  renderDialog();
}

/// Refuse everything queued, because this window is about to stop being the
/// one that could answer it. All at once: refusals have no order between
/// them, and the case the queue exists for is several tool calls together.
function refuseAsks() {
  const queued = asks.splice(0, asks.length);
  renderDialog();
  return Promise.all(
    queued.map((ask) =>
      invoke("answer_permission", { id: ask.id, optionId: "" }).catch(() => {}),
    ),
  );
}

// -------------------------------------------------------------- connection

/// What a failed connect means, in the person's vocabulary.
///
/// J2 has more than one way to fail and they need different answers: a runtime
/// that is not there cannot be fixed the way a runtime with no model can, and
/// telling somebody the wrong one sends them to retry the wrong thing. The
/// runtime states its own fault precisely, so this maps that to what it means
/// and what is safe, and leaves the runtime's words in the expander as
/// evidence.
///
/// Ordered: the key case has to be tested before the model case, because the
/// runtime words it as "no usable model: $KEY is not set" and the model rule
/// would otherwise swallow it.
const CONNECT_FAULTS = [
  {
    match: /is not set/i,
    what: "The runtime has a model configured, but not the key it needs.",
    safe: "Nothing started. No Bot ran, and nothing on this computer was touched.",
    demo: true,
  },
  {
    match: /no usable model|no model configured/i,
    what: "The runtime started, but it has no model to use.",
    safe: "Nothing started. No Bot ran, and nothing on this computer was touched.",
    demo: true,
  },
  {
    match: /is not on your PATH|no openbot binary at/i,
    what: "The openbot runtime is not where this window looked for it.",
    // No demo offer here: the demo runs *in* the runtime, so with no runtime
    // there is nothing to run it. Offering it would be a button that fails
    // the same way.
    safe: "Nothing started.",
    demo: false,
  },
];

/// The runtime's first meaningful line, which is where it states the fault.
function whyFrom(raw) {
  const lines = String(raw)
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  // Skip the window's own framing line; the runtime's own words come after it.
  const said = lines.find((l) => /^Error:/i.test(l)) || lines[lines.length - 1] || "";
  return said.replace(/^Error:\s*/i, "");
}

function clearConnectError() {
  show(connectError, false);
  show($("connect-demo"), false);
  emphasise(connectBtn, true);
  emphasise($("connect-demo"), false);
}

/// Which button is the primary one. There is exactly one per screen, and after
/// a failed connect it is not Connect: pressing Connect again does the same
/// thing and fails the same way. The action that resolves the state gets the
/// emphasis, or the loudest control on the screen is the one that cannot work.
function emphasise(btn, on) {
  btn.classList.toggle("primary", on);
}

function showConnectError(err) {
  const raw = String(err);
  const fault = CONNECT_FAULTS.find((f) => f.match.test(raw));
  $("connect-error-what").textContent = fault
    ? fault.what
    : "The runtime did not start.";
  // Never the whole message as the message: the why is one line, and the rest
  // is evidence that belongs behind the expander.
  $("connect-error-why").textContent = whyFrom(raw);
  $("connect-error-safe").textContent = fault
    ? fault.safe
    : "Nothing started.";
  $("connect-error-raw").textContent = raw;
  show(connectError, true);
  const offerDemo = Boolean(fault && fault.demo);
  show($("connect-demo"), offerDemo);
  emphasise($("connect-demo"), offerDemo);
  emphasise(connectBtn, !offerDemo);
}

/// Connect, optionally in the scripted demo.
///
/// `demo` is passed explicitly rather than read from a global so the button
/// that offers it and the button that does not cannot drift apart.
async function connect(demo = false) {
  const openbot = $("openbot-path").value.trim();
  const home = $("home-path").value.trim();
  const hub = $("hub-url").value.trim();
  clearConnectError();
  connectBtn.disabled = true;
  try {
    const found = await invoke("connect", {
      openbot: openbot || "openbot",
      // No tilde. If the field is somehow empty the shell resolves it, which
      // is the same answer the field was filled with, and never a path with
      // a `~` in it for openbot to take literally.
      home: home || (await invoke("default_home")),
      hub: hub || "http://127.0.0.1:9812",
      demo,
    });
    await enterWorkspace();
    // "Connected" has to mean what a person reads into it. The agent is
    // running either way; whether there is a computer behind it is a separate
    // question, and answering it here is the difference between finding out
    // now and finding out after writing a message.
    if (found.computer) {
      setStatus(`connected · ${found.tools} tools`, "connected");
    } else {
      setStatus("no computer", "error");
      showComputerProblem(found.why);
    }
  } catch (err) {
    showConnectError(err);
  } finally {
    connectBtn.disabled = false;
  }
}

/// Say that the agent is up and the computer is not, with what to do.
///
/// A banner rather than the status pill alone: the pill is two words in a
/// corner, and this is the reason nothing a person tries next will work.
function showComputerProblem(why) {
  const banner = $("no-computer");
  $("no-computer-why").textContent = why || "the hub did not answer";
  show(banner, true);
}

async function enterWorkspace() {
  show(connectPanel, false);
  show(workspace, true);
  // Enter the no-conversation state by calling the function that defines
  // it, rather than trusting the markup to have started there.
  // `closeConversation` hides Edit and `openBot` shows it, so the invariant
  // holds across every transition, and the state a person arrives in is not
  // a transition: without this call the button would sit in the header on
  // first connect, live, over a blank name, and clicking it would do nothing
  // at all, on the one screen every new install starts on.
  closeConversation();
  setStatus("connected", "connected");
  await refreshRoster();
  // What `@` and `/` can offer. Not awaited into the roster's path: a slow
  // `skill ls` should not hold up the sidebar, and an empty menu for the first
  // second is a smaller failure than a window that looks hung.
  refreshMentionable();
}

async function disconnect() {
  try {
    await invoke("disconnect");
  } catch (err) {
    setStatus(String(err), "error");
    return;
  }
  closeConversation();
  closeComputer();
  forgetAsks();
  show(workspace, false);
  show(connectPanel, true);
}

// ------------------------------------------------------------------- turns

async function sendPrompt(text) {
  const joining = busy;
  busy = true;
  show(cancelBtn, true);
  setStatus(joining ? "redirecting…" : "thinking…", "busy");
  // Echoed here only when it starts the turn. A message that joins one is
  // queued until the next step boundary and the agent announces it then, as a
  // `Redirected` event; echoing it here as well would put it in the
  // transcript twice, once where it had not happened yet. Showing something
  // as delivered before it is delivered is the kind of lie a transcript
  // exists to prevent.
  if (!joining) appendChunk({ kind: "user", text });
  try {
    // The reply is not in here: every word arrived as a `chunk` event while
    // the turn was running. What comes back is only how it ended.
    // Taken before the call and cleared after it: if `prompt` throws, the
    // chips are still there and the message can be sent again without
    // re-picking every file. The copies are already in the workspace either
    // way; re-attaching the same file would make a second one.
    const sending = attached.map((a) => a.path);
    const turn = await invoke("prompt", { session, text, attached: sending });
    attached.length = 0;
    renderAttached();
    setStatus(turn.note || "connected", turn.note ? "" : "connected");
  } catch (err) {
    // Includes the case where a message that joined a turn arrived after the
    // Bot had stopped reading: the agent says so rather than pretending it
    // landed, and the text goes back in the box so it can be sent again
    // without being retyped.
    setStatus(String(err), "error");
    if (joining && !input.value.trim()) input.value = text;
  } finally {
    busy = false;
    show(cancelBtn, false);
    refreshRoster();
  }
}

// ------------------------------------------------------------------- wiring

$("no-computer-dismiss").addEventListener("click", () => show($("no-computer"), false));

listen("chunk", (event) => {
  if (event.payload.session === session) appendChunk(event.payload);
});
listen("permission-request", (event) => enqueueAsk(event.payload));
// The turn ended, so nothing is waiting on these any more. Taking them down
// is not the same as answering them: they were refused on the way out, and a
// dialog that outlives the turn it belongs to is a question about a call that
// will never be made.
listen("permission-withdrawn", (event) => forgetAsks(event.payload || []));

connectBtn.addEventListener("click", () => connect(false));
// Value before configuration: the demo needs no key and no model, so the one
// action offered on a J2 failure is the one that shows a Bot doing real work
// on real tools. Wrapped for the same reason as above — a MouseEvent is truthy.
$("connect-demo").addEventListener("click", () => connect(true));
$("disconnect").addEventListener("click", disconnect);
$("pick-home").addEventListener("click", async () => {
  const folder = await invoke("pick_folder");
  if (folder) setPath($("home-path"), folder);
});

for (const id of ["openbot-path", "home-path", "hub-url"]) {
  // Typed as well as picked: a tooltip that only tracked the picker would go
  // stale the moment somebody edited the field, which is worse than none.
  // The hub URL is here too: it has no picker, but it is a value a person is
  // asked to confirm before connecting, and the argument for showing it whole
  // does not depend on how it got there.
  $(id).addEventListener("input", (e) => {
    e.target.title = e.target.value;
  });
}

$("pick-openbot").addEventListener("click", async () => {
  const file = await invoke("pick_binary");
  if (file) setPath($("openbot-path"), file);
});

/// Set a path field, and keep the whole value reachable.
///
/// A field you cannot read is a field you cannot check. The panel is 560px
/// wide and a real Windows path does not fit, so the end of the value, the
/// part that says which binary, is the part cut off. The approval card
/// already holds the rule: every argument shown whole, wrapped rather than
/// truncated, because hiding part of the input defeats the purpose.
/// Confirming a path before connecting is the same act.
///
/// The tooltip is the standard affordance for it and is read by assistive
/// tech, so the value stays available without the panel growing to fit the
/// longest path anybody might have.
function setPath(el, value) {
  el.value = value;
  el.title = value;
}

toggleHidden.addEventListener("click", () => {
  showHidden = !showHidden;
  toggleHidden.textContent = showHidden ? "Hide hidden chats" : "Show hidden chats";
  refreshRoster();
});

$("new-bot").addEventListener("click", () => {
  newName.value = "";
  nameError.textContent = "";
  show(nameDialog, true);
  newName.focus();
});
// The empty pane's button is the same action, not a second implementation of
// it: one handler, so the two cannot drift into asking for different things.
$("no-bot-new").addEventListener("click", () => $("new-bot").click());
$("name-cancel").addEventListener("click", () => show(nameDialog, false));
$("name-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const name = newName.value.trim();
  if (!name) {
    nameError.textContent = "A Bot needs a name.";
    return;
  }
  show(nameDialog, false);
  // Opening a Bot that does not exist creates it, so "new" and "open" are one
  // act, which is what a sidebar makes them anyway.
  await openBot(name);
});

// ------------------------------------------------------------- edit a Bot

const editDialog = $("edit-dialog");
const editName = $("edit-name");
const editTitle = $("edit-title");
const editDescription = $("edit-description");
const editError = $("edit-error");

/// What the fields held when the dialog opened, so only what changed is sent.
///
/// The command treats an absent field as unchanged, and this is what makes
/// that useful: a form that posted all three would write back whatever
/// was on screen when it loaded, so opening the dialog and saving would
/// silently overwrite a description edited somewhere else in the meantime.
let editWas = null;

/// Fill the dialog from the roster rather than from the header.
///
/// The header shows the name and the title; the description is not on screen
/// anywhere, and a form that opened with an empty box for it would look like
/// the Bot has none, then save the emptiness.
async function openEditBot() {
  if (!openName) return;
  editError.textContent = "";
  let bot = null;
  try {
    const all = await invoke("roster", { hidden: true });
    bot = all.find((b) => b.name === openName) || null;
  } catch (err) {
    editError.textContent = String(err);
  }
  if (!bot) {
    // Better than a form full of guesses. Groups land here too: they have a
    // roster entry of their own kind and none of these fields.
    editError.textContent =
      "This conversation has no editable profile — only Bots do.";
    editWas = null;
    show(editDialog, true);
    return;
  }
  editWas = {
    hidden: Boolean(bot.hidden),
    id: bot.id,
    name: bot.name,
    title: bot.title || "",
    description: bot.description || "",
    // What deleting it would destroy. Routines and groups are keyed by id,
    // which is why the id is kept here and not just the name.
    messages: bot.messages || 0,
  };
  $("dup-name").value = "";
  $("dup-error").textContent = "";
  // Never opens already-armed: a confirm left showing from last time is a
  // Delete button one click from a Bot nobody meant to touch.
  show($("del-confirm"), false);
  show($("del-ask"), true);
  $("del-error").textContent = "";
  $("hide-error").textContent = "";
  await describeHiding();
  editName.value = editWas.name;
  editTitle.value = editWas.title;
  editDescription.value = editWas.description;
  show(editDialog, true);
  editName.focus();
}

/// Say what hiding this Bot would and would not do.
///
/// Hiding is not pausing. `openbot bot hide` says so and lists what still
/// runs, because SPEC §8 calls it a genuine footgun: the Bot leaves the
/// sidebar and goes on working, and spending, out of sight. A window that
/// offered the same button without the same sentence would be the same act
/// with the safeguard removed.
async function describeHiding() {
  const btn = $("hide-bot");
  btn.textContent = editWas.hidden ? "Show in sidebar" : "Hide from sidebar";
  if (editWas.hidden) {
    $("hide-what").textContent = "This Bot is hidden. Its work never stopped.";
    return;
  }
  let live = [];
  try {
    live = (await invoke("routines")).filter(
      (r) => r.bot === editWas.id && r.enabled,
    );
  } catch {
    // A count that cannot be read is left out rather than reported as none.
  }
  $("hide-what").textContent = live.length
    ? `Keeps its conversation, and keeps running: ${live
        .map((r) => `${r.id} (${r.trigger})`)
        .join(", ")}. Hiding does not pause anything.`
    : "Keeps its conversation. Hiding does not pause anything it is given later.";
}

$("hide-bot").addEventListener("click", async () => {
  if (!editWas) return;
  try {
    await invoke("bot_hide", { bot: editWas.id, hidden: !editWas.hidden });
  } catch (err) {
    $("hide-error").textContent = String(err);
    return;
  }
  const nowHidden = !editWas.hidden;
  editWas.hidden = nowHidden;
  show(editDialog, false);
  // A Bot hidden while open would leave the conversation on screen and out of
  // the list: the window disagreeing with itself. Showing one does not have
  // that problem, so only hiding closes it.
  if (nowHidden && !showHidden) closeConversation();
  await refreshRoster();
});

$("edit-bot").addEventListener("click", openEditBot);
$("edit-cancel").addEventListener("click", () => show(editDialog, false));
$("edit-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (!editWas) return show(editDialog, false);
  const name = editName.value.trim();
  if (!name) {
    editError.textContent = "A Bot needs a name.";
    return;
  }
  // Only what moved. Sending an unchanged field is harmless with one window
  // and becomes an overwrite the moment two are open on one home.
  const change = { bot: openName };
  if (name !== editWas.name) change.rename = name;
  if (editTitle.value !== editWas.title) change.title = editTitle.value;
  if (editDescription.value !== editWas.description) {
    change.description = editDescription.value;
  }
  if (!change.rename && change.title === undefined && change.description === undefined) {
    // Nothing to do, and saying so beats a round trip that reports success
    // for having done nothing.
    return show(editDialog, false);
  }
  try {
    await invoke("bot_describe", change);
  } catch (err) {
    editError.textContent = String(err);
    return;
  }
  show(editDialog, false);
  // The window addresses Bots by name, so a rename has to reach the header
  // and the sidebar before anything else is clicked.
  if (change.rename) {
    openName = change.rename;
    botName.textContent = change.rename;
  }
  if (change.title !== undefined) botTitle.textContent = change.title;
  await refreshRoster();
});

$("dup-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const dupError = $("dup-error");
  dupError.textContent = "";
  const newName = $("dup-name").value.trim();
  if (!newName) {
    dupError.textContent = "The copy needs a name of its own.";
    return;
  }
  if (!openName) return;
  try {
    await invoke("bot_duplicate", { bot: openName, newName });
  } catch (err) {
    // Stays open on failure: "a bot named X already exists" is answerable by
    // typing a different name, and closing the dialog would make somebody
    // reopen it to read the reason.
    dupError.textContent = String(err);
    return;
  }
  $("dup-name").value = "";
  show(editDialog, false);
  // Opened, not merely listed. Duplicating is how you start work as the copy,
  // and leaving it in the sidebar for somebody to find is a step nobody wants.
  await openBot(newName);
});

/// Name what deleting this Bot would take, in the words a person would use.
///
/// Not "are you sure?". That asks somebody to confirm a decision without
/// telling them anything they did not already know. The surprise here is
/// never the Bot; it is the routine that ran every morning, or that the
/// group it coordinates is about to lose its coordinator.
async function deletionCost() {
  const parts = [];
  if (editWas.messages) {
    parts.push(
      `${editWas.messages} message${editWas.messages === 1 ? "" : "s"}`,
    );
  }
  try {
    const routines = await invoke("routines");
    const mine = routines.filter((r) => r.bot === editWas.id);
    if (mine.length) {
      parts.push(
        `${mine.length} routine${mine.length === 1 ? "" : "s"} that will not run again`,
      );
    }
  } catch {
    // A count that cannot be read is left out rather than reported as zero.
    // "0 routines" is a claim; silence is not.
  }
  let inGroups = [];
  try {
    const groups = await invoke("groups");
    inGroups = groups
      .filter((g) => g.members.some((m) => m.id === editWas.id))
      .map((g) => g.name);
  } catch {}

  let what = `Delete ${editWas.name}?`;
  if (parts.length) what += ` This destroys ${parts.join(" and ")}.`;
  if (inGroups.length) {
    what += ` It is in ${inGroups.join(", ")} and will be taken out.`;
  }
  return what + " This cannot be undone.";
}

$("del-start").addEventListener("click", async () => {
  if (!editWas) return;
  $("del-what").textContent = "Working out what that would remove…";
  show($("del-ask"), false);
  show($("del-confirm"), true);
  $("del-what").textContent = await deletionCost();
});

$("del-cancel").addEventListener("click", () => {
  show($("del-confirm"), false);
  show($("del-ask"), true);
});

$("del-go").addEventListener("click", async () => {
  if (!editWas) return;
  try {
    await invoke("bot_delete", { bot: editWas.id });
  } catch (err) {
    $("del-error").textContent = String(err);
    return;
  }
  show(editDialog, false);
  // The conversation on screen belongs to a Bot that no longer exists. Left
  // open it is a header, a transcript and a live composer over nothing, the
  // exact state `closeConversation` exists for.
  closeConversation();
  await refreshRoster();
});

const rulesDialog = $("rules-dialog");
const rulesList = $("rules-list");
const rulesEmpty = $("rules-empty");
const ruleAction = $("rule-action");
const ruleTool = $("rule-tool");
const ruleReason = $("rule-reason");
const ruleError = $("rule-error");

/// Run one settings action and show only what this attempt produced.
///
/// The error line is shared by every control in this panel, so it has to be
/// cleared on success. Otherwise a refused removal goes on being displayed
/// after a later removal works, and an error that outlives the failure it
/// describes is indistinguishable from a live one. Somebody reads it and
/// believes the panel is broken, or worse, believes the rule they just
/// removed is still there.
///
/// One place, so the three controls here cannot each remember separately.
async function settingsAction(run) {
  ruleError.textContent = "";
  try {
    await run();
    return true;
  } catch (err) {
    ruleError.textContent = String(err);
    return false;
  }
}

/// What each action does, in the words a person decides with rather than the
/// enum's. "require_approval" is the wire's name for it, not a label.
const ACTIONS = {
  allow: "runs without asking",
  require_approval: "asks first",
  deny: "refused outright",
};

async function refreshRules() {
  let rules;
  try {
    rules = await invoke("policy_list");
  } catch (err) {
    ruleError.textContent = String(err);
    return;
  }
  rulesList.innerHTML = "";
  rules.forEach((rule, i) => {
    const dt = document.createElement("dt");
    dt.textContent = rule.tool + (rule.when ? ` when ${rule.when.key}=${rule.when.glob}` : "");
    const dd = document.createElement("dd");
    const what = document.createElement("span");
    what.className = rule.action === "allow" ? "rule-allow" : "rule-stop";
    what.textContent = ACTIONS[rule.action] || rule.action;
    dd.appendChild(what);
    if (rule.reason) {
      const why = document.createElement("span");
      why.className = "rule-why";
      why.textContent = " — " + rule.reason;
      dd.appendChild(why);
    }
    const remove = document.createElement("button");
    remove.className = "danger forget";
    remove.textContent = "Remove";
    // By position, which is what the binary addresses. Recomputed on every
    // render, so removing one cannot shift the target of another.
    remove.addEventListener("click", async () => {
      await settingsAction(() => invoke("policy_remove", { number: i + 1 }));
      refreshRules();
    });
    dd.appendChild(remove);
    rulesList.appendChild(dt);
    rulesList.appendChild(dd);
  });
  show(rulesEmpty, rules.length === 0);
}

async function refreshWiring() {
  /// `action` adds a control to the row; connectors have none, because
  /// installing one is a browser sign-in rather than a button.
  const draw = (rows, listEl, emptyEl, term, sub, action) => {
    listEl.innerHTML = "";
    for (const row of rows) {
      const dt = document.createElement("dt");
      dt.textContent = term(row);
      const dd = document.createElement("dd");
      dd.textContent = sub(row);
      if (action) dd.appendChild(action(row));
      listEl.appendChild(dt);
      listEl.appendChild(dd);
    }
    show(emptyEl, rows.length === 0);
  };
  try {
    draw(
      await invoke("connectors"),
      $("connectors-list"),
      $("connectors-empty"),
      (c) => c.id,
      // The credential names it needs, never a value. If it needs none, say
      // so rather than leaving the line blank.
      (c) => (c.secrets.length ? c.secrets.join(", ") : "no credential"),
    );
    draw(
      await invoke("routines"),
      $("routines-list"),
      $("routines-empty"),
      (r) => `${r.bot_name || r.bot} / ${r.id}`,
      // Paused first, because that is the state somebody needs to notice; a
      // paused routine looks identical to a working one otherwise.
      (r) =>
        (r.enabled ? "" : "paused — ") +
        r.trigger +
        (r.next ? `, next ${r.next.replace("T", " ").slice(0, 16)}` : ""),
      // The docs' "pausable". A routine is the run nobody is watching, so
      // this is the control that matters when one starts failing every night
      // or costing more than it is worth. Pausing keeps the definition and
      // the history, so it needs no confirmation: the same button undoes it,
      // which is exactly what deleting a Bot cannot offer.
      (r) => {
        const btn = document.createElement("button");
        btn.className = "forget" + (r.enabled ? "" : " primary");
        btn.textContent = r.enabled ? "Pause" : "Resume";
        btn.addEventListener("click", async () => {
          await settingsAction(() =>
            invoke("routine_pause", {
              bot: r.bot,
              routine: r.id,
              paused: r.enabled,
            }),
          );
          refreshWiring();
        });
        return btn;
      },
    );
  } catch (err) {
    ruleError.textContent = String(err);
  }
}

$("rules-btn").addEventListener("click", () => {
  ruleError.textContent = "";
  ruleTool.value = "";
  ruleReason.value = "";
  show(rulesDialog, true);
  refreshRules();
  refreshWiring();
});
$("rules-close").addEventListener("click", () => show(rulesDialog, false));
$("rule-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  ruleError.textContent = "";
  const reason = ruleReason.value.trim();
  try {
    await invoke("policy_add", {
      rule: {
        action: ruleAction.value,
        tool: ruleTool.value.trim(),
        when: null,
        // Only sent when there is one: the binary requires a reason for
        // anything that stops a call, and an empty string is not one.
        reason: reason || null,
      },
    });
  } catch (err) {
    ruleError.textContent = String(err);
    return;
  }
  ruleTool.value = "";
  ruleReason.value = "";
  refreshRules();
});

const secretsDialog = $("secrets-dialog");
const secretsList = $("secrets-list");
const secretsEmpty = $("secrets-empty");
const secretName = $("secret-name");
const secretValue = $("secret-value");
const secretError = $("secret-error");

async function refreshSecrets() {
  let held;
  try {
    held = await invoke("secret_list");
  } catch (err) {
    secretError.textContent = String(err);
    return;
  }
  secretsList.innerHTML = "";
  for (const entry of held) {
    const dt = document.createElement("dt");
    dt.textContent = entry.name;
    const dd = document.createElement("dd");
    // The fingerprint, because there is nothing else to show and there is
    // no way to get the value back by design.
    dd.textContent = entry.fingerprint;
    const forget = document.createElement("button");
    forget.className = "danger forget";
    forget.textContent = "Forget";
    forget.addEventListener("click", async () => {
      try {
        await invoke("secret_remove", { name: entry.name });
      } catch (err) {
        secretError.textContent = String(err);
      }
      refreshSecrets();
    });
    dd.appendChild(forget);
    secretsList.appendChild(dt);
    secretsList.appendChild(dd);
  }
  show(secretsEmpty, held.length === 0);
}

$("credentials").addEventListener("click", () => {
  secretError.textContent = "";
  secretName.value = "";
  secretValue.value = "";
  show(secretsDialog, true);
  refreshSecrets();
});
$("secrets-close").addEventListener("click", () => {
  // Clear the field on the way out. A password box left populated behind a
  // closed dialog is one that reappears on the next open.
  secretValue.value = "";
  show(secretsDialog, false);
});
$("secret-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  secretError.textContent = "";
  try {
    await invoke("secret_set", {
      name: secretName.value,
      value: secretValue.value,
    });
  } catch (err) {
    // Whatever went wrong, the value is not put back on screen or into the
    // message; the shell's error never carries it either.
    secretError.textContent = String(err);
    return;
  } finally {
    secretValue.value = "";
  }
  secretName.value = "";
  refreshSecrets();
});

$("computer").addEventListener("click", async () => {
  if (!computerPanel.classList.contains("hidden")) return closeComputer();
  computerError.textContent = "";
  show(computerPanel, true);
  try {
    // The address carries a one-time key. It is assigned once, here, rather
    // than kept anywhere the page could leak it into a link or a log.
    computerFrame.src = await invoke("open_computer");
    watchComputer();
  } catch (err) {
    computerError.textContent = String(err);
    computerFrame.removeAttribute("src");
  }
});

/// While the panel is open, notice if the computer stops being served.
///
/// An iframe onto a dead process keeps showing what it last painted, which
/// looks exactly like a computer sitting idle. Polling is the only option
/// here: the frame is another process's window and there is no event to
/// listen for.
let watchingComputer = null;

function watchComputer() {
  clearInterval(watchingComputer);
  watchingComputer = setInterval(async () => {
    let alive;
    try {
      alive = await invoke("computer_alive");
    } catch {
      alive = false;
    }
    if (alive) return;
    clearInterval(watchingComputer);
    watchingComputer = null;
    // Blank it first: a still picture of a computer that is gone is worse
    // than an empty panel, because only one of them is accurate.
    computerFrame.removeAttribute("src");
    computerError.textContent =
      "the computer stopped being served — close this and open it again";
  }, 3000);
}

async function closeComputer() {
  // Blank the frame before stopping the viewer, so the panel never shows a
  // frozen last frame of a computer that is no longer being watched.
  clearInterval(watchingComputer);
  watchingComputer = null;
  computerFrame.removeAttribute("src");
  computerError.textContent = "";
  show(computerPanel, false);
  try {
    await invoke("close_computer");
  } catch (err) {
    setStatus(String(err), "error");
  }
}
$("close-computer").addEventListener("click", closeComputer);

// ------------------------------------------------------------- palette

const palette = $("palette");
const paletteInput = $("palette-input");
const paletteResults = $("palette-results");
const paletteEmpty = $("palette-empty");

/// The palette's empty state. A constant because the failure path below
/// replaces it and every render has to put it back.
const NOTHING_MATCHES = "Nothing matches.";
let paletteItems = [];
let paletteAt = 0;

/// Everything reachable by name. Bots and groups come from the roster the
/// sidebar already has, so the palette can never offer a teammate the sidebar
/// does not: one list, one source.
function paletteEntries(query) {
  const actions = [
    { label: "Settings", run: () => $("rules-btn").click() },
    { label: "Credentials", run: () => $("credentials").click() },
    { label: "Agent Computer", run: () => $("computer").click() },
    { label: "New Bot", run: () => $("new-bot").click() },
    { label: "Show hidden chats", run: () => $("toggle-hidden").click() },
    { label: "Disconnect", run: () => $("disconnect").click() },
  ];
  const bots = [...document.querySelectorAll("#bots .bot")].map((el) => ({
    label: el.querySelector(".bot-name").textContent,
    kind: "Bot",
    run: () => el.click(),
  }));
  const groups = [...document.querySelectorAll("#groups .bot")].map((el) => ({
    label: el.querySelector(".bot-name").textContent,
    kind: "Group",
    run: () => el.click(),
  }));
  const q = query.trim().toLowerCase();
  // Teammates before actions: switching between them is what this is for.
  return [...bots, ...groups, ...actions]
    .filter((e) => !q || e.label.toLowerCase().includes(q))
    .slice(0, 12);
}

function renderPalette() {
  paletteResults.innerHTML = "";
  paletteItems.forEach((entry, i) => {
    const li = document.createElement("li");
    li.className = "palette-item" + (i === paletteAt ? " at" : "");
    const label = document.createElement("span");
    label.textContent = entry.label;
    li.appendChild(label);
    if (entry.kind) {
      const kind = document.createElement("span");
      kind.className = "palette-kind";
      kind.textContent = entry.kind;
      li.appendChild(kind);
    }
    li.addEventListener("click", () => choosePalette(i));
    paletteResults.appendChild(li);
  });
  // Reset every render: a failed search leaves its own sentence here, and the
  // next keystroke has to clear it or one broken search would go on claiming
  // the store is unreadable for the rest of the session.
  paletteEmpty.textContent = NOTHING_MATCHES;
  show(paletteEmpty, paletteItems.length === 0);
}

function openPalette() {
  paletteInput.value = "";
  paletteAt = 0;
  paletteItems = paletteEntries("");
  renderPalette();
  show(palette, true);
  paletteInput.focus();
}

function closePalette() {
  show(palette, false);
}

function choosePalette(i) {
  const entry = paletteItems[i];
  if (!entry) return;
  // Closed before the action runs: several of these open a dialog of their
  // own, and a palette still on top of one is a palette in the way.
  closePalette();
  entry.run();
}

// --------------------------------------------------- mentions and skills
//
// The docs' two composer affordances: `@` names a teammate, a routine or a
// connected app, and `/` names a saved skill. Both exist because the names are
// the interface: a person who has to remember what they called something
// types it wrong, and a Bot that was never mentioned simply does not answer.

const mentions = $("mentions");
const mentionsList = $("mentions-list");
const mentionsNote = $("mentions-note");

/// What `/` can offer, and what it cannot. Read at connect: skills are files
/// somebody edits while the window is open, so this is refreshed rather than
/// read once; see `refreshMentionable`.
let skillCatalog = { skills: [], problems: [] };
/// Routines and connected apps, for `@`. Bots and groups are not cached:
/// they come from the sidebar's own DOM, so this menu can never offer a
/// teammate the sidebar does not show.
let wiring = { routines: [], connectors: [] };

let mentionItems = [];
let mentionAt = 0;
/// Where the trigger character sits in the box, while a menu is open.
let mentionFrom = null;

/// When the lists were last read, so clicking into the box does not spawn
/// three processes every time. Each of these is a `openbot` subprocess, and
/// focusing a text field is something a person does constantly.
let mentionableAt = 0;
const MENTIONABLE_STALE_MS = 3000;

async function refreshMentionable(force = true) {
  const now = Date.now();
  if (!force && now - mentionableAt < MENTIONABLE_STALE_MS) return;
  mentionableAt = now;
  // Independently: a home with a broken connector should still offer skills,
  // rather than one failing list leaving the composer with nothing. Rebuilt
  // field by field rather than assigned. The composer reads these on every
  // keystroke, so an answer of an unexpected shape has to leave it usable; a
  // menu that offers nothing is a smaller failure than a box that throws on
  // the next character typed.
  try {
    const got = await invoke("skills");
    skillCatalog = {
      skills: Array.isArray(got?.skills) ? got.skills : [],
      problems: Array.isArray(got?.problems) ? got.problems : [],
    };
  } catch {
    skillCatalog = { skills: [], problems: [] };
  }
  for (const [key, cmd] of [
    ["routines", "routines"],
    ["connectors", "connectors"],
  ]) {
    try {
      const got = await invoke(cmd);
      wiring[key] = Array.isArray(got) ? got : [];
    } catch {
      wiring[key] = [];
    }
  }
}

/// The token being typed at the caret, if it is a mention.
///
/// A trigger only counts at the start of a word: `3/4` and `a@b` are not
/// somebody reaching for this menu, and popping one open in the middle of a
/// sentence is worse than not having it. Returns `null` otherwise.
function triggerAt(text, caret) {
  const upto = text.slice(0, caret);
  const start = Math.max(upto.lastIndexOf(" "), upto.lastIndexOf("\n")) + 1;
  const token = upto.slice(start);
  if (token.length === 0) return null;
  const char = token[0];
  if (char !== "@" && char !== "/") return null;
  const query = token.slice(1);
  // A space ends it: once the name is chosen the menu has no more to say.
  if (query.includes(" ")) return null;
  return { char, start, query };
}

/// What a trigger offers, filtered by what has been typed after it.
///
/// Each entry carries two strings, and they are not always the same one:
/// `label` is what a person reads, `insert` is what goes in the box. A Bot
/// shows as "Talent Scout" and inserts as `@talent-scout`, because
/// `Group::owner_for` matches an `@mention` against the Bot's id and
/// `openbot_bots::mentions` stops at the first character outside `[a-z0-9_-]`.
/// Inserting the display name would put `@Talent Scout` in the message, which
/// reaches the resolver as `talent` and names nobody: a menu that looks like
/// it addressed a teammate and did not.
function mentionEntries(char, query) {
  const q = query.trim().toLowerCase();
  let all;
  if (char === "/") {
    // Skill names are already slugs: `skill new` lowercases and hyphenates.
    all = skillCatalog.skills.map((s) => ({
      label: s.name,
      insert: s.name,
      what: s.description,
      kind: "Skill",
    }));
  } else {
    const fromSidebar = (sel, kind) =>
      [...document.querySelectorAll(sel)].map((el) => ({
        label: el.querySelector(".bot-name").textContent,
        insert: el.dataset.mention,
        what: "",
        kind,
      }));
    all = [
      ...fromSidebar("#bots .bot", "Bot"),
      ...fromSidebar("#groups .bot", "Group"),
      ...wiring.routines.map((r) => ({
        label: r.id,
        insert: r.id,
        what: r.trigger,
        kind: "Routine",
      })),
      ...wiring.connectors.map((c) => ({
        label: c.id,
        insert: c.id,
        what: c.url,
        kind: "App",
      })),
    ];
  }
  // Matched on both, so typing what is on screen finds it and typing the id
  // does too; the two differ for exactly the Bots this menu exists to name.
  return all
    .filter((e) => e.insert)
    .filter(
      (e) =>
        !q ||
        e.label.toLowerCase().includes(q) ||
        e.insert.toLowerCase().includes(q),
    )
    .slice(0, 8);
}

function renderMentions() {
  mentionsList.innerHTML = "";
  mentionItems.forEach((entry, i) => {
    const li = document.createElement("li");
    li.className = "mentions-item" + (i === mentionAt ? " at" : "");
    li.setAttribute("role", "option");
    li.setAttribute("aria-selected", String(i === mentionAt));
    const name = document.createElement("span");
    name.textContent = entry.label;
    li.appendChild(name);
    if (entry.what) {
      const what = document.createElement("span");
      what.className = "mentions-what";
      what.textContent = entry.what;
      li.appendChild(what);
    }
    const kind = document.createElement("span");
    kind.className = "mentions-kind";
    kind.textContent = entry.kind;
    li.appendChild(kind);
    // `mousedown`, not `click`: the textarea loses focus first otherwise, and
    // the menu closes out from under the pointer.
    li.addEventListener("mousedown", (e) => {
      e.preventDefault();
      chooseMention(i);
    });
    mentionsList.appendChild(li);
  });
}

function closeMentions() {
  mentionFrom = null;
  mentionItems = [];
  show(mentions, false);
}

/// Put the chosen name in the box, replacing what was typed to find it.
function chooseMention(i) {
  const entry = mentionItems[i];
  if (!entry || mentionFrom === null) return;
  const text = input.value;
  const caret = input.selectionStart;
  const char = text[mentionFrom];
  const before = text.slice(0, mentionFrom);
  const after = text.slice(caret);
  const inserted = `${char}${entry.insert} `;
  input.value = before + inserted + after;
  const at = before.length + inserted.length;
  input.setSelectionRange(at, at);
  closeMentions();
  input.focus();
}

/// Open, update or close the menu for whatever is at the caret.
function updateMentions() {
  const found = triggerAt(input.value, input.selectionStart);
  if (!found) return closeMentions();
  mentionItems = mentionEntries(found.char, found.query);
  // A `/` with nothing behind it still has something to say when skills failed
  // to load, so the menu opens on the note alone. Silence there would read as
  // "no skills", which is a different and untrue statement.
  const note = found.char === "/" ? skillProblemNote() : "";
  if (mentionItems.length === 0 && !note) return closeMentions();
  mentionFrom = found.start;
  mentionAt = 0;
  renderMentions();
  mentionsNote.textContent = note;
  show(mentionsNote, Boolean(note));
  show(mentions, true);
}

/// What is on disk and being ignored.
///
/// A skill that stopped parsing is invisible everywhere else: the file is
/// there, `skill new` said it was created, and the Bot has quietly not been
/// following it. The menu is where somebody looks for it, so the menu is where
/// it has to be said.
function skillProblemNote() {
  const n = skillCatalog.problems.length;
  if (n === 0) return "";
  const which = n === 1 ? "1 skill" : `${n} skills`;
  return `${which} could not be loaded, so no Bot can use ${
    n === 1 ? "it" : "them"
  } — run \`openbot skill ls\` to see why.`;
}

/// How long typing has to stop before the message search goes out.
///
/// `search` shells out to `openbot search`, which reads every conversation in
/// the home (on the order of half a second over 50 Bots and 100k messages, in
/// release). Searching on every keystroke would make typing `renewal` seven
/// processes and seven scans of the same home to answer one question, with
/// only the last one's results ever shown.
///
/// Short enough to feel immediate (names still filter on the keystroke,
/// because those are already in the page) and long enough that a word typed
/// at any normal speed costs one scan instead of one per letter.
const SEARCH_SETTLE_MS = 150;
let searchTimer = null;

paletteInput.addEventListener("input", () => {
  const q = paletteInput.value;
  paletteItems = paletteEntries(q);
  paletteAt = 0;
  renderPalette();
  // Messages come from the binary, so they arrive after the names do. They are
  // appended rather than replacing, and only if the box still says what it
  // said when the search left; otherwise a slow answer to an old query lands
  // on top of a new one. Kept alongside the debounce rather than replaced by
  // it: waiting for a pause makes the race rarer, and a slow scan can still be
  // overtaken by a fast one.
  if (searchTimer) clearTimeout(searchTimer);
  if (!q.trim()) return;
  searchTimer = setTimeout(() => searchMessages(q), SEARCH_SETTLE_MS);
});

function searchMessages(q) {
  invoke("search", { query: q })
    .then((hits) => {
      if (paletteInput.value !== q) return;
      for (const hit of hits.slice(0, 8)) {
        paletteItems.push({
          label: hit.text,
          kind: hit.kind === "group" ? "In group" : "Said",
          run: () =>
            hit.kind === "group" ? openGroup(hit.name) : openBot(hit.name),
        });
      }
      renderPalette();
    })
    .catch((err) => {
      // "Nothing matches" is an answer, and a failed search has not given
      // one. Names are matched in the page and are already on screen; only
      // the message hits come from the binary, so a search that fails leaves
      // the empty state saying the conversation does not exist. Somebody then
      // stops looking for something that is there.
      if (paletteInput.value !== q) return;
      paletteEmpty.textContent = `Could not search conversations — ${err}`;
      show(paletteEmpty, true);
    });
}

paletteInput.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const step = e.key === "ArrowDown" ? 1 : -1;
    // Wraps, so holding one arrow cannot strand the selection at an end.
    paletteAt = (paletteAt + step + paletteItems.length) % (paletteItems.length || 1);
    renderPalette();
  } else if (e.key === "Enter") {
    e.preventDefault();
    choosePalette(paletteAt);
  }
});

document.addEventListener("keydown", (e) => {
  // The docs' Cmd/Ctrl+N. Only with a workspace on screen: before connecting
  // there is nowhere to put a Bot, and the browser's own New Window is a
  // better thing for the key to do than an error.
  if (
    (e.ctrlKey || e.metaKey) &&
    e.key.toLowerCase() === "n" &&
    !workspace.classList.contains("hidden")
  ) {
    e.preventDefault();
    $("new-bot").click();
    return;
  }
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
    e.preventDefault();
    if (palette.classList.contains("hidden")) openPalette();
    else closePalette();
    return;
  }
  // Escape closes whatever is on top, except an approval, which is a
  // question that has to be answered rather than dismissed. Escaping it
  // would leave the Bot waiting with nothing on screen to say so.
  if (e.key === "Escape") {
    if (!palette.classList.contains("hidden")) return closePalette();
    if (!nameDialog.classList.contains("hidden")) return show(nameDialog, false);
    if (!editDialog.classList.contains("hidden")) return show(editDialog, false);
    if (!secretsDialog.classList.contains("hidden")) return show(secretsDialog, false);
    if (!rulesDialog.classList.contains("hidden")) return show(rulesDialog, false);
  }
});

composer.addEventListener("submit", (e) => {
  e.preventDefault();
  const text = input.value.trim();
  // `busy` does not block: a message sent while the Bot is working joins
  // the turn rather than starting a second one, which is what the docs mean by
  // redirecting work in progress.
  if (!text || !session) return;
  input.value = "";
  sendPrompt(text);
});

// Enter sends, Shift+Enter breaks the line. A chat box where Enter inserts a
// newline is a chat box people send half-written messages from, hunting for
// the button.
input.addEventListener("keydown", (e) => {
  // While the menu is up it owns the keys that move and choose. Enter must not
  // reach the form: a person picking a name from a list has not finished
  // writing, and sending the half-typed message is unrecoverable.
  if (!mentions.classList.contains("hidden") && mentionItems.length > 0) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const step = e.key === "ArrowDown" ? 1 : -1;
      mentionAt = (mentionAt + step + mentionItems.length) % mentionItems.length;
      return renderMentions();
    }
    if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      return chooseMention(mentionAt);
    }
    if (e.key === "Escape") {
      // Stopped here so the document-level handler does not also read it and
      // close a dialog underneath.
      e.preventDefault();
      e.stopPropagation();
      return closeMentions();
    }
  }
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    composer.requestSubmit();
  }
});

// Typing, and moving the caret with a click or an arrow: a menu that only
// tracked keystrokes would stay open over a word the caret has left.
input.addEventListener("input", updateMentions);
input.addEventListener("click", updateMentions);
input.addEventListener("keyup", (e) => {
  if (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End") {
    updateMentions();
  }
});
input.addEventListener("blur", closeMentions);
// A skill is content, not code: somebody writes one while the window is open
// and expects the next message to be able to name it. `openbotd` reloads them
// per task for the same reason. Clicking into the box is the moment before
// they type `/`, which makes it the cheapest place to be current. Throttled,
// because a click is not a rare event and each refresh is three subprocesses.
input.addEventListener("focus", () => refreshMentionable(false));

cancelBtn.addEventListener("click", async () => {
  try {
    forgetAsks(await invoke("cancel", { session }));
  } catch (err) {
    setStatus(String(err), "error");
  }
});

// Where the runtime is. An installed OPENBOT ships it beside itself, and that
// build is the one this client was tested against; running from source falls
// back to the bare name and PATH. Only fills the field, never overwrites what
// somebody typed.
invoke("default_openbot")
  .then((path) => {
    const field = $("openbot-path");
    if (path && !field.value.trim()) setPath(field, path);
  })
  .catch(() => {});

// The same for where the Bots go. Filled with a real path rather than left to
// a fallback: a default of the literal string `~/.openbot` would not be
// expanded on the way to a subprocess, so openbot would make a folder called
// `~` beside wherever the app was launched and put every Bot in it.
invoke("default_home")
  .then((path) => {
    const field = $("home-path");
    if (path && !field.value.trim()) setPath(field, path);
  })
  .catch(() => {});

// A reload must not offer to connect an engine that is already running: the
// page is transient, the shell's state is not.
invoke("connected")
  .then((on) => {
    if (on) return enterWorkspace();
    show(connectPanel, true);
    show(workspace, false);
  })
  .catch(() => {});
