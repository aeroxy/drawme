// The room: signing in, a nickname, finding someone to draw with (Match) or
// letting a friend in (Invite), the direct connection, voice, and the other
// person's pointer. It takes the place of jspaint's sessions.js.
//
// The server is only asked while waiting and while connecting. Once the
// connection is up, drawing and talking go straight between the browsers.
/* global $, $Window, $status_area, $status_text, $canvas_area, main_canvas, main_ctx, current_history_node, magnification */

import { $DialogWindow } from "../src/$ToolWindow.js";
import { reset_canvas_and_history, show_error_message } from "../src/functions.js";
import { $G, debounce, to_canvas_coords } from "../src/helpers.js";
import { showMessageBox } from "../src/msgbox.js";
import * as store from "./idb.js";
import { Connection } from "./rtc.js";
import { room } from "./state.js";
import { CanvasSync } from "./sync.js";

/** How often to ask for the inbox: gently while waiting, briskly while connecting. */
const POLL_WAITING_MS = 2000;
const POLL_CONNECTING_MS = 500;
/** How long connecting may take before the other side counts as gone. */
const HANDSHAKE_MS = 25000;
const NICKNAME_MAX = 24;

// jspaint starts a new local session for File > New and File > Open. Here
// they stay in the room, and the new picture goes to the other person.
window.new_local_session = () => {};

/**
 * A place in a room, from the moment the server gives it until leaving.
 * @typedef {{
 *   peer: string, room: string, role: "host" | "guest", mode: "match" | "invite",
 *   connection: Connection | null, sync: CanvasSync | null,
 *   connected: boolean, connecting: boolean, knocking: boolean, leaving: boolean, reopening: boolean,
 *   polling: boolean, partner: string | null, partnerMic: boolean, notice: string | null, timer?: number,
 * }} Seat
 */
/** @type {Seat | null} */
let seat = null;
let nickname = "";
/** @type {RTCIceServer[] | null} */
let iceServers = null;
/** @type {MediaStream | null} */
let micStream = null;
/** Set when the browser held back the other person's sound until the page is touched. */
let needsTap = false;
let signInShown = false;

// jspaint's own status text, "For Help, click Help Topics on the Help Menu.",
// goes: there is no Help menu. jspaint keeps writing to it, detached, which
// does nothing. The room's field takes its place, and its width.
$status_text.remove();

// The status bar gets a field of its own: what the room is doing, and the mic.
const $field = $(document.createElement("div")).addClass("status-field inset-shallow drawme-status").prependTo($status_area);
const $text = $(document.createElement("span")).addClass("drawme-status-text").appendTo($field);
const $mic = $(document.createElement("button"))
  .attr({ type: "button", title: "Microphone" })
  .addClass("drawme-mic")
  .appendTo($field)
  .on("click", () => toggleMic());

// The other person's pointer, with their nickname beside it.
const pointer = document.createElement("div");
pointer.className = "drawme-cursor";
pointer.hidden = true;
const arrow = Object.assign(new Image(), { src: "images/cursors/default.png", alt: "" });
const tag = document.createElement("span");
pointer.append(arrow, tag);
document.body.append(pointer);

// Their voice.
const speaker = document.createElement("audio");
speaker.autoplay = true;
speaker.setAttribute("playsinline", "");
document.body.append(speaker);

Object.assign(room.actions, {
  toggleMic,
  showInviteLink,
  findPartner,
  inviteFriend,
  leave: () => {
    leave();
    setAddress("");
  },
  changeNickname,
  signOut,
});

start();

async function start() {
  renderStatus();
  await restoreDrawing();
  try {
    await api("GET", "/api/me");
  } catch {
    return; // Not signed in: the sign-in screen is up.
  }
  room.signedIn = true;
  renderStatus();
  nickname = (await store.get("nickname")) || (await askNickname({ required: true })) || "";
  renderStatus();
  const invite = inviteInAddress();
  if (invite) knock(invite);
  else chooseMode();
}

