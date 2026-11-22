//! Rooms: who is waiting, who is paired with whom, and each person's inbox
//! for the few messages that set up a direct connection. Everything lives in
//! Redis, and every operation is one Lua script, so it runs atomically: two
//! people arriving at the same moment still pair up, and a knock can't race
//! an admit.
//!
//! The server never sees a stroke or a sound. Once two browsers are
//! connected, nothing here is called again until one of them leaves.
//!
//! Keys, under a prefix (`drawme:` deployed, `dev:` locally, its own per
//! test run):
//!
//! | Key | What |
//! |---|---|
//! | `room:{id}` | hash: `mode` (`match` or `invite`), `host` and `guest` peers |
//! | `peer:{id}` | hash: the `user` it belongs to, its `room`, and a pending `knock` |
//! | `waiting` | list of match rooms whose host is alone, oldest first |
//! | `alive:{peer}` | set while the peer keeps asking for its inbox |
//! | `inbox:{peer}` | list of JSON messages for the peer |
//! | `knocks:{room}` | hash: knock id to the knocking peer |
//! | `admitted:{room}` | set of people the host let in, who may come back without knocking |
//!
//! A peer is a random 128-bit id, known only to the browser it belongs to,
//! and every call checks it against who is signed in.

use std::sync::LazyLock;

use redis::aio::ConnectionManager;
use redis::{RedisError, Script};
use uuid::Uuid;

/// How long rooms, peers and inboxes outlive their last use. Two people
/// drawing together never call the server, so this is generous.
const TTL_SECONDS: u64 = 24 * 60 * 60;

/// How long a peer counts as here after it last asked for its inbox. Pages
/// ask every few seconds, but a browser can slow a hidden tab's timers to
/// once a minute, so this is longer than that.
pub const ALIVE_SECONDS: u64 = 90;

/// Helpers every script starts with. ARGV[1..4] are always the prefix, the
/// two lifetimes and the person asking; each script's own arguments follow.
const PRELUDE: &str = r#"
local P, TTL, ALIVE, USER = ARGV[1], tonumber(ARGV[2]), tonumber(ARGV[3]), ARGV[4]
local WAITING = P .. 'waiting'
local function key(kind, id) return P .. kind .. ':' .. id end
local function get(k, f) local v = redis.call('HGET', k, f); if v then return v end; return '' end
local function touch(k) redis.call('EXPIRE', k, TTL) end
local function is_alive(peer) return peer ~= '' and redis.call('EXISTS', key('alive', peer)) == 1 end
local function keep_alive(peer) redis.call('SET', key('alive', peer), '1', 'EX', ALIVE) end
local function tell(peer, message) local k = key('inbox', peer); redis.call('RPUSH', k, message); touch(k) end
local function forget(peer) redis.call('DEL', key('peer', peer), key('inbox', peer), key('alive', peer)) end
local function new_peer(peer, room)
  local k = key('peer', peer)
  redis.call('HSET', k, 'user', USER, 'room', room)
  touch(k)
  keep_alive(peer)
end
-- The room of the caller's own peer, or nil when the peer is gone or isn't theirs.
local function mine(peer)
  local k = key('peer', peer)
  if peer == '' or get(k, 'user') ~= USER then return nil end
  return get(k, 'room')
end
-- The oldest match room whose host is still here and isn't the caller,
-- clearing out rooms whose host has gone on the way.
local function next_waiting(except)
  for _, room in ipairs(redis.call('LRANGE', WAITING, 0, -1)) do
    if room ~= except then
      local rk = key('room', room)
      local host, guest = get(rk, 'host'), get(rk, 'guest')
      if guest ~= '' or not is_alive(host) then
        redis.call('LREM', WAITING, 0, room)
        if guest == '' then
          redis.call('DEL', rk)
          if host ~= '' then forget(host) end
        end
      elseif get(key('peer', host), 'user') ~= USER then
        return room, host
      end
    end
  end
  return nil, nil
end
"#;

/// Joins the oldest room with someone waiting alone, or opens one and waits.
/// ARGV[5] new peer, ARGV[6] the id for a new room.
const MATCH: &str = r#"
local peer, fresh = ARGV[5], ARGV[6]
local room, host = next_waiting(nil)
if room then
  redis.call('LREM', WAITING, 0, room)
  local rk = key('room', room)
  redis.call('HSET', rk, 'guest', peer)
  touch(rk)
  new_peer(peer, room)
  tell(host, '{"type":"joined"}')
  return {'guest', room}
