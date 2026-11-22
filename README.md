# drawme-1998

Draw together, two at a time, in a classic paint program, and talk while you draw. Match pairs you with someone: the first person to arrive opens a room, and the next one joins it. Or open a room of your own and send a friend the link: you see who is knocking, and choose whether to let them in.

## How it behaves

| Rule | Where it lives |
|---|---|
| Everyone signs in with Aero, then picks a nickname. The nickname is all the other person sees; it lives in this browser's IndexedDB and changes from the Room menu | `src/auth.rs`; `signedOut` and `askNickname` in `public/drawme/room.js` |
| **Match**: the first person to arrive opens a room, the next joins it, and the one after opens a new one. Whoever has waited longest is served first, and nobody is paired with themselves | `MATCH` in `src/rooms.rs` |
| **Invite**: the host gets a link, `/#invite:<room>`. Opening it knocks, and the host sees the knocker's nickname and Aero email and lets them in or not. Letting one in turns away anyone else knocking. Someone let in before walks straight back in, the host included | `INVITE`, `KNOCK` and `ADMIT` in `src/rooms.rs`; `askHost` in `public/drawme/room.js` |
| A knock waits for a host who has stepped away, since a phone pauses the page while its owner sends the link. A host who reloads while alone keeps the room, so the link they sent still works, and anyone knocking is asked about again | `KNOCK` and `LEAVE` in `src/rooms.rs` |
| When the other person leaves, or the connection breaks, you keep the room and the picture, and the next person joins you: in Match, whoever is already waiting alone, else the next to arrive. Joining a room means taking its picture | `REOPEN` in `src/rooms.rs`; `partnerGone` in `public/drawme/room.js` |
| Drawing goes browser to browser. After each change JS Paint commits, only the pixels that differ go, each with a stamp, so two strokes on the same spot at the same moment end up the same on both screens. A change of canvas size sends the whole picture | `public/drawme/sync.js` |
| Undo takes back only your own strokes: the other person's pixels go into your undo history too | `patch` in `public/drawme/sync.js` |
| Voice: Room > Microphone, or the mic button in the status bar. The audio line is set up with the connection, so turning a mic on or off never needs another handshake. A speaker icon shows when the other person's mic is on | `public/drawme/rtc.js`; `toggleMic` in `public/drawme/room.js` |
| The other person's pointer shows on your canvas, with their nickname | `showCursor` in `public/drawme/room.js` |
| The picture is kept in this browser's IndexedDB, as a PNG, so a reload doesn't lose it. Nothing about the picture ever reaches the server | `saveSoon` and `restoreDrawing` in `public/drawme/room.js`; `public/drawme/idb.js` |
| File > New and File > Open stay in the room, and the new picture goes to the other person | `window.new_local_session` in `public/drawme/room.js` |

## What runs where

