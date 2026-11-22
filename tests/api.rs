//! The API end to end, through the router, against the Redis that
//! `TEST_REDIS_URL` names. Each run keeps its keys under a prefix of its own
//! and deletes them at the end, so any database will do, even the real one.
//! Without `TEST_REDIS_URL` these return early.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use drawme::{Options, Rooms, Verifier};
use pasetors::keys::{Generate, SymmetricKey};
use pasetors::paserk::FormatAsPaserk;
use pasetors::version4::V4;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Harness {
  app: Router,
  minter: Verifier,
  rooms: Rooms,
}

async fn harness() -> Option<Harness> {
  let Ok(url) = std::env::var("TEST_REDIS_URL") else {
    eprintln!("TEST_REDIS_URL is not set; skipping");
    return None;
  };
  let prefix = format!("test:{}:", drawme::rooms::new_id());
  let rooms = Rooms::connect(&url, &prefix)
    .await
    .expect("TEST_REDIS_URL should connect");
  let key = SymmetricKey::<V4>::generate().unwrap();
  let mut paserk = String::new();
  key.fmt(&mut paserk).unwrap();
  let options = Options {
    secure_cookies: true,
    dev_sign_in: false,
  };
  let app = drawme::app(
    rooms.clone(),
    Verifier::new(&paserk, "drawme-test").unwrap(),
    None,
    options,
  );
  Some(Harness {
    app,
    minter: Verifier::new(&paserk, "drawme-test").unwrap(),
    rooms,
  })
}

/// Someone signed in, as their session cookie.
struct Who(String);

impl Harness {
  fn person(&self, email: &str) -> Who {
    Who(format!(
      "drawme_session={}",
      self.minter.mint(email).unwrap()
    ))
  }

  async fn send(
    &self,
    method: Method,
    path: &str,
    who: Option<&Who>,
    body: Option<Value>,
  ) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(who) = who {
      request = request.header(header::COOKIE, &who.0);
    }
    let request = match body {
      Some(body) => request
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string())),
      None => request.body(Body::empty()),
    }
    .unwrap();
    let response = self.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
      .await
      .unwrap();
    (
      status,
      serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
  }

  async fn post(&self, who: &Who, path: &str, body: Value) -> (StatusCode, Value) {
    self.send(Method::POST, path, Some(who), Some(body)).await
  }

  /// Joins (or opens) a match room: (peer, room, role).
  async fn match_up(&self, who: &Who) -> (String, String, String) {
    let (status, seat) = self.post(who, "/api/match", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{seat}");
    (
      text(&seat["peer"]),
      text(&seat["room"]),
      text(&seat["role"]),
    )
  }

  async fn inbox(&self, who: &Who, peer: &str) -> (StatusCode, Vec<Value>) {
    let (status, body) = self
      .send(
        Method::GET,
        &format!("/api/inbox?peer={peer}"),
        Some(who),
        None,
      )
      .await;
    (
      status,
      body["messages"].as_array().cloned().unwrap_or_default(),
    )
  }

  /// The types of what has arrived for a peer.
  async fn arrived(&self, who: &Who, peer: &str) -> Vec<String> {
    let (status, messages) = self.inbox(who, peer).await;
    assert_eq!(status, StatusCode::OK);
    messages
      .iter()
      .map(|message| text(&message["type"]))
      .collect()
  }

  /// Makes a peer look like it stopped asking for its inbox long ago.
  async fn silence(&self, peer: &str) {
    let mut conn = self.rooms.connection();
    let _: () = redis::cmd("DEL")
      .arg(format!("{}alive:{peer}", self.rooms.prefix()))
      .query_async(&mut conn)
      .await
      .unwrap();
  }

  async fn clean_up(&self) {
    let mut conn = self.rooms.connection();
    let mut cursor = 0u64;
    loop {
      let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .arg(cursor)
        .arg("MATCH")
        .arg(format!("{}*", self.rooms.prefix()))
        .arg("COUNT")
        .arg(500)
        .query_async(&mut conn)
        .await
        .unwrap();
      if !keys.is_empty() {
        let _: () = redis::cmd("DEL")
          .arg(&keys)
          .query_async(&mut conn)
          .await
          .unwrap();
      }
      if next == 0 {
        break;
      }
      cursor = next;
    }
  }
}