end
local rk = key('room', fresh)
redis.call('HSET', rk, 'mode', 'match', 'host', peer, 'guest', '')
touch(rk)
new_peer(peer, fresh)
redis.call('RPUSH', WAITING, fresh)
return {'host', fresh}
"#;

/// Opens a room that only a knock the host admits can join. The host counts
/// as admitted, so they can come back to it if their connection drops.
/// ARGV[5] new peer, ARGV[6] new room.
const INVITE: &str = r#"
local peer, room = ARGV[5], ARGV[6]
local rk = key('room', room)
redis.call('HSET', rk, 'mode', 'invite', 'host', peer, 'guest', '')
touch(rk)
new_peer(peer, room)
local ak = key('admitted', room)
redis.call('SADD', ak, USER)
touch(ak)
return {'host', room}
"#;

/// Asks to join an invite room. Someone the host admitted before walks
/// straight back in. A knock doesn't need the host to be asking for their
/// inbox right now: it waits for them (a phone's browser pauses a page while
/// its owner is off sending the link). The host themselves, back after
/// reloading, takes the room back, with whoever is still knocking.
/// ARGV[5] new peer, ARGV[6] room, ARGV[7] knock id, ARGV[8] the message for the host.
const KNOCK: &str = r#"
local peer, room, knock, message = ARGV[5], ARGV[6], ARGV[7], ARGV[8]
local rk = key('room', room)
if get(rk, 'mode') ~= 'invite' then return {'gone'} end
local host = get(rk, 'host')
if get(key('peer', host), 'user') == USER and not is_alive(host) then
  forget(host)
  redis.call('HSET', rk, 'host', peer)
  touch(rk)
  new_peer(peer, room)
  for _, knocker in ipairs(redis.call('HVALS', key('knocks', room))) do
    local pending = get(key('peer', knocker), 'message')
    if pending ~= '' then tell(peer, pending) end
  end
  return {'host'}
end
if get(rk, 'guest') ~= '' then return {'full'} end
new_peer(peer, room)
if redis.call('SISMEMBER', key('admitted', room), USER) == 1 then
  redis.call('HSET', rk, 'guest', peer)
  touch(rk)
  tell(host, '{"type":"joined"}')
  return {'admitted'}
end
local kk = key('knocks', room)
redis.call('HSET', kk, knock, peer)
touch(kk)
redis.call('HSET', key('peer', peer), 'knock', knock, 'message', message)
tell(host, message)
return {'waiting'}
"#;

/// The host lets a knock in, or doesn't. Letting one in turns away everyone
/// else still knocking. ARGV[5] host peer, ARGV[6] knock id, ARGV[7] '1' to admit.
const ADMIT: &str = r#"
local peer, knock, yes = ARGV[5], ARGV[6], ARGV[7] == '1'
local room = mine(peer)
if not room then return {'gone'} end
local rk = key('room', room)
if get(rk, 'host') ~= peer then return {'gone'} end
local kk = key('knocks', room)
local knocker = redis.call('HGET', kk, knock)
if not knocker then return {'gone'} end
redis.call('HDEL', kk, knock)
redis.call('HDEL', key('peer', knocker), 'knock', 'message')
keep_alive(peer)
if not yes then
  tell(knocker, '{"type":"denied"}')
  return {'denied'}
end
if get(rk, 'guest') ~= '' then
  tell(knocker, '{"type":"denied","reason":"full"}')
  return {'full'}
end
if not is_alive(knocker) then
  forget(knocker)
  return {'gone'}
end
redis.call('HSET', rk, 'guest', knocker)
touch(rk)
local ak = key('admitted', room)
redis.call('SADD', ak, get(key('peer', knocker), 'user'))
touch(ak)
tell(knocker, '{"type":"admitted"}')
for _, other in ipairs(redis.call('HVALS', kk)) do
  tell(other, '{"type":"denied","reason":"full"}')
  redis.call('HDEL', key('peer', other), 'knock', 'message')
end
redis.call('DEL', kk)
return {'admitted'}
"#;