| Piece | Choice |
|---|---|
| Paint program | JS Paint at commit `53be67a`, from jsDelivr. `vercel.json` rewrites its folders there, so the browser sees one origin (its web workers and relative paths need that) and Vercel's CDN keeps each file, which jsDelivr marks immutable for a year. This repository holds only what differs: `public/index.html`, JS Paint's page with the room module in place of its sessions, and `public/drawme/`. Moving to another commit means changing `vercel.json` and `JSPAINT_COMMIT` in `src/bin/dev.rs`, which a test keeps in step |
| API | Rust: axum 0.8 as one Vercel Function (`api/server.rs`) through `vercel_runtime`'s axum layer, in `sin1`. Vercel's Rust runtime is in beta |
| Rooms | Redis (Upstash) over TLS. Every operation is one Lua script, so it runs atomically. Keys expire by themselves: a room outlives its last use by a day, and a peer counts as gone 90 s after it last asked for its inbox |
| Realtime | WebRTC between the two browsers: a reliable data channel for pixels, an unreliable one for pointers, and one audio line. STUN from Cloudflare and Google, and Cloudflare's TURN relay once set up, for networks that block direct connections, with credentials minted per room so the key stays on the server |
| Sign-in | [Aero](https://aero.oops.wtf)'s Login with Aero: identity only. The token (PASETO `v4.local`: the visitor's email, encrypted with a key only Aero and drawme hold, valid 30 days) goes from the callback page straight to the API, which keeps it in an HttpOnly cookie. No script can read it, and the `sendBeacon` a closing page sends still carries it |

## Local development

```sh
cp .env.example .env      # set REDIS_URL
cargo run --bin dev       # http://127.0.0.1:8787
```

The dev server serves `public/`, fetches JS Paint's files from jsDelivr as Vercel does, and keeps its Redis keys under `dev:`, apart from the deployment's. Aero only answers on the deployed callback, so signing in locally is a form that takes any email and mints the same kind of token, through the same callback page. Two browser profiles, or a normal and a private window, are two people.

## Tests

```sh
cargo test                                  # unit tests
TEST_REDIS_URL=rediss://... cargo test      # and the API against Redis
```

The API tests keep their keys under a prefix of their own and delete them afterwards, so the real database will do. Without `TEST_REDIS_URL` they return early.

## Deploying on Vercel

1. Import the repository as a Vercel project. `vercel.json` does the rest: Vercel builds `api/server.rs` as a function, serves `public/`, and rewrites `/api/*` to the function and JS Paint's folders to jsDelivr.
2. Set `REDIS_URL` in the project's environment variables: an Upstash database's `rediss://` address, ideally in AWS Singapore (`ap-southeast-1`) to match the `sin1` function region.
3. Register drawme on [Aero's Connect page](https://aero.oops.wtf/connect) under Login with Aero, with the callback `https://<your domain>/auth/aero`. Set the client id and the `k4.local.…` key it gives as `AERO_CLIENT_ID` and `AERO_LOGIN_KEY` in the project's environment variables, never in the repository. Until both are set, nobody can sign in.
4. Optionally, create a TURN key in Cloudflare's Realtime dashboard and set `TURN_KEY_ID` and `TURN_KEY_API_TOKEN`. Without them, browsers connect with STUN only, which some networks block. Cloudflare's first 1,000 GB a month are free, and a voice call is a few dozen kilobits a second.
5. Deploy, or push to the production branch.

## API

| Method | Path | Body | Answer |
|---|---|---|---|
| `GET` | `/api/sign-in?return=` | | 303 to Aero's consent screen, keeping `state` and where to come back to (`#invite:<room>`, or nothing) in a 10-minute cookie |
| `POST` | `/api/session` | `{ token, state }` | sets the session cookie, if `state` matches this browser's sign-in; `{ email, return }` |
| `GET` | `/api/me` | | `{ email }`, or 401 |
| `POST` | `/api/sign-out` | | 204, and the cookie cleared |
| `GET` | `/api/ice` | | `{ iceServers }` for `RTCPeerConnection` |
| `POST` | `/api/match` | | `{ peer, room, role }`: `guest` when someone was waiting, else `host` |
| `POST` | `/api/invite` | | `{ peer, room, role: "host" }` |
| `POST` | `/api/knock` | `{ room, nickname }` | `{ peer, room, status }`: `waiting`, `admitted` (let in before) or `host` (your own room, back after a reload); 409 when full, 410 when closed |
| `POST` | `/api/admit` | `{ peer, knock, admit }` | `{ result }`: `admitted`, `denied`, or `full` when someone else got in first |
| `GET` | `/api/inbox?peer=` | | `{ messages }`, oldest first. Asking also marks the peer as still here; 410 once its seat is gone |
| `POST` | `/api/signal` | `{ peer, data }` | 204: `data`, an offer or an answer, goes to the other person; 409 when alone |
| `POST` | `/api/reopen` | `{ peer }` | `{ status }`: `waiting`, or `paired` when someone waiting alone was brought in |
| `POST` | `/api/leave` | `{ peer, keep? }` | 204. `keep`, from a closing page, keeps an invite room whose host is alone |

Every call after `/api/session` needs the session cookie, and a `peer` answers only to the person it belongs to. Inbox messages are `joined`, `moved`, `knock` (with `knock`, `email`, `nickname`), `knock-cancelled`, `admitted`, `denied` (with a `reason`: `full` or `ended`), `signal` (with `data`) and `left`. Errors come back as `{ error }`.

## Known limitations

- A stroke shows on the other screen once it's finished: JS Paint draws a stroke in progress on a layer of its own and commits it on release. Pointers move live.
- Undoing a stroke the other person has drawn over leaves their part, as it should. But if they then undo theirs, the part of yours they had covered can come back.
- Without the TURN key, two people can't connect when one of their networks blocks direct connections.

## License

MIT licensed: see [LICENSE](LICENSE).
