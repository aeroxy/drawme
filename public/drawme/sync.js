// Keeps two canvases the same, peer to peer, whatever jspaint tool made the
// change, because it compares pixels, not tools.
//
// jspaint announces every change it commits with "session-update". Each time,
// the canvas is compared with the last copy both sides agreed on, and only
// the pixels that differ go out: a bounding box, one bit per pixel saying
// which changed, and their colors, deflated.
//
// Every pixel remembers the stamp of the last write it took: a counter both
// sides keep ahead of everything they've seen (a Lamport clock), with one
// bit for host or guest to break ties. A write lands only on pixels whose
// stamp is older, so when both people paint the same spot at the same
// moment, both screens settle on the same winner, whichever stroke arrives
// first.
//
// The other person's pixels also go into the states saved in undo history,
// so undo takes back only your own strokes.
//
// A change of canvas size (resize, crop, rotate, open, new) can't be a
// patch: the whole picture goes as a PNG and lands as one undoable step. So
// does the room's picture when someone joins, which replaces theirs.
/* global main_canvas, main_ctx, current_history_node:writable, redos, $canvas_area */

import { cancel, deselect, get_history_ancestors, reset_canvas_and_history, undoable, update_helper_layer } from "../src/functions.js";
import { $G, get_help_folder_icon } from "../src/helpers.js";

const PATCH = 1;
const PICTURE = 2;
const START = 3;
const PATCH_HEADER = 29;
const PICTURE_HEADER = 13;
/** Messages are cut into pieces this size, which every browser's channel takes. */
const CHUNK = 64 * 1024;
const CHUNK_HEADER = 8;
/** How many undo steps back get the other person's pixels. */
const HISTORY_DEPTH = 100;

export class CanvasSync {
  /**
   * @param {import("./rtc.js").Connection} connection
   * @param {{ host: boolean, onChange: () => void }} options
   */
  constructor(connection, { host, onChange }) {
    this.connection = connection;
    this.host = host;
    this.onChange = onChange;
    this.side = host ? 1 : 0;
    this.clock = 0;
    /** Whether both sides have the same picture, so patches mean something. */
    this.ready = false;
    /** Set while applying the other side's picture, which jspaint announces as a change of ours. */
    this.applying = false;
    /** @type {ImageData | null} the picture as both sides last agreed on it */
    this.synced = null;
    /** @type {Uint32Array} the stamp of the last write each pixel took */
    this.stamps = new Uint32Array(0);
    /** The stamp of the last whole picture, so an older one can't undo a newer resize. */
    this.pictureStamp = 0;
    this.nextId = 0;
    /** @type {Map<number, Uint8Array[]>} pieces of messages still arriving */
    this.pieces = new Map();
    this.sending = Promise.resolve();
    this.receiving = Promise.resolve();
  }

  start() {
    $G.on("session-update.drawme-sync", () => this.changed());
    // The host's picture is the room's: send it, and the guest takes it.
    if (this.host) this.sendStart();
  }

  stop() {
    $G.off(".drawme-sync");
    this.ready = false;
    this.pieces.clear();
  }

  /** The next stamp for a write of ours: newer than anything seen. */
  stamp() {
    this.clock += 1;
    return (this.clock * 2 + this.side) >>> 0;
  }

  /** @param {number} stamp a stamp from the other side */
  saw(stamp) {
    this.clock = Math.max(this.clock, stamp >>> 1);
  }

  /** Agrees on `picture` as the shared state, every pixel stamped `stamp`. */
  agree(picture, stamp) {
    this.synced = picture;
    this.stamps = new Uint32Array(picture.width * picture.height).fill(stamp);
    this.pictureStamp = stamp;
    this.ready = true;
  }

  sendStart() {
    const picture = main_ctx.getImageData(0, 0, main_canvas.width, main_canvas.height);
    const png = toPng(main_canvas);
    this.agree(picture, 0);
    this.queue(async () => this.ship(pictureMessage(START, 0, picture.width, picture.height, await png)));
  }

