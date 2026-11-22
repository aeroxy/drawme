// Adjusts jspaint's menus before app.js builds the menu bar from them. Drops
// what a room makes meaningless: jspaint's own multi-user sessions, and its
// manager for drawings kept in localStorage (drawme keeps the drawing in
// IndexedDB instead), its Help menu, and its About window. Adds a Room menu
// before Extras.
/* global localize, MENU_DIVIDER */

import { menus } from "../src/menus.js";
import { room } from "./state.js";

/** Removes the item labelled `label`, with the divider just before it. */
function drop(menu, label) {
  const items = menus[localize(menu)];
  const at = items?.findIndex((item) => item !== MENU_DIVIDER && item.label === localize(label)) ?? -1;
  if (at < 0) return;
  const from = at > 0 && items[at - 1] === MENU_DIVIDER ? at - 1 : at;
  items.splice(from, at - from + 1);
}

drop("&File", "Manage Storage");
drop("E&xtras", "Manage Storage");
drop("E&xtras", "&Multi-User");
// The Help menu held only Help Topics and About Paint. F1 opens the help
// window from a handler in app.js, so stop it here, in the capture phase,
// before that handler runs.
delete menus[localize("&Help")];
addEventListener(
  "keydown",
  (event) => {
    if (event.key !== "F1") return;
    event.preventDefault();
    event.stopImmediatePropagation();
  },
  true,
);
// jspaint's own project links (its news, Discord, GitHub and donation page)
// give way to drawme's.
for (const label of ["Project News", "Discord", "GitHub", "Donate"]) drop("E&xtras", label);
menus[localize("E&xtras")].push(
  MENU_DIVIDER,
  {
    emoji_icon: "ℹ️",
    label: "GitHub",
    action: () => { window.open("https://github.com/aeroxy/drawme"); },
    description: "Shows the project on GitHub.",
  },
  {
    emoji_icon: "💵",
    label: "Donate",
    action: () => { window.open("https://aero-sg.us.to/"); },
    description: "Supports the project.",
  },
);

const roomMenu = [
  {
    label: "&Microphone",
    checkbox: {
      check: () => room.micOn,
      toggle: () => room.actions.toggleMic?.(),
    },
    enabled: () => room.signedIn,
    description: "Lets the other person hear you.",
  },
  {
    label: "&Invite Link...",
    action: () => room.actions.showInviteLink?.(),
    enabled: () => Boolean(room.inviteLink),
    description: "Shows the link that asks to join this room.",
  },
  MENU_DIVIDER,
  {
    label: "&Match Me With Someone",
    action: () => room.actions.findPartner?.(),
    enabled: () => room.signedIn,
    description: "Draws with whoever is waiting next, or waits for the next person.",
  },
  {
    label: "Invite a &Friend...",
    action: () => room.actions.inviteFriend?.(),
    enabled: () => room.signedIn,
    description: "Opens a room that only people you let in can join.",
  },
  {
    label: "&Leave Room",
    action: () => room.actions.leave?.(),
    enabled: () => room.mode !== null,
    description: "Stops drawing together. Your picture stays.",
  },
  MENU_DIVIDER,
  {
    label: "Change &Nickname...",
    action: () => room.actions.changeNickname?.(),
    enabled: () => room.signedIn,
    description: "Changes the name the other person sees.",
  },
  {
    label: "Sign &Out",
    action: () => room.actions.signOut?.(),
    enabled: () => room.signedIn,
    description: "Signs out of drawme-1998 in this browser.",
  },
];

// Object keys keep their insertion order, which is the menu bar's order.
const after = [localize("E&xtras")].filter((key) => key in menus);
const moved = after.map((key) => [key, menus[key]]);
for (const key of after) delete menus[key];
menus["&Room"] = roomMenu;
for (const [key, items] of moved) menus[key] = items;