// ——— The server ————————————————————————————————————————————————

async function api(method, path, body) {
  const response = await fetch(path, {
    method,
    headers: body ? { "Content-Type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await response.json().catch(() => ({}));
  if (response.status === 401) signedOut();
  if (!response.ok) {
    throw Object.assign(new Error(data.error || `drawme-1998 answered ${response.status}`), { status: response.status });
  }
  return data;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The invite in the address bar (`#invite:<room>`), if any. */
function inviteInAddress() {
  return location.hash.match(/(?:^#|,)invite:([0-9a-f]{32})(?=,|$)/)?.[1] ?? null;
}

function setAddress(hash) {
  history.replaceState(null, "", `${location.pathname}${location.search}${hash}`);
}

// ——— Signing in and the nickname ———————————————————————————————————

/** A cover over the app, below the next window, for questions that come first. */
function backdrop() {
  const cover = document.createElement("div");
  cover.className = "drawme-backdrop";
  cover.style.zIndex = String($Window.Z_INDEX++);
  document.body.append(cover);
  return cover;
}

function paragraph(...parts) {
  const p = document.createElement("p");
  p.append(...parts);
  return p;
}

function bold(text) {
  return Object.assign(document.createElement("b"), { textContent: text });
}

function signedOut() {
  const s = seat;
  if (s) drop(s);
  room.signedIn = false;
  renderStatus();
  if (signInShown) return;
  signInShown = true;
  backdrop();
  const $w = $DialogWindow("Welcome to drawme-1998");
  $w.addClass("drawme-dialog horizontal-buttons");
  $w.$main.append(
    paragraph("Draw together in a classic paint program, two at a time, and talk while you draw."),
    paragraph("Sign in with Aero to start. The other person never sees your email unless you knock on their room."),
  );
  $w.$Button(
    "Sign in with Aero",
    () => {
      const invite = inviteInAddress();
      location.assign(`/api/sign-in?return=${encodeURIComponent(invite ? `#invite:${invite}` : "")}`);
    },
    { type: "submit" },
  );
  $w.on("close", (event) => event.preventDefault());
  $w.center();
}

/** Asks for a nickname. Resolves with it, or null when cancelled. */
function askNickname({ required = false } = {}) {
  return new Promise((resolve) => {
    const cover = required ? backdrop() : null;
    const $w = $DialogWindow("Your Nickname");
    $w.addClass("drawme-dialog horizontal-buttons");
    const input = Object.assign(document.createElement("input"), {
      type: "text",
      maxLength: NICKNAME_MAX,
      value: nickname,
      autocomplete: "nickname",
      className: "inset-deep",
    });
    const label = Object.assign(document.createElement("label"), { className: "drawme-field" });
    label.append("Nickname: ", input);
    $w.$main.append(
      paragraph("Pick a name for the other person to see. It stays in this browser, and you can change it from the Room menu."),
      label,
    );
    let done = false;
    const finish = (value) => {
      done = true;
      cover?.remove();
      $w.close();
      resolve(value);
    };
    $w.$Button(
      "OK",
      () => {
        const value = input.value.trim();
        if (!value || [...value].length > NICKNAME_MAX) return input.focus();
        store.set("nickname", value);
        finish(value);
      },
      { type: "submit" },
    );
    if (!required) $w.$Button("Cancel", () => finish(null));
    $w.on("close", (event) => {
      if (done) return;
      if (required) return event.preventDefault();
      done = true;
      resolve(null);
    });
    $w.center();
    input.focus();
    input.select();
  });
}

async function changeNickname() {
  const value = await askNickname();
  if (!value) return;
  nickname = value;
  seat?.connection?.send(JSON.stringify({ type: "hello", nickname, mic: room.micOn }));
  renderStatus();
}

async function signOut() {
  await leave();
  setAddress("");
  await fetch("/api/sign-out", { method: "POST" }).catch(() => {});
  signedOut();
}

// ——— Choosing a room ——————————————————————————————————————————————

/** The "who would you like to draw with?" window, while it's open. */
let $chooser = null;

function chooseMode() {
  if (!room.signedIn || $chooser) return;
  const cover = backdrop();
  const $w = $DialogWindow("drawme-1998");
  $w.addClass("drawme-dialog horizontal-buttons");
  $w.$main.append(
    paragraph("Hi, ", bold(nickname), ". Who would you like to draw with?"),
    paragraph("Match pairs you with whoever is waiting, or you wait for the next person. Invite gives you a link to send, and you choose who comes in."),
  );
  const choose = (action) => () => {
    $w.close();
    action();
  };
  $w.$Button("Match me with someone", choose(findPartner), { type: "submit" });
  $w.$Button("Invite a friend", choose(inviteFriend));
  $w.on("closed", () => {
    cover.remove();
    if ($chooser === $w) $chooser = null;
  });
  $chooser = $w;
  $w.center();
}

function closeChooser() {
  const $w = $chooser;
  $chooser = null;
  $w?.close();
}

async function findPartner() {
  await leave();
  try {
    enter({ ...(await api("POST", "/api/match")), mode: "match" });
  } catch (error) {
    if (error.status !== 401) show_error_message("Couldn't find a room.", error);
  }
}

async function inviteFriend() {
  await leave();
  try {
    enter({ ...(await api("POST", "/api/invite")), mode: "invite" });
    showInviteLink();
  } catch (error) {
    if (error.status !== 401) show_error_message("Couldn't open a room.", error);
  }
}

/** Asks to join an invite room. */
async function knock(id) {
  await leave();
  let answer;
  try {
    answer = await api("POST", "/api/knock", { room: id, nickname });
  } catch (error) {
    if (error.status === 401) return;
    setAddress("");
    await showMessageBox({
      title: "drawme-1998",
      message: error.status === 409 ? "That room already has two people in it." : "That invite has ended. Ask for a new link.",
      iconID: "info",
    });
    return chooseMode();
  }
  knocked(id, answer);
}

/** Takes the seat a knock got: a guest's, or the host's own room back after a reload. */
function knocked(id, { peer, status }) {
  if (status === "host") return enter({ peer, room: id, role: "host", mode: "invite" });
  enter({ peer, room: id, role: "guest", mode: "invite" }, { knocking: status === "waiting" });
}

/**
 * Takes a seat the server gave. A guest starts the connection at once,
 * unless they're still waiting to be let in.
 */
function enter({ peer, room: id, role, mode }, { knocking = false } = {}) {
  /** @type {Seat} */
  const s = {
    peer,
    room: id,
    role,
    mode,
    connection: null,
    sync: null,
    connected: false,
    connecting: false,
    knocking,
    leaving: false,
    reopening: false,
    polling: false,
    partner: null,
    partnerMic: false,
    notice: null,
  };
  seat = s;
  closeChooser();
  room.mode = mode;
  room.inviteLink = mode === "invite" ? `${location.origin}/#invite:${id}` : null;
  setAddress(mode === "invite" ? `#invite:${id}` : "");
  if (knocking) showWaiting(s);
  else if (role === "guest") connectAsGuest(s);
  renderStatus();
  poll(s);
}

// ——— Waiting: the inbox ———————————————————————————————————————————

async function poll(s) {
  if (s.polling) return;
  s.polling = true;
  try {
    while (seat === s && !s.connected) {
      let messages = [];
      try {
        ({ messages } = await api("GET", `/api/inbox?peer=${s.peer}`));
      } catch (error) {
        if (seat !== s || error.status === 401) return;
        if (error.status === 410) return void lost(s);
        await sleep(5000);
        continue;
      }
      for (const message of messages) {
        if (seat !== s) return;
        await hear(s, message);
      }
      if (seat !== s || s.connected) return;
      await sleep(s.connecting ? POLL_CONNECTING_MS : POLL_WAITING_MS);
    }
  } finally {
    s.polling = false;
  }
}

async function hear(s, message) {
  switch (message?.type) {
    case "joined": // Someone took the other seat; their offer is on its way.
      connecting(s);
      break;
    case "moved": // Brought into a room whose last partner left.
      s.role = "guest";
      await connectAsGuest(s);
      break;
    case "admitted":
      s.knocking = false;
      closeWaiting();
      await connectAsGuest(s);
      break;
    case "denied": {
      await leave();
      setAddress("");
      const reason = { full: "Someone else got in first, so the room is full.", ended: "That room has closed." };
      await showMessageBox({ title: "drawme-1998", message: reason[message.reason] ?? "The host didn't let you in.", iconID: "info" });
      chooseMode();
      break;
    }
    case "knock":
      askHost(s, message); // Not awaited: more than one can wait for an answer.
      break;
    case "knock-cancelled":
      dismissKnock(message.knock);
      break;
    case "signal":
      await signal(s, message.data);
      break;
    case "left":
      await partnerGone(s);
      break;
  }
  renderStatus();
}

/** The server no longer knows this seat: start again, back in the same room if possible. */
async function lost(s) {
  if (seat !== s) return;
  const { mode, room: id, role } = s;
  drop(s);
  if (mode === "match") return findPartner();
  // Everyone let into an invite room walks back in, the host included.
  try {
    return knocked(id, await api("POST", "/api/knock", { room: id, nickname }));
  } catch (error) {
    if (error.status === 401) return;
  }
  if (role === "host") return inviteFriend();
  setAddress("");
  await showMessageBox({ title: "drawme-1998", message: "That room has closed.", iconID: "info" });
  chooseMode();
}

// ——— Connecting ——————————————————————————————————————————————————

/** Marks a handshake as under way, and gives up on it after a while. */
function connecting(s) {
  s.connecting = true;
  clearTimeout(s.timer);
  s.timer = setTimeout(() => {
    if (seat !== s || s.connected) return;
    s.notice = "Couldn't connect to them.";
    partnerGone(s);
  }, HANDSHAKE_MS);
  renderStatus();
}

async function connection(s) {
  const old = s.connection;
  s.connection = null;
  old?.close();
  iceServers ??= await api("GET", "/api/ice")
    .then((answer) => answer.iceServers)
    .catch(() => [{ urls: "stun:stun.cloudflare.com:3478" }]);
  const made = new Connection(iceServers);
  s.connection = made;
  made.addEventListener("open", () => opened(s, made));
  made.addEventListener("close", () => {
    if (s.connection === made) partnerGone(s);
  });
  made.addEventListener("message", (event) => fromPartner(s, /** @type {CustomEvent} */ (event).detail));
  made.addEventListener("cursor", (event) => showCursor(s, /** @type {CustomEvent} */ (event).detail));
  made.addEventListener("track", (event) => listen(/** @type {CustomEvent} */ (event).detail));
  return made;
}

/** The guest starts: an offer to the host, whose answer comes to the inbox. */
async function connectAsGuest(s) {
  s.role = "guest";
  connecting(s);
  const made = await connection(s);
  const offer = await made.offer();
  await made.setMic(micTrack()).catch(() => {});
  if (seat !== s || s.connection !== made) return;
  try {
    await api("POST", "/api/signal", { peer: s.peer, data: offer });
  } catch (error) {
    if (error.status === 410) return void lost(s);
    if (error.status === 409) return void partnerGone(s);
  }
  poll(s);
}

async function signal(s, data) {
  if (data?.type === "offer" && s.role === "host") {
    connecting(s);
    const made = await connection(s);
    const answer = await made.answer(data);
    await made.setMic(micTrack()).catch(() => {});
    if (seat !== s || s.connection !== made) return;
    await api("POST", "/api/signal", { peer: s.peer, data: answer }).catch((error) => {
      if (error.status === 409) partnerGone(s);
    });
  } else if (data?.type === "answer" && s.role === "guest" && s.connection && !s.connection.closed) {
    await s.connection.accept(data).catch((error) => console.error("drawme: the answer didn't take", error));
  }
}

function opened(s, made) {
  if (seat !== s || s.connection !== made) return;
  clearTimeout(s.timer);
  s.connected = true;
  s.connecting = false;
  s.notice = null;
  room.connected = true;
  s.sync = new CanvasSync(made, { host: s.role === "host", onChange: saveSoon });
  s.sync.start();
  made.send(JSON.stringify({ type: "hello", nickname, mic: room.micOn }));
  closeKnocks();
  renderStatus();
}

function fromPartner(s, data) {
  if (seat !== s) return;
  if (typeof data !== "string") return s.sync?.receive(data);
  let message;
  try {
    message = JSON.parse(data);
  } catch {
    return;
  }
  if (message.type === "hello") {
    s.partner = String(message.nickname ?? "").trim().slice(0, 48) || "Partner";
    s.partnerMic = Boolean(message.mic);
  } else if (message.type === "mic") {
    s.partnerMic = Boolean(message.on);
  } else if (message.type === "bye") {
    return void partnerGone(s);
  }
  renderStatus();
}

/**
 * The other person left, or the connection broke: keep the room, as its
 * host, and wait for the next person (or let in whoever is already waiting).
 */
async function partnerGone(s) {
  if (seat !== s || s.leaving || s.reopening) return;
  s.reopening = true;
  if (s.connected && s.partner) s.notice = `${s.partner} left.`;
  clearTimeout(s.timer);
  const old = s.connection;
  s.connection = null;
  old?.close();
  s.sync?.stop();
  s.sync = null;
  s.connected = false;
  s.connecting = false;
  s.partner = null;
  s.partnerMic = false;
  room.connected = false;
  pointer.hidden = true;
  renderStatus();
  try {
    const { status } = await api("POST", "/api/reopen", { peer: s.peer });
    s.role = "host";
    if (status === "paired") connecting(s);
  } catch (error) {
    s.reopening = false;
    if (seat !== s || error.status === 401) return;
    if (error.status === 410) return void lost(s);
    await sleep(3000);
    return void partnerGone(s);
  }
  s.reopening = false;
  renderStatus();
  poll(s);
}

/** Forgets the seat here, without telling the server. */
function drop(s) {
  s.leaving = true;
  clearTimeout(s.timer);
  try {
    s.connection?.send(JSON.stringify({ type: "bye" }));
  } catch {
    // Already closed.
  }
  const old = s.connection;
  s.connection = null;
  old?.close();
  s.sync?.stop();
  if (seat === s) seat = null;
  room.mode = null;
  room.connected = false;
  room.inviteLink = null;
  pointer.hidden = true;
  closeWaiting();
  closeKnocks();
  renderStatus();
}

/**
 * Leaves the room. The picture stays. A closing page (`beacon`) asks the
 * server to keep an invite room whose host is alone, so the link they sent
 * still works when they come back.
 */
async function leave({ beacon = false } = {}) {
  const s = seat;
  if (!s) return;
  drop(s);
  const body = JSON.stringify({ peer: s.peer, keep: beacon });
  if (beacon) {
    navigator.sendBeacon("/api/leave", new Blob([body], { type: "application/json" }));
  } else {
    await fetch("/api/leave", { method: "POST", headers: { "Content-Type": "application/json" }, body, keepalive: true }).catch(() => {});
  }
}

addEventListener("pagehide", () => leave({ beacon: true }));
// An invite link opened in a page that's already here changes only the fragment.
addEventListener("hashchange", () => {
  const invite = inviteInAddress();
  if (invite && room.signedIn && seat?.room !== invite) knock(invite);
});
// A page brought back from the back-forward cache left its room when hidden.
addEventListener("pageshow", (event) => {
  if (event.persisted) location.reload();
});

// ——— Invites and knocks ————————————————————————————————————————————

function showInviteLink() {
  const link = room.inviteLink;
  if (!link) return;
  const $w = $DialogWindow("Invite a Friend");
  $w.addClass("drawme-dialog horizontal-buttons");
  const input = Object.assign(document.createElement("input"), { type: "text", readOnly: true, value: link, className: "inset-deep drawme-link" });
  input.addEventListener("focus", () => input.select());
  $w.$main.append(
    paragraph("Send this link to a friend. When they open it, you'll see their nickname and email, and choose whether to let them in."),
    input,
  );
  const $copy = $w.$Button("Copy", async () => {
    try {
      await navigator.clipboard.writeText(link);
      $copy.text("Copied");
    } catch {
      input.focus();
    }
  });
  if (navigator.share) {
    $w.$Button("Share...", () => navigator.share({ title: "Draw with me", url: link }).catch(() => {}));
  }
  $w.$Button("OK", () => $w.close(), { type: "submit" });
  $w.center();
}

/** @type {Map<string, any>} open "wants to join" windows, by knock */
const knockWindows = new Map();

async function askHost(s, { knock, email, nickname: name }) {
  const escape = (text) => String(text ?? "").replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  const prompt = showMessageBox({
    title: "Someone Wants to Join",
    messageHTML: `<p><b>${escape(name)}</b> wants to draw with you.</p><p>Signed in with Aero as ${escape(email)}</p>`,
    buttons: [
      { label: "Let them in", value: "admit", default: true },
      { label: "Deny", value: "deny" },
    ],
    iconID: "info",
  });
  knockWindows.set(knock, prompt.$window);
  const answer = await prompt;
  // Gone from the map: closed here because they gave up or someone else got in.
  if (!knockWindows.delete(knock) || seat !== s) return;
  try {
    const { result } = await api("POST", "/api/admit", { peer: s.peer, knock, admit: answer === "admit" });
    if (result === "admitted") {
      closeKnocks();
      connecting(s);
    }
  } catch {
    // They gave up while being asked.
  }
  renderStatus();
}

function dismissKnock(knock) {
  const $w = knockWindows.get(knock);
  knockWindows.delete(knock);
  $w?.close();
}

function closeKnocks() {
  for (const knock of [...knockWindows.keys()]) dismissKnock(knock);
}

let $waiting = null;

function showWaiting(s) {
  closeWaiting();
  const $w = $DialogWindow("Asking to Join");
  $w.addClass("drawme-dialog horizontal-buttons");
  $w.$main.append(paragraph("Waiting for the host to let you in…"));
  // A knock waits for a host who has stepped away; say so after a while.
  const away = setTimeout(() => {
    if (seat === s && s.knocking) $w.$main.append(paragraph("The host may be away from drawme-1998 right now. You can keep waiting."));
  }, 60000);
  $w.$Button("Cancel", async () => {
    await leave();
    setAddress("");
    chooseMode();
  });
  $w.on("closed", () => {
    clearTimeout(away);
    if ($waiting === $w) $waiting = null;
  });
  $waiting = $w;
  $w.center();
}

function closeWaiting() {
  const $w = $waiting;
  $waiting = null;
  $w?.close();
}

// ——— Voice ——————————————————————————————————————————————————————

function micTrack() {
  return micStream?.getAudioTracks()[0] ?? null;
}

async function toggleMic() {
  if (room.micOn) {
    micStream?.getTracks().forEach((track) => track.stop());
    micStream = null;
    room.micOn = false;
  } else {
    try {
      micStream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch {
      showMessageBox({
        title: "Microphone",
        message: "drawme-1998 couldn't use your microphone. Check that this site is allowed to use it.",
        iconID: "warning",
      });
      return;
    }
    room.micOn = true;
  }
  await seat?.connection?.setMic(micTrack()).catch(() => {});
  seat?.connection?.send(JSON.stringify({ type: "mic", on: room.micOn }));
  renderStatus();
}

/** @param {MediaStream} stream the other person's sound */
function listen(stream) {
  speaker.srcObject = stream;
  playVoice();
}

function playVoice() {
  speaker.play().then(
    () => {
      needsTap = false;
      renderStatus();
    },
    () => {
      needsTap = true;
      renderStatus();
    },
  );
}

// Browsers hold back sound until the page has been touched; any touch will do.
addEventListener("pointerdown", () => needsTap && speaker.srcObject && playVoice(), true);

// ——— Pointers ————————————————————————————————————————————————————

/** The pointer goes out at most every CURSOR_MS, and its last position always does. */
const CURSOR_MS = 40;
let lastCursorAt = 0;
let pendingCursor = null;
let cursorTimer = 0;

function sendCursor() {
  cursorTimer = 0;
  const made = seat?.connected ? seat.connection : null;
  if (made && pendingCursor) made.sendCursor(pendingCursor);
  pendingCursor = null;
  lastCursorAt = performance.now();
}

$G.on("pointermove.drawme", (event) => {
  if (!seat?.connected) return;
  const { x, y } = to_canvas_coords(event);
  pendingCursor = { x, y };
  const wait = CURSOR_MS - (performance.now() - lastCursorAt);
  if (wait <= 0) sendCursor();
  else cursorTimer ||= setTimeout(sendCursor, wait);
});
$G.on("blur.drawme", () => seat?.connection?.sendCursor({ away: true }));

function showCursor(s, position) {
  if (seat !== s || !position || position.away || !Number.isFinite(position.x) || !Number.isFinite(position.y)) {
    pointer.hidden = true;
    return;
  }
  const rect = window.canvas_bounding_client_rect;
  pointer.style.transform = `translate(${rect.left + magnification * position.x}px, ${rect.top + magnification * position.y}px)`;
  tag.textContent = s.partner ?? "";
  pointer.hidden = false;
}

// ——— The picture, kept in this browser ————————————————————————————

const saveSoon = debounce(() => {
  main_canvas.toBlob((blob) => blob && store.set("drawing", blob), "image/png");
}, 1500);
$G.on("session-update.drawme-save", saveSoon);

async function restoreDrawing() {
  const blob = await store.get("drawing");
  if (!(blob instanceof Blob)) return;
  try {
    const bitmap = await createImageBitmap(blob);
    reset_canvas_and_history();
    main_ctx.copy(bitmap);
    current_history_node.image_data = main_ctx.getImageData(0, 0, main_canvas.width, main_canvas.height);
    $canvas_area.trigger("resize");
  } catch {
    // An unreadable save: start blank.
  }
}

// ——— Status ——————————————————————————————————————————————————————

function renderStatus() {
  const s = seat;
  let text;
  if (!room.signedIn) text = "Not signed in";
  else if (!s) text = "Drawing alone. Room menu: Match or Invite.";
  else if (s.connected) text = `Drawing with ${s.partner ?? "someone"}${s.partnerMic ? " 🔊" : ""}`;
  else if (s.knocking) text = "Asking the host to let you in…";
  else if (s.connecting) text = "Connecting…";
  else if (s.mode === "invite") text = "Waiting for your friend to knock…";
  else text = "Waiting for someone to join…";
  if (s?.notice && !s.connected) text = `${s.notice} ${text}`;
  if (needsTap && s?.connected) text += " Click to hear them.";
  $text.text(text).attr("title", text);
  $mic.text(room.micOn ? "🎙 Mic on" : "🎙 Mic off").attr("aria-pressed", String(room.micOn)).prop("disabled", !room.signedIn);
}