  /** jspaint committed a change: send what differs from the agreed picture. */
  changed() {
    if (!this.ready || this.applying || !this.synced) return;
    const width = main_canvas.width;
    const height = main_canvas.height;
    const now = main_ctx.getImageData(0, 0, width, height);
    if (width !== this.synced.width || height !== this.synced.height) {
      const stamp = this.stamp();
      const png = toPng(main_canvas);
      this.agree(now, stamp);
      this.queue(async () => this.ship(pictureMessage(PICTURE, stamp, width, height, await png)));
      return;
    }
    const current = new Uint32Array(now.data.buffer);
    const agreed = new Uint32Array(this.synced.data.buffer);
    let left = width;
    let top = height;
    let right = -1;
    let bottom = -1;
    for (let y = 0; y < height; y++) {
      const row = y * width;
      for (let x = 0; x < width; x++) {
        if (current[row + x] !== agreed[row + x]) {
          if (x < left) left = x;
          if (x > right) right = x;
          if (y < top) top = y;
          bottom = y;
        }
      }
    }
    if (right < 0) return;
    const w = right - left + 1;
    const h = bottom - top + 1;
    const stamp = this.stamp();
    const mask = new Uint8Array((w * h + 7) >> 3);
    const colors = new Uint8Array(w * h * 4);
    let count = 0;
    for (let j = 0; j < h; j++) {
      for (let i = 0; i < w; i++) {
        const p = (top + j) * width + left + i;
        if (current[p] === agreed[p]) continue;
        const m = j * w + i;
        mask[m >> 3] |= 1 << (m & 7);
        colors.set(now.data.subarray(p * 4, p * 4 + 4), count * 4);
        count += 1;
        agreed[p] = current[p];
        this.stamps[p] = stamp;
      }
    }
    const header = new DataView(new ArrayBuffer(PATCH_HEADER));
    header.setUint8(0, PATCH);
    [stamp, width, height, left, top, w, h].forEach((value, n) => header.setUint32(1 + n * 4, value));
    const body = concat([mask, colors.subarray(0, count * 4)]);
    this.queue(async () => this.ship(concat([new Uint8Array(header.buffer), await deflate(body)])));
  }

  /** Sends in the order changes happened, even though compressing takes a while. */
  queue(task) {
    this.sending = this.sending.then(task).catch((error) => console.error("drawme: sending failed", error));
  }

  /** Sends one message, in pieces. @param {Uint8Array} bytes */
  async ship(bytes) {
    const id = (this.nextId = (this.nextId + 1) >>> 0);
    const count = Math.max(1, Math.ceil(bytes.length / CHUNK));
    for (let index = 0; index < count; index++) {
      const piece = bytes.subarray(index * CHUNK, (index + 1) * CHUNK);
      const chunk = new Uint8Array(CHUNK_HEADER + piece.length);
      const view = new DataView(chunk.buffer);
      view.setUint32(0, id);
      view.setUint16(4, index);
      view.setUint16(6, count);
      chunk.set(piece, CHUNK_HEADER);
      await this.connection.drain();
      this.connection.send(chunk);
    }
  }

  /** A piece of a message from the other side. @param {ArrayBuffer} data */
  receive(data) {
    const view = new DataView(data);
    const id = view.getUint32(0);
    const index = view.getUint16(4);
    const count = view.getUint16(6);
    const parts = this.pieces.get(id) ?? [];
    parts[index] = new Uint8Array(data, CHUNK_HEADER);
    if (parts.filter(Boolean).length < count) {
      this.pieces.set(id, parts);
      return;
    }
    this.pieces.delete(id);
    const bytes = concat(parts);
    this.receiving = this.receiving.then(() => this.handle(bytes)).catch((error) => console.error("drawme: receiving failed", error));
  }