fn text(value: &Value) -> String {
  value.as_str().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn rooms_need_a_sign_in_and_the_cookie_is_enough() {
  let Some(h) = harness().await else { return };
  let (status, _) = h.send(Method::GET, "/api/me", None, None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  let (status, _) = h
    .send(Method::POST, "/api/match", None, Some(json!({})))
    .await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
  let ada = h.person("Ada@Example.com");
  let (status, me) = h.send(Method::GET, "/api/me", Some(&ada), None).await;
  assert_eq!(
    (status, text(&me["email"])),
    (StatusCode::OK, "ada@example.com".to_owned())
  );
  let forged = Who(format!("{}x", ada.0));
  let (status, _) = h.send(Method::GET, "/api/me", Some(&forged), None).await;
  assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_sign_in_keeps_the_token_only_for_the_browser_that_started_it() {
  let Some(h) = harness().await else { return };
  let request = Request::get("/api/sign-in?return=%23invite%3A0123456789abcdef0123456789abcdef")
    .body(Body::empty())
    .unwrap();
  let response = h.app.clone().oneshot(request).await.unwrap();
  assert_eq!(response.status(), StatusCode::SEE_OTHER);
  let location = response.headers()[header::LOCATION]
    .to_str()
    .unwrap()
    .to_owned();
  assert!(
    location.starts_with("https://aero.oops.wtf/login-with-aero?client_id=drawme-test&state="),
    "{location}"
  );
  let state = location
    .split("state=")
    .nth(1)
    .unwrap()
    .split('&')
    .next()
    .unwrap()
    .to_owned();
  let set = response.headers()[header::SET_COOKIE]
    .to_str()
    .unwrap()
    .to_owned();
  assert!(
    set.starts_with(&format!("drawme_sign_in={state}|#invite:")),
    "{set}"
  );
  assert!(
    set.contains("HttpOnly") && set.contains("Secure") && set.contains("Path=/api"),
    "{set}"
  );
  let remembered = set.split(';').next().unwrap().to_owned();

  let token = h.minter.mint("ada@example.com").unwrap();
  let answer = |state: &str, cookie: &str| {
    Request::post("/api/session")
      .header(header::CONTENT_TYPE, "application/json")
      .header(header::COOKIE, cookie)
      .body(Body::from(
        json!({ "token": token, "state": state }).to_string(),
      ))
      .unwrap()
  };
  // An answer to someone else's sign-in, or with no sign-in started here, is refused.
  let response = h
    .app
    .clone()
    .oneshot(answer("0123456789abcdef0123456789abcdef", &remembered))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
  let response = h
    .app
    .clone()
    .oneshot(answer(&state, "other=1"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);

  let response = h
    .app
    .clone()
    .oneshot(answer(&state, &remembered))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let cookies: Vec<String> = response
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .map(|value| value.to_str().unwrap().to_owned())
    .collect();
  assert!(
    cookies
      .iter()
      .any(|c| c.starts_with(&format!("drawme_session={token};")) && c.contains("HttpOnly")),
    "{cookies:?}"
  );
  assert!(
    cookies
      .iter()
      .any(|c| c.starts_with("drawme_sign_in=;") && c.contains("Max-Age=0")),
    "{cookies:?}"
  );
  let body = axum::body::to_bytes(response.into_body(), 1 << 16)
    .await
    .unwrap();
  let body: Value = serde_json::from_slice(&body).unwrap();
  assert_eq!(
    text(&body["return"]),
    "#invite:0123456789abcdef0123456789abcdef"
  );
  // The deployed API has no local sign-in.
  let (status, _) = h.send(Method::POST, "/api/dev/aero", None, None).await;
  assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_second_visitor_joins_the_first_and_the_third_opens_a_new_room() {
  let Some(h) = harness().await else { return };
  let (ada, bob, cat) = (
    h.person("ada@example.com"),
    h.person("bob@example.com"),
    h.person("cat@example.com"),
  );
  let (ada_peer, first, role) = h.match_up(&ada).await;
  assert_eq!(role, "host");
  let (bob_peer, room, role) = h.match_up(&bob).await;
  assert_eq!((room.as_str(), role.as_str()), (first.as_str(), "guest"));
  let (_, second, role) = h.match_up(&cat).await;
  assert_eq!(role, "host");
  assert_ne!(second, first);
  assert_eq!(h.arrived(&ada, &ada_peer).await, ["joined"]);

  // The handshake: the guest's offer reaches the host, the answer comes back.
  let offer = json!({ "type": "offer", "sdp": "v=0 offer" });
  let (status, _) = h
    .post(
      &bob,
      "/api/signal",
      json!({ "peer": bob_peer, "data": offer }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (_, messages) = h.inbox(&ada, &ada_peer).await;
  assert_eq!(messages, [json!({ "type": "signal", "data": offer })]);
  let answer = json!({ "type": "answer", "sdp": "v=0 answer" });
  let (status, _) = h
    .post(
      &ada,
      "/api/signal",
      json!({ "peer": ada_peer, "data": answer }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (_, messages) = h.inbox(&bob, &bob_peer).await;
  assert_eq!(messages, [json!({ "type": "signal", "data": answer })]);
  assert!(
    h.arrived(&bob, &bob_peer).await.is_empty(),
    "an inbox empties as it's read"
  );
  h.clean_up().await;
}

#[tokio::test]
async fn nobody_is_paired_with_themselves() {
  let Some(h) = harness().await else { return };
  let (ada, bob) = (h.person("ada@example.com"), h.person("bob@example.com"));
  let (_, first, role) = h.match_up(&ada).await;
  assert_eq!(role, "host");
  // Ada again, in another tab: a room of her own, not a seat in her first.
  let (_, second, role) = h.match_up(&ada).await;
  assert_eq!(role, "host");
  assert_ne!(second, first);
  let (_, room, role) = h.match_up(&bob).await;
  assert_eq!(
    (room, role.as_str()),
    (first, "guest"),
    "the longest wait is served first"
  );
  h.clean_up().await;
}

#[tokio::test]
async fn people_arriving_at_the_same_moment_all_pair_up() {
  let Some(h) = harness().await else { return };
  let mut arrivals = tokio::task::JoinSet::new();
  for n in 0..10 {
    let app = h.app.clone();
    let who = h.person(&format!("p{n}@example.com"));
    arrivals.spawn(async move {
      let request = Request::post("/api/match")
        .header(header::COOKIE, who.0)
        .body(Body::empty())
        .unwrap();
      let response = app.oneshot(request).await.unwrap();
      let body = axum::body::to_bytes(response.into_body(), 1 << 16)
        .await
        .unwrap();
      let seat: Value = serde_json::from_slice(&body).unwrap();
      (text(&seat["room"]), text(&seat["role"]))
    });
  }
  let mut rooms = std::collections::HashMap::<String, Vec<String>>::new();
  while let Some(seat) = arrivals.join_next().await {
    let (room, role) = seat.unwrap();
    rooms.entry(room).or_default().push(role);
  }
  assert_eq!(rooms.len(), 5, "{rooms:?}");
  for roles in rooms.values_mut() {
    roles.sort();
    assert_eq!(roles, &["guest", "host"]);
  }
  h.clean_up().await;
}

#[tokio::test]
async fn a_host_who_has_gone_quiet_is_skipped() {
  let Some(h) = harness().await else { return };
  let (ada, bob) = (h.person("ada@example.com"), h.person("bob@example.com"));
  let (ada_peer, first, _) = h.match_up(&ada).await;
  h.silence(&ada_peer).await;
  let (_, room, role) = h.match_up(&bob).await;
  assert_eq!(role, "host");
  assert_ne!(room, first);
  // Ada's page, if it was only asleep, learns its seat is gone and starts over.
  let (status, _) = h.inbox(&ada, &ada_peer).await;
  assert_eq!(status, StatusCode::GONE);
  h.clean_up().await;
}

#[tokio::test]
async fn when_a_partner_leaves_the_other_keeps_the_room_for_the_next_arrival() {
  let Some(h) = harness().await else { return };
  let (ada, bob, cat) = (
    h.person("ada@example.com"),
    h.person("bob@example.com"),
    h.person("cat@example.com"),
  );
  let (ada_peer, room, _) = h.match_up(&ada).await;
  let (bob_peer, _, _) = h.match_up(&bob).await;
  let (status, _) = h
    .post(&bob, "/api/leave", json!({ "peer": bob_peer }))
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  assert_eq!(h.arrived(&ada, &ada_peer).await, ["joined", "left"]);
  let (status, reopened) = h
    .post(&ada, "/api/reopen", json!({ "peer": ada_peer }))
    .await;
  assert_eq!(
    (status, text(&reopened["status"])),
    (StatusCode::OK, "waiting".to_owned())
  );
  let (_, again, role) = h.match_up(&cat).await;
  assert_eq!((again, role.as_str()), (room, "guest"));
  assert_eq!(h.arrived(&ada, &ada_peer).await, ["joined"]);
  // Bob's seat is gone; leaving again is harmless.
  let (status, _) = h
    .post(&bob, "/api/leave", json!({ "peer": bob_peer }))
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  h.clean_up().await;
}

#[tokio::test]
async fn a_reopened_room_takes_in_someone_already_waiting_alone() {
  let Some(h) = harness().await else { return };
  let (ada, bob, cat, dan) = (
    h.person("ada@example.com"),
    h.person("bob@example.com"),
    h.person("cat@example.com"),
    h.person("dan@example.com"),
  );
  let (ada_peer, room, _) = h.match_up(&ada).await;
  let (bob_peer, _, _) = h.match_up(&bob).await;
  let (cat_peer, cats_room, role) = h.match_up(&cat).await;
  assert_eq!(role, "host");
  // Bob's connection drops without a goodbye; Ada keeps the room, and Cat,
  // who was waiting alone, is moved in with her rather than both waiting.
  let (status, reopened) = h
    .post(&ada, "/api/reopen", json!({ "peer": ada_peer }))
    .await;
  assert_eq!(
    (status, text(&reopened["status"])),
    (StatusCode::OK, "paired".to_owned())
  );
  assert_eq!(h.arrived(&cat, &cat_peer).await, ["moved"]);
  let (status, _) = h
    .post(
      &cat,
      "/api/signal",
      json!({ "peer": cat_peer, "data": { "type": "offer" } }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  assert_eq!(h.arrived(&ada, &ada_peer).await, ["joined", "signal"]);
  let (status, _) = h.inbox(&bob, &bob_peer).await;
  assert_eq!(
    status,
    StatusCode::GONE,
    "the dropped partner's seat is given up"
  );
  // Cat's old room is gone, so the next arrival waits in a new one.
  let (_, next, role) = h.match_up(&dan).await;
  assert_eq!(role, "host");
  assert!(next != room && next != cats_room);
  h.clean_up().await;
}

#[tokio::test]
async fn an_invite_needs_the_host_to_admit_and_the_admitted_come_back_freely() {
  let Some(h) = harness().await else { return };
  let (hana, jo, kim, lee) = (
    h.person("hana@example.com"),
    h.person("jo@example.com"),
    h.person("kim@example.com"),
    h.person("lee@example.com"),
  );
  let (status, seat) = h.post(&hana, "/api/invite", json!({})).await;
  assert_eq!(
    (status, text(&seat["role"])),
    (StatusCode::OK, "host".to_owned())
  );
  let (hana_peer, room) = (text(&seat["peer"]), text(&seat["room"]));

  let (status, knocked) = h
    .post(
      &jo,
      "/api/knock",
      json!({ "room": room, "nickname": "  Jo  " }),
    )
    .await;
  assert_eq!(
    (status, text(&knocked["status"])),
    (StatusCode::OK, "waiting".to_owned())
  );
  let jo_peer = text(&knocked["peer"]);
  let (_, kim_knock) = h
    .post(
      &kim,
      "/api/knock",
      json!({ "room": room, "nickname": "Kim" }),
    )
    .await;
  let kim_peer = text(&kim_knock["peer"]);
  let (_, messages) = h.inbox(&hana, &hana_peer).await;
  assert_eq!(messages.len(), 2);
  assert_eq!(text(&messages[0]["type"]), "knock");
  assert_eq!(
    text(&messages[0]["email"]),
    "jo@example.com",
    "the host sees who is asking"
  );
  assert_eq!(text(&messages[0]["nickname"]), "Jo");
  let jo_knock = text(&messages[0]["knock"]);

  let (status, result) = h
    .post(
      &hana,
      "/api/admit",
      json!({ "peer": hana_peer, "knock": jo_knock, "admit": true }),
    )
    .await;
  assert_eq!(
    (status, text(&result["result"])),
    (StatusCode::OK, "admitted".to_owned())
  );
  assert_eq!(h.arrived(&jo, &jo_peer).await, ["admitted"]);
  let (_, messages) = h.inbox(&kim, &kim_peer).await;
  assert_eq!(messages, [json!({ "type": "denied", "reason": "full" })]);
  let (status, _) = h
    .post(
      &lee,
      "/api/knock",
      json!({ "room": room, "nickname": "Lee" }),
    )
    .await;
  assert_eq!(
    status,
    StatusCode::CONFLICT,
    "a full room turns knocks away"
  );
  // Admitting the same knock twice does nothing.
  let (status, _) = h
    .post(
      &hana,
      "/api/admit",
      json!({ "peer": hana_peer, "knock": jo_knock, "admit": true }),
    )
    .await;
  assert_eq!(status, StatusCode::GONE);

  let (status, _) = h
    .post(
      &jo,
      "/api/signal",
      json!({ "peer": jo_peer, "data": { "type": "offer" } }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  assert_eq!(h.arrived(&hana, &hana_peer).await, ["signal"]);

  // Jo's connection drops; coming back, Jo walks straight in.
  let (status, reopened) = h
    .post(&hana, "/api/reopen", json!({ "peer": hana_peer }))
    .await;
  assert_eq!(
    (status, text(&reopened["status"])),
    (StatusCode::OK, "waiting".to_owned())
  );
  let (status, knocked) = h
    .post(&jo, "/api/knock", json!({ "room": room, "nickname": "Jo" }))
    .await;
  assert_eq!(
    (status, text(&knocked["status"])),
    (StatusCode::OK, "admitted".to_owned())
  );
  assert_eq!(h.arrived(&hana, &hana_peer).await, ["joined"]);

  // Match never hands out an invite room.
  let (_, other, role) = h.match_up(&lee).await;
  assert_eq!(role, "host");
  assert_ne!(other, room);
  h.clean_up().await;
}

#[tokio::test]
async fn a_host_who_says_no_is_heard_and_a_closed_room_turns_knocks_away() {
  let Some(h) = harness().await else { return };
  let (hana, jo, kim) = (
    h.person("hana@example.com"),
    h.person("jo@example.com"),
    h.person("kim@example.com"),
  );
  let (_, seat) = h.post(&hana, "/api/invite", json!({})).await;
  let (hana_peer, room) = (text(&seat["peer"]), text(&seat["room"]));
  let (_, knocked) = h
    .post(&jo, "/api/knock", json!({ "room": room, "nickname": "Jo" }))
    .await;
  let jo_peer = text(&knocked["peer"]);
  let (_, messages) = h.inbox(&hana, &hana_peer).await;
  let knock = text(&messages[0]["knock"]);
  let (status, result) = h
    .post(
      &hana,
      "/api/admit",
      json!({ "peer": hana_peer, "knock": knock, "admit": false }),
    )
    .await;
  assert_eq!(
    (status, text(&result["result"])),
    (StatusCode::OK, "denied".to_owned())
  );
  assert_eq!(h.arrived(&jo, &jo_peer).await, ["denied"]);

  // A knock still waiting when the host leaves is told the room has ended.
  let (_, knocked) = h
    .post(
      &kim,
      "/api/knock",
      json!({ "room": room, "nickname": "Kim" }),
    )
    .await;
  let kim_peer = text(&knocked["peer"]);
  let (status, _) = h
    .post(&hana, "/api/leave", json!({ "peer": hana_peer }))
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (_, messages) = h.inbox(&kim, &kim_peer).await;
  assert_eq!(messages, [json!({ "type": "denied", "reason": "ended" })]);
  let (status, _) = h
    .post(&jo, "/api/knock", json!({ "room": room, "nickname": "Jo" }))
    .await;
  assert_eq!(status, StatusCode::GONE);
  let (status, _) = h
    .post(
      &jo,
      "/api/knock",
      json!({ "room": "not-a-room", "nickname": "Jo" }),
    )
    .await;
  assert_eq!(status, StatusCode::GONE);
  let (status, _) = h
    .post(
      &jo,
      "/api/knock",
      json!({ "room": room, "nickname": "   " }),
    )
    .await;
  assert_eq!(status, StatusCode::BAD_REQUEST);
  h.clean_up().await;
}

#[tokio::test]
async fn an_invite_waits_for_its_host_and_survives_them_reloading() {
  let Some(h) = harness().await else { return };
  let (hana, jo) = (h.person("hana@example.com"), h.person("jo@example.com"));
  let (_, seat) = h.post(&hana, "/api/invite", json!({})).await;
  let (hana_peer, room) = (text(&seat["peer"]), text(&seat["room"]));
  // Hana's phone has paused the page while she sends the link: Jo's knock waits for her.
  h.silence(&hana_peer).await;
  let (status, knocked) = h
    .post(&jo, "/api/knock", json!({ "room": room, "nickname": "Jo" }))
    .await;
  assert_eq!(
    (status, text(&knocked["status"])),
    (StatusCode::OK, "waiting".to_owned())
  );
  let jo_peer = text(&knocked["peer"]);

  // Hana reloads. Alone in her room, her closing page keeps it...
  let (status, _) = h
    .post(
      &hana,
      "/api/leave",
      json!({ "peer": hana_peer, "keep": true }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  // ...and the page that opens on her link takes it back, with Jo still knocking.
  let (status, back) = h
    .post(
      &hana,
      "/api/knock",
      json!({ "room": room, "nickname": "Hana" }),
    )
    .await;
  assert_eq!(
    (status, text(&back["status"])),
    (StatusCode::OK, "host".to_owned())
  );
  let hana_peer = text(&back["peer"]);
  let (_, messages) = h.inbox(&hana, &hana_peer).await;
  assert_eq!(messages.len(), 1, "{messages:?}");
  assert_eq!(
    (text(&messages[0]["type"]), text(&messages[0]["email"])),
    ("knock".to_owned(), "jo@example.com".to_owned())
  );
  let knock = text(&messages[0]["knock"]);
  let (_, result) = h
    .post(
      &hana,
      "/api/admit",
      json!({ "peer": hana_peer, "knock": knock, "admit": true }),
    )
    .await;
  assert_eq!(text(&result["result"]), "admitted");
  assert_eq!(h.arrived(&jo, &jo_peer).await, ["admitted"]);

  // Keeping is only for a host who is alone: with Jo in, Hana's closing page hands Jo the room,
  let (status, _) = h
    .post(
      &hana,
      "/api/leave",
      json!({ "peer": hana_peer, "keep": true }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  assert_eq!(h.arrived(&jo, &jo_peer).await, ["left"]);
  // and Hana, let in from the start, comes back as Jo's guest.
  let (_, again) = h
    .post(
      &hana,
      "/api/knock",
      json!({ "room": room, "nickname": "Hana" }),
    )
    .await;
  assert_eq!(text(&again["status"]), "admitted");
  h.clean_up().await;
}

#[tokio::test]
async fn a_host_opening_their_own_link_in_another_tab_joins_as_a_guest() {
  let Some(h) = harness().await else { return };
  let hana = h.person("hana@example.com");
  let (_, seat) = h.post(&hana, "/api/invite", json!({})).await;
  let (hana_peer, room) = (text(&seat["peer"]), text(&seat["room"]));
  // Her first tab is still here, so the second one doesn't take the room from it.
  let (status, second) = h
    .post(
      &hana,
      "/api/knock",
      json!({ "room": room, "nickname": "Hana too" }),
    )
    .await;
  assert_eq!(
    (status, text(&second["status"])),
    (StatusCode::OK, "admitted".to_owned())
  );
  assert_eq!(h.arrived(&hana, &hana_peer).await, ["joined"]);
  // A match room isn't kept by a closing page.
  let ada = h.person("ada@example.com");
  let (ada_peer, first, _) = h.match_up(&ada).await;
  let (status, _) = h
    .post(
      &ada,
      "/api/leave",
      json!({ "peer": ada_peer, "keep": true }),
    )
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (_, next, role) = h.match_up(&h.person("bob@example.com")).await;
  assert_eq!(role, "host");
  assert_ne!(next, first);
  h.clean_up().await;
}

#[tokio::test]
async fn a_knocker_who_gives_up_is_withdrawn() {
  let Some(h) = harness().await else { return };
  let (hana, jo) = (h.person("hana@example.com"), h.person("jo@example.com"));
  let (_, seat) = h.post(&hana, "/api/invite", json!({})).await;
  let (hana_peer, room) = (text(&seat["peer"]), text(&seat["room"]));
  let (_, knocked) = h
    .post(&jo, "/api/knock", json!({ "room": room, "nickname": "Jo" }))
    .await;
  let (_, messages) = h.inbox(&hana, &hana_peer).await;
  let knock = text(&messages[0]["knock"]);
  let (status, _) = h
    .post(&jo, "/api/leave", json!({ "peer": text(&knocked["peer"]) }))
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (_, messages) = h.inbox(&hana, &hana_peer).await;
  assert_eq!(
    messages,
    [json!({ "type": "knock-cancelled", "knock": knock })]
  );
  let (status, _) = h
    .post(
      &hana,
      "/api/admit",
      json!({ "peer": hana_peer, "knock": knock, "admit": true }),
    )
    .await;
  assert_eq!(status, StatusCode::GONE);
  h.clean_up().await;
}

#[tokio::test]
async fn a_peer_answers_only_to_the_person_it_belongs_to() {
  let Some(h) = harness().await else { return };
  let (ada, bob) = (h.person("ada@example.com"), h.person("bob@example.com"));
  let (ada_peer, _, _) = h.match_up(&ada).await;
  let (status, _) = h.inbox(&bob, &ada_peer).await;
  assert_eq!(status, StatusCode::GONE);
  let (status, _) = h
    .post(&bob, "/api/reopen", json!({ "peer": ada_peer }))
    .await;
  assert_eq!(status, StatusCode::GONE);
  let (status, _) = h
    .post(&bob, "/api/leave", json!({ "peer": ada_peer }))
    .await;
  assert_eq!(status, StatusCode::NO_CONTENT);
  let (status, _) = h.inbox(&ada, &ada_peer).await;
  assert_eq!(
    status,
    StatusCode::OK,
    "someone else's leave changes nothing"
  );
  let (status, _) = h
    .post(&ada, "/api/signal", json!({ "peer": ada_peer, "data": {} }))
    .await;
  assert_eq!(
    status,
    StatusCode::CONFLICT,
    "alone, there is nobody to signal"
  );
  let big = "x".repeat(40 * 1024);
  let (status, _) = h
    .post(
      &ada,
      "/api/signal",
      json!({ "peer": ada_peer, "data": big }),
    )
    .await;
  assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
  h.clean_up().await;
}
