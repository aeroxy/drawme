// What the Room menu (menus.js) and the room itself (room.js) share. The
// menu bar is built before the room starts, so menu items read this when
// they're shown or clicked, and the room fills in `actions` once it runs.

export const room = {
  signedIn: false,
  /** "match" or "invite" while in a room, else null. */
  mode: null,
  /** Whether the direct connection to the other person is up. */
  connected: false,
  micOn: false,
  /** The link that asks to join, while this browser is in an invite room. */
  inviteLink: null,
  /** @type {Record<string, () => void>} */
  actions: {},
};