/// Hands over what has arrived for a peer, and marks it as still here.
/// ARGV[5] peer.
const POLL: &str = r#"
local peer = ARGV[5]
local room = mine(peer)
if not room then return {'gone'} end
keep_alive(peer)
touch(key('peer', peer))
if room ~= '' then touch(key('room', room)) end
local ik = key('inbox', peer)
local messages = redis.call('LRANGE', ik, 0, -1)
redis.call('DEL', ik)
table.insert(messages, 1, 'ok')
return messages
"#;

/// Passes a message to the other person in the room. ARGV[5] peer, ARGV[6] message.
const SIGNAL: &str = r#"
local peer = ARGV[5]
local room = mine(peer)
if not room then return {'gone'} end
local rk = key('room', room)
local host, guest = get(rk, 'host'), get(rk, 'guest')
local other
if host == peer then other = guest elseif guest == peer then other = host else return {'gone'} end
if other == '' then return {'alone'} end
tell(other, ARGV[6])
keep_alive(peer)
return {'ok'}
"#;

/// The other person is gone (their connection closed or never came up): the
/// caller stays, as host, and the room opens again. A match room takes
/// whoever is already waiting alone, or rejoins the line.
/// ARGV[5] peer.
const REOPEN: &str = r#"
local peer = ARGV[5]
local room = mine(peer)
if not room then return {'gone'} end
local rk = key('room', room)
local host, guest = get(rk, 'host'), get(rk, 'guest')
if host ~= peer and guest ~= peer then return {'gone'} end
local other = guest
if guest == peer then other = host end
if other ~= '' then forget(other) end
redis.call('HSET', rk, 'host', peer, 'guest', '')
touch(rk)
keep_alive(peer)
if get(rk, 'mode') ~= 'match' then return {'waiting'} end
local wroom, whost = next_waiting(room)
if wroom then
  redis.call('LREM', WAITING, 0, wroom)
  redis.call('DEL', key('room', wroom))
  local pk = key('peer', whost)
  redis.call('HSET', pk, 'room', room)
  touch(pk)
  redis.call('HSET', rk, 'guest', whost)
  tell(whost, '{"type":"moved"}')
  return {'paired'}
end
redis.call('LREM', WAITING, 0, room)
redis.call('RPUSH', WAITING, room)
return {'waiting'}
"#;

/// The caller leaves. Whoever remains keeps the room, as host; an empty room
/// closes, turning away anyone still knocking. With ARGV[6] '1' (a page
/// closing or reloading), the host of an invite room who is alone keeps it
/// instead, so the link they sent still works when they come back.
/// ARGV[5] peer.
const LEAVE: &str = r#"
local peer, keep = ARGV[5], ARGV[6] == '1'
local room = mine(peer)
if not room then return {'ok'} end
if room ~= '' then
  local rk = key('room', room)
  local host, guest = get(rk, 'host'), get(rk, 'guest')
  if keep and host == peer and guest == '' and get(rk, 'mode') == 'invite' then
    redis.call('DEL', key('alive', peer))
    return {'kept'}
  end
  local knock = get(key('peer', peer), 'knock')
  if knock ~= '' then
    redis.call('HDEL', key('knocks', room), knock)
    if host ~= '' then tell(host, '{"type":"knock-cancelled","knock":"' .. knock .. '"}') end
  elseif host == peer or guest == peer then
    local other = guest
    if guest == peer then other = host end
    if other ~= '' then
      redis.call('HSET', rk, 'host', other, 'guest', '')
      touch(rk)
      tell(other, '{"type":"left"}')
    else
      for _, knocker in ipairs(redis.call('HVALS', key('knocks', room))) do
        tell(knocker, '{"type":"denied","reason":"ended"}')
      end
      redis.call('LREM', WAITING, 0, room)
      redis.call('DEL', rk, key('knocks', room), key('admitted', room))
    end
  end
end
forget(peer)
return {'ok'}
"#;

fn script(body: &str) -> Script {
  Script::new(&format!("{PRELUDE}{body}"))
}

static MATCH_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(MATCH));
static INVITE_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(INVITE));
static KNOCK_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(KNOCK));
static ADMIT_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(ADMIT));
static POLL_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(POLL));
static SIGNAL_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(SIGNAL));
static REOPEN_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(REOPEN));
static LEAVE_SCRIPT: LazyLock<Script> = LazyLock::new(|| script(LEAVE));

/// Which side of a room a peer is on. The host's drawing is the one a
/// newcomer gets; the guest starts the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
  Host,
  Guest,
}