  /** @param {Uint8Array} bytes */
  async handle(bytes) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const kind = view.getUint8(0);
    if (kind === PATCH) {
      const [stamp, width, height, x, y, w, h] = [0, 1, 2, 3, 4, 5, 6].map((n) => view.getUint32(1 + n * 4));
      const body = await inflate(bytes.subarray(PATCH_HEADER));
      const maskLength = (w * h + 7) >> 3;
      this.patch({ stamp, width, height, x, y, w, h }, body.subarray(0, maskLength), body.subarray(maskLength));
    } else if (kind === PICTURE || kind === START) {
      const stamp = view.getUint32(1);
      const bitmap = await createImageBitmap(new Blob([bytes.subarray(PICTURE_HEADER)], { type: "image/png" }));
      if (kind === START) this.takeStart(bitmap);
      else this.picture(stamp, bitmap);
    }
  }

  /** The room's picture, as someone joins: theirs is replaced, history and all. */
  takeStart(bitmap) {
    this.applying = true;
    try {
      deselect();
      cancel();
      reset_canvas_and_history();
      main_ctx.copy(bitmap);
      current_history_node.image_data = main_ctx.getImageData(0, 0, main_canvas.width, main_canvas.height);
      $canvas_area.trigger("resize");
    } finally {
      this.applying = false;
    }
    this.clock = 0;
    this.agree(main_ctx.getImageData(0, 0, main_canvas.width, main_canvas.height), 0);
    this.onChange();
  }

  /** The other side's whole picture, after they changed the canvas size. */
  picture(stamp, bitmap) {
    this.saw(stamp);
    if (stamp < this.pictureStamp) return;
    this.applying = true;
    try {
      undoable({ name: "Partner's Change", icon: get_help_folder_icon("p_database.png") }, () => {
        main_ctx.copy(bitmap);
        $canvas_area.trigger("resize");
      });
    } finally {
      this.applying = false;
    }
    this.agree(main_ctx.getImageData(0, 0, main_canvas.width, main_canvas.height), stamp);
    this.onChange();
  }

  /** The other side's changed pixels. */
  patch({ stamp, width, height, x, y, w, h }, mask, colors) {
    this.saw(stamp);
    if (!this.synced || width !== main_canvas.width || height !== main_canvas.height) {
      return; // Made on a canvas size since replaced; the resize that replaced it wins.
    }
    // Where the changed pixels are, as indexes into the whole canvas.
    const where = new Uint32Array(colors.length / 4);
    let count = 0;
    for (let j = 0; j < h; j++) {
      for (let i = 0; i < w; i++) {
        const m = j * w + i;
        if (mask[m >> 3] & (1 << (m & 7))) where[count++] = (y + j) * width + x + i;
      }
    }
    // Saved states before this one get every pixel, so undoing a stroke of
    // your own never takes away one of theirs.
    for (const node of [...get_history_ancestors(current_history_node).slice(0, HISTORY_DEPTH), ...redos]) {
      const data = node.image_data;
      if (!data || data.width !== width || data.height !== height) continue;
      for (let n = 0; n < count; n++) data.data.set(colors.subarray(n * 4, n * 4 + 4), where[n] * 4);
    }
    // The canvas, the agreed copy and the current state take only the
    // writes newer than what each pixel has.
    const region = main_ctx.getImageData(x, y, w, h);
    const now = current_history_node.image_data;
    const current = now && now.width === width && now.height === height ? now.data : null;
    let landed = 0;
    for (let n = 0; n < count; n++) {
      const p = where[n];
      if (stamp <= this.stamps[p]) continue;
      this.stamps[p] = stamp;
      const color = colors.subarray(n * 4, n * 4 + 4);
      const px = p % width - x;
      const py = Math.floor(p / width) - y;
      region.data.set(color, (py * w + px) * 4);
      this.synced.data.set(color, p * 4);
      current?.set(color, p * 4);
      landed += 1;
    }
    if (landed === 0) return;
    main_ctx.putImageData(region, x, y);
    update_helper_layer();
    this.onChange();
  }
}

/** A PNG of the canvas as it is now: the browser copies it before returning. */
function toPng(canvas) {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => (blob ? blob.arrayBuffer().then((buffer) => resolve(new Uint8Array(buffer)), reject) : reject(new Error("no PNG"))), "image/png");
  });
}

function pictureMessage(kind, stamp, width, height, png) {
  const header = new DataView(new ArrayBuffer(PICTURE_HEADER));
  header.setUint8(0, kind);
  header.setUint32(1, stamp);
  header.setUint32(5, width);
  header.setUint32(9, height);
  return concat([new Uint8Array(header.buffer), png]);
}

/** @param {Uint8Array[]} parts */
function concat(parts) {
  const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** @param {Uint8Array} bytes */
async function deflate(bytes) {
  const stream = new Blob([bytes]).stream().pipeThrough(new CompressionStream("deflate-raw"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** @param {Uint8Array} bytes */
async function inflate(bytes) {
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("deflate-raw"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}
