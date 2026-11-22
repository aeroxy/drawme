// The direct connection to the other person: two data channels and a voice
// line, all set up by a single offer and answer through the server.
//
// - "pixels": reliable and ordered, for the drawing.
// - "cursor": unordered and unreliable, for where the other pointer is. A lost
//   position is soon replaced by the next one.
// - audio: negotiated up front in both directions, with nothing on it until a
//   microphone is turned on. Turning it on or off swaps the track, so it never
//   needs another handshake.

/** How long to wait for ICE candidates before sending what has been found. */
const GATHER_MS = 2500;
/** How long a connection may stay "disconnected" before it counts as gone. */
const LINGER_MS = 6000;
/** Above this much queued, sending waits for the channel to catch up. */
const QUEUE_LIMIT = 1 << 20;

export class Connection extends EventTarget {
  /** @param {RTCIceServer[]} iceServers */
  constructor(iceServers) {
    super();
    this.closed = false;
    this.pc = new RTCPeerConnection({ iceServers });
    this.pixels = this.pc.createDataChannel("pixels", { negotiated: true, id: 0 });
    this.pixels.binaryType = "arraybuffer";
    this.cursor = this.pc.createDataChannel("cursor", { negotiated: true, id: 1, ordered: false, maxRetransmits: 0 });
    /** @type {RTCRtpTransceiver | null} */
    this.audio = null;

    let opened = 0;
    const open = () => {
      if (++opened === 2) this.emit("open");
    };
    this.pixels.onopen = open;
    this.cursor.onopen = open;
    this.pixels.onclose = () => this.close();
    this.pixels.onmessage = (event) => this.emit("message", event.data);
    this.cursor.onmessage = (event) => {
      try {
        this.emit("cursor", JSON.parse(event.data));
      } catch {
        // Not a position.
      }
    };
    this.pc.ontrack = (event) => this.emit("track", event.streams[0] ?? new MediaStream([event.track]));
    let linger;
    this.pc.onconnectionstatechange = () => {
      clearTimeout(linger);
      const state = this.pc.connectionState;
      if (state === "failed" || state === "closed") this.close();
      else if (state === "disconnected") linger = setTimeout(() => this.close(), LINGER_MS);
    };
  }

  /** @param {string} type @param {unknown} [detail] */
  emit(type, detail) {
    this.dispatchEvent(new CustomEvent(type, { detail }));
  }

  /** The guest starts: an offer with every candidate already in it. */
  async offer() {
    this.audio = this.pc.addTransceiver("audio", { direction: "sendrecv" });
    await this.pc.setLocalDescription(await this.pc.createOffer());
    await gathered(this.pc);
    return { type: "offer", sdp: this.pc.localDescription?.sdp };
  }

  /** The host answers, sending sound both ways. @param {RTCSessionDescriptionInit} offer */
  async answer(offer) {
    await this.pc.setRemoteDescription(offer);
    this.audio = this.pc.getTransceivers().find((transceiver) => transceiver.receiver.track.kind === "audio") ?? null;
    if (this.audio) this.audio.direction = "sendrecv";
    await this.pc.setLocalDescription(await this.pc.createAnswer());
    await gathered(this.pc);
    return { type: "answer", sdp: this.pc.localDescription?.sdp };
  }

  /** @param {RTCSessionDescriptionInit} answer */
  async accept(answer) {
    await this.pc.setRemoteDescription(answer);
  }

  /** Puts a microphone track on the voice line, or takes it off (null). */
  async setMic(track) {
    await this.audio?.sender.replaceTrack(track ?? null);
  }

  /** @param {string | ArrayBuffer | Uint8Array} data */
  send(data) {
    if (this.pixels.readyState === "open") this.pixels.send(/** @type {any} */ (data));
  }

  sendCursor(position) {
    if (this.cursor.readyState === "open") this.cursor.send(JSON.stringify(position));
  }

  /** Resolves once the pixel channel has room for more, or has closed. */
  async drain() {
    const channel = this.pixels;
    if (channel.readyState !== "open" || channel.bufferedAmount <= QUEUE_LIMIT) return;
    channel.bufferedAmountLowThreshold = QUEUE_LIMIT / 2;
    await new Promise((resolve) => {
      channel.addEventListener("bufferedamountlow", resolve, { once: true });
      channel.addEventListener("close", resolve, { once: true });
    });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.pc.close();
    this.emit("close");
  }
}

/** Waits until ICE gathering is done, or GATHER_MS, whichever is first. */
function gathered(pc) {
  return new Promise((resolve) => {
    if (pc.iceGatheringState === "complete") return resolve(undefined);
    const check = () => {
      if (pc.iceGatheringState !== "complete") return;
      pc.removeEventListener("icegatheringstatechange", check);
      resolve(undefined);
    };
    pc.addEventListener("icegatheringstatechange", check);
    setTimeout(resolve, GATHER_MS);
  });
}