/// A place in a room.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Seat {
  pub peer: String,
  pub room: String,
  pub role: Role,
}

/// How a knock went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Knock {
  /// The host has been asked.
  Waiting,
  /// Let straight in: the host admitted this person before.
  Admitted,
  /// It's the host's own room, which they left with a page that closed or
  /// reloaded: theirs again, still hosting.
  Host,
}

/// What the host's answer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Admitted {
  Admitted,
  Denied,
  /// Someone else got the seat first; this knock was turned away.
  Full,
}

/// Where a reopened room stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reopened {
  /// Alone, waiting for the next person.
  Waiting,
  /// Someone already waiting alone was brought in; their offer is coming.
  Paired,
}

#[derive(Debug)]
pub enum RoomError {
  /// The peer, room or knock no longer exists, or isn't the caller's.
  Gone,
  /// The room already has two people.
  Full,
  /// Nobody else is in the room to pass a message to.
  Alone,
  Redis(RedisError),
}

impl From<RedisError> for RoomError {
  fn from(err: RedisError) -> Self {
    RoomError::Redis(err)
  }
}

impl std::fmt::Display for RoomError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      RoomError::Gone => f.write_str("gone"),
      RoomError::Full => f.write_str("full"),
      RoomError::Alone => f.write_str("alone"),
      RoomError::Redis(err) => write!(f, "redis: {err}"),
    }
  }
}

impl std::error::Error for RoomError {}

/// A fresh id for a peer, room or knock: 128 random bits as 32 hex digits.
pub fn new_id() -> String {
  Uuid::new_v4().simple().to_string()
}

/// Whether text could be an id this module made. Checked before an id from a
/// request goes anywhere near a key name.
pub fn is_id(text: &str) -> bool {
  text.len() == 32
    && text
      .bytes()
      .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone)]
pub struct Rooms {
  conn: ConnectionManager,
  prefix: String,
}

impl Rooms {
  /// Connects to the Redis at `url` (`rediss://` for TLS, as Upstash
  /// needs), keeping every key under `prefix`.
  pub async fn connect(url: &str, prefix: &str) -> Result<Rooms, RedisError> {
    let client = redis::Client::open(url)?;
    let conn = ConnectionManager::new(client).await?;
    Ok(Rooms {
      conn,
      prefix: prefix.to_owned(),
    })
  }

  pub fn prefix(&self) -> &str {
    &self.prefix
  }

  /// A connection for tests that look at keys directly.
  pub fn connection(&self) -> ConnectionManager {
    self.conn.clone()
  }

  /// Runs a script with the common arguments, then `args`. A connection that
  /// dropped while the instance sat idle is retried once.
  async fn run(
    &self,
    script: &Script,
    user: &Uuid,
    args: &[&str],
  ) -> Result<Vec<String>, RoomError> {
    let mut attempt = 0;
    loop {
      let mut invocation = script.prepare_invoke();
      invocation
        .arg(&self.prefix)
        .arg(TTL_SECONDS)
        .arg(ALIVE_SECONDS)
        .arg(user.simple().to_string());
      for arg in args {
        invocation.arg(*arg);
      }
      let mut conn = self.conn.clone();
      match invocation.invoke_async::<Vec<String>>(&mut conn).await {
        Ok(reply) => return Ok(reply),
        Err(err) if attempt == 0 && (err.is_io_error() || err.is_connection_dropped()) => {
          attempt += 1
        }
        Err(err) => return Err(err.into()),
      }
    }
  }

  /// Pairs the caller with whoever has waited longest, or opens a room for
  /// them to wait in. Never pairs someone with themselves.
  pub async fn match_up(&self, user: &Uuid) -> Result<Seat, RoomError> {
    let peer = new_id();
    let reply = self.run(&MATCH_SCRIPT, user, &[&peer, &new_id()]).await?;
    seat(peer, &reply)
  }

  /// Opens an invite room, with the caller as host.
  pub async fn invite(&self, user: &Uuid) -> Result<Seat, RoomError> {
    let peer = new_id();
    let reply = self.run(&INVITE_SCRIPT, user, &[&peer, &new_id()]).await?;
    seat(peer, &reply)
  }

  /// Knocks on an invite room. The host sees `email` and `nickname`.
  pub async fn knock(
    &self,
    user: &Uuid,
    room: &str,
    email: &str,
    nickname: &str,
  ) -> Result<(String, Knock), RoomError> {
    if !is_id(room) {
      return Err(RoomError::Gone);
    }
    let peer = new_id();
    let knock = new_id();
    let message =
      serde_json::json!({ "type": "knock", "knock": knock, "email": email, "nickname": nickname })
        .to_string();
    let reply = self
      .run(&KNOCK_SCRIPT, user, &[&peer, room, &knock, &message])
      .await?;
    match status(&reply) {
      "waiting" => Ok((peer, Knock::Waiting)),
      "admitted" => Ok((peer, Knock::Admitted)),
      "host" => Ok((peer, Knock::Host)),
      "full" => Err(RoomError::Full),
      _ => Err(RoomError::Gone),
    }
  }

  /// The host's answer to a knock.
  pub async fn admit(
    &self,
    user: &Uuid,
    peer: &str,
    knock: &str,
    admit: bool,
  ) -> Result<Admitted, RoomError> {
    if !is_id(peer) || !is_id(knock) {
      return Err(RoomError::Gone);
    }
    let reply = self
      .run(
        &ADMIT_SCRIPT,
        user,
        &[peer, knock, if admit { "1" } else { "0" }],
      )
      .await?;
    match status(&reply) {
      "admitted" => Ok(Admitted::Admitted),
      "denied" => Ok(Admitted::Denied),
      "full" => Ok(Admitted::Full),
      _ => Err(RoomError::Gone),
    }
  }

  /// What has arrived for the peer, oldest first, as JSON text.
  pub async fn poll(&self, user: &Uuid, peer: &str) -> Result<Vec<String>, RoomError> {
    if !is_id(peer) {
      return Err(RoomError::Gone);
    }
    let mut reply = self.run(&POLL_SCRIPT, user, &[peer]).await?;
    if status(&reply) != "ok" {
      return Err(RoomError::Gone);
    }
    reply.remove(0);
    Ok(reply)
  }

  /// Passes `message` (JSON text) to the other person in the peer's room.
  pub async fn signal(&self, user: &Uuid, peer: &str, message: &str) -> Result<(), RoomError> {
    if !is_id(peer) {
      return Err(RoomError::Gone);
    }
    let reply = self.run(&SIGNAL_SCRIPT, user, &[peer, message]).await?;
    match status(&reply) {
      "ok" => Ok(()),
      "alone" => Err(RoomError::Alone),
      _ => Err(RoomError::Gone),
    }
  }

  /// The other person is gone: the caller keeps the room and it opens again.
  pub async fn reopen(&self, user: &Uuid, peer: &str) -> Result<Reopened, RoomError> {
    if !is_id(peer) {
      return Err(RoomError::Gone);
    }
    let reply = self.run(&REOPEN_SCRIPT, user, &[peer]).await?;
    match status(&reply) {
      "waiting" => Ok(Reopened::Waiting),
      "paired" => Ok(Reopened::Paired),
      _ => Err(RoomError::Gone),
    }
  }

  /// The peer leaves. Leaving twice, or a peer that is already gone, is fine.
  /// `keep`: the page is closing or reloading, and an invite room's host who
  /// is alone keeps the room for when they're back.
  pub async fn leave(&self, user: &Uuid, peer: &str, keep: bool) -> Result<(), RoomError> {
    if !is_id(peer) {
      return Ok(());
    }
    self
      .run(&LEAVE_SCRIPT, user, &[peer, if keep { "1" } else { "0" }])
      .await?;
    Ok(())
  }
}

fn status(reply: &[String]) -> &str {
  reply.first().map(String::as_str).unwrap_or("")
}

fn seat(peer: String, reply: &[String]) -> Result<Seat, RoomError> {
  let role = match status(reply) {
    "host" => Role::Host,
    "guest" => Role::Guest,
    _ => return Err(RoomError::Gone),
  };
  let room = reply.get(1).cloned().ok_or(RoomError::Gone)?;
  Ok(Seat { peer, room, role })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_are_32_lowercase_hex_digits() {
    let id = new_id();
    assert!(is_id(&id), "{id}");
    assert_ne!(new_id(), id);
    for bad in [
      "",
      "abc",
      "g".repeat(32).as_str(),
      "ABCDEF0123456789ABCDEF0123456789",
      "0123456789abcdef0123456789abcde:",
    ] {
      assert!(!is_id(bad), "{bad:?}");
    }
  }
}
