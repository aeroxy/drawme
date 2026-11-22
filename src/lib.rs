//! drawme's API: signing in with Aero, and the matchmaking and handshake
//! that put two browsers in touch. Drawing and voice then go directly
//! between the browsers over WebRTC; nothing here carries them.

pub mod auth;
pub mod rooms;
pub mod turn;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Form, FromRequestParts, Query, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use auth::{Person, Verifier};
pub use rooms::{RoomError, Rooms};
pub use turn::Turn;

/// Holds the Aero token. HttpOnly, so no script on the page can read it,
/// and sent with every same-site request, `sendBeacon` included.
pub const SESSION_COOKIE: &str = "drawme_session";
/// Holds a sign-in's `state` and where to come back to, between leaving for
/// Aero and its answer.
pub const SIGN_IN_COOKIE: &str = "drawme_sign_in";

/// How long a sign-in may take at Aero's consent screen.
const SIGN_IN_SECONDS: u64 = 10 * 60;
/// Nicknames run to this many characters. They're only shown to the other
/// person in a room, and live in the browser that chose them.
pub const NICKNAME_MAX: usize = 24;
/// A WebRTC offer or answer, with every candidate, fits easily.
const SIGNAL_MAX_BYTES: usize = 32 * 1024;

pub struct Options {
  /// `Secure` on cookies: on for the deployment, off for plain-http localhost.
  pub secure_cookies: bool,
  /// The dev server's stand-in for Aero's consent screen: sign in as any
  /// email. Never on for the deployment.
  pub dev_sign_in: bool,
}

#[derive(Clone)]
struct AppState {
  rooms: Rooms,
  verifier: Arc<Verifier>,
  turn: Option<Arc<Turn>>,
  http: reqwest::Client,
  options: Arc<Options>,
}

pub fn app(rooms: Rooms, verifier: Verifier, turn: Option<Turn>, options: Options) -> Router {
  let dev_sign_in = options.dev_sign_in;
  let state = AppState {
    rooms,
    verifier: Arc::new(verifier),
    turn: turn.map(Arc::new),
    http: reqwest::Client::new(),
    options: Arc::new(options),
  };
  let router = Router::new()
    .route("/api/sign-in", get(sign_in))
    .route("/api/session", post(session))
    .route("/api/me", get(me))
    .route("/api/sign-out", post(sign_out))
    .route("/api/ice", get(ice))
    .route("/api/match", post(match_up))
    .route("/api/invite", post(invite))
    .route("/api/knock", post(knock))
    .route("/api/admit", post(admit))
    .route("/api/inbox", get(inbox))
    .route("/api/signal", post(signal))
    .route("/api/reopen", post(reopen))
    .route("/api/leave", post(leave));
  let router = if dev_sign_in {
    router.route("/api/dev/aero", post(dev_aero))
  } else {
    router
  };
  router.with_state(state)
}

/// Whoever the session cookie names. Every room call needs one.
struct Me(Person);

impl FromRequestParts<AppState> for Me {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
    cookie(&parts.headers, SESSION_COOKIE)
      .and_then(|token| state.verifier.person(token))
      .map(Me)
      .ok_or_else(|| ApiError::Unauthorized("Sign in with Aero first.".to_owned()))
  }
}

#[derive(Debug)]
pub enum ApiError {
  BadRequest(String),
  Unauthorized(String),
  Conflict(String),
  Gone(String),
  TooLarge(String),
  Unavailable(String),
  Internal,
}

impl IntoResponse for ApiError {
  fn into_response(self) -> Response {
    let (status, message) = match self {
      ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
      ApiError::Unauthorized(message) => (StatusCode::UNAUTHORIZED, message),
      ApiError::Conflict(message) => (StatusCode::CONFLICT, message),
      ApiError::Gone(message) => (StatusCode::GONE, message),
      ApiError::TooLarge(message) => (StatusCode::PAYLOAD_TOO_LARGE, message),
      ApiError::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
      ApiError::Internal => (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Something went wrong.".to_owned(),
      ),
    };
    (status, Json(ErrorBody { error: message })).into_response()
  }
}

#[derive(Serialize)]
struct ErrorBody {
  error: String,
}

impl From<RoomError> for ApiError {
  fn from(err: RoomError) -> Self {
    match err {
      RoomError::Gone => ApiError::Gone("That room has closed.".to_owned()),
      RoomError::Full => ApiError::Conflict("That room already has two people in it.".to_owned()),
      RoomError::Alone => ApiError::Conflict("Nobody else is in the room yet.".to_owned()),
      RoomError::Redis(err) => {
        eprintln!("redis: {err}");
        ApiError::Unavailable("Rooms aren't answering right now. Try again in a moment.".to_owned())
      }
    }
  }
}

/// The value of the cookie `name`, if the request carries one.
fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
  headers
    .get_all(COOKIE)
    .iter()
    .filter_map(|value| value.to_str().ok())
    .flat_map(|value| value.split(';'))
    .find_map(|pair| {
      let (key, value) = pair.trim().split_once('=')?;
      (key == name).then_some(value)
    })
}

fn set_cookie(name: &str, value: &str, path: &str, max_age: u64, secure: bool) -> HeaderValue {
  let secure = if secure { "; Secure" } else { "" };
  let text =
    format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}");
  HeaderValue::from_str(&text).expect("cookie values here are ASCII")
}

/// Where a sign-in may return to: nothing, or a fragment such as
/// `#invite:<room>`. Never a path, so it can't point off the site.
fn valid_return(text: &str) -> bool {
  text.is_empty()
    || (text.len() <= 100
      && text.starts_with('#')
      && text[1..]
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_')))
}

/// A nickname as the person typed it, trimmed, if it's one we'll show.
fn nickname(text: &str) -> Option<&str> {
  let text = text.trim();
  let length = text.chars().count();
  ((1..=NICKNAME_MAX).contains(&length) && !text.chars().any(char::is_control)).then_some(text)
}

#[derive(Deserialize)]
struct SignInQuery {
  #[serde(default, rename = "return")]
  back: String,
}

/// Leaves for Aero's consent screen, remembering `state` and where to come
/// back to in a short-lived cookie.
async fn sign_in(
  State(app): State<AppState>,
  Query(query): Query<SignInQuery>,
) -> Result<Response, ApiError> {
  if !valid_return(&query.back) {
    return Err(ApiError::BadRequest(
      "A sign-in can only come back to this page.".to_owned(),
    ));
  }
  let state = rooms::new_id();
  let remember = format!("{state}|{}", query.back);
  let cookie = set_cookie(
    SIGN_IN_COOKIE,
    &remember,
    "/api",
    SIGN_IN_SECONDS,
    app.options.secure_cookies,
  );
  if app.options.dev_sign_in {
    return Ok(([(SET_COOKIE, cookie)], Html(dev_sign_in_page(&state))).into_response());
  }
  let url = app
    .verifier
    .sign_in_url(&state)
    .ok_or_else(|| ApiError::Unavailable("Sign-in isn't set up yet.".to_owned()))?;
  Ok(([(SET_COOKIE, cookie)], Redirect::to(&url)).into_response())
}

#[derive(Deserialize)]
struct SessionBody {
  token: String,
  state: String,
}

/// Aero's answer, which the callback page passes on: keeps the token in the
/// session cookie if this browser started the sign-in (its `state` matches),
/// which stops someone pushing their own sign-in into another's browser.
async fn session(
  State(app): State<AppState>,
  headers: HeaderMap,
  Json(body): Json<SessionBody>,
) -> Result<Response, ApiError> {
  let started_here =
    || ApiError::BadRequest("This sign-in didn't start in this browser. Try again.".to_owned());
  let remembered = cookie(&headers, SIGN_IN_COOKIE).ok_or_else(started_here)?;
  let (state, back) = remembered.split_once('|').unwrap_or((remembered, ""));
  if body.state != state || !valid_return(back) {
    return Err(started_here());
  }
  let person = app.verifier.person(&body.token).ok_or_else(|| {
    ApiError::Unauthorized("Aero's answer didn't check out. Try again.".to_owned())
  })?;
  let secure = app.options.secure_cookies;
  let mut response = Json(json!({ "email": person.email, "return": back })).into_response();
  let headers = response.headers_mut();
  headers.append(
    SET_COOKIE,
    set_cookie(
      SESSION_COOKIE,
      &body.token,
      "/",
      auth::SESSION_TTL_SECONDS,
      secure,
    ),
  );
  headers.append(
    SET_COOKIE,
    set_cookie(SIGN_IN_COOKIE, "", "/api", 0, secure),
  );
  Ok(response)
}

async fn me(Me(person): Me) -> Json<Value> {
  Json(json!({ "email": person.email }))
}

async fn sign_out(State(app): State<AppState>) -> Response {
  let cookie = set_cookie(SESSION_COOKIE, "", "/", 0, app.options.secure_cookies);
  (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie)]).into_response()
}

async fn ice(State(app): State<AppState>, Me(_): Me) -> Json<Value> {
  Json(json!({ "iceServers": turn::ice_servers(&app.http, app.turn.as_deref()).await }))
}

async fn match_up(
  State(app): State<AppState>,
  Me(person): Me,
) -> Result<Json<rooms::Seat>, ApiError> {
  Ok(Json(app.rooms.match_up(&person.id).await?))
}

async fn invite(
  State(app): State<AppState>,
  Me(person): Me,
) -> Result<Json<rooms::Seat>, ApiError> {
  Ok(Json(app.rooms.invite(&person.id).await?))
}

#[derive(Deserialize)]
struct KnockBody {
  room: String,
  nickname: String,
}

async fn knock(
  State(app): State<AppState>,
  Me(person): Me,
  Json(body): Json<KnockBody>,
) -> Result<Json<Value>, ApiError> {
  let name = nickname(&body.nickname).ok_or_else(|| {
    ApiError::BadRequest(format!("A nickname is 1 to {NICKNAME_MAX} characters."))
  })?;
  let (peer, status) = app
    .rooms
    .knock(&person.id, &body.room, &person.email, name)
    .await?;
  Ok(Json(
    json!({ "peer": peer, "room": body.room, "status": status }),
  ))
}

#[derive(Deserialize)]
struct AdmitBody {
  peer: String,
  knock: String,
  admit: bool,
}

async fn admit(
  State(app): State<AppState>,
  Me(person): Me,
  Json(body): Json<AdmitBody>,
) -> Result<Json<Value>, ApiError> {
  let result = app
    .rooms
    .admit(&person.id, &body.peer, &body.knock, body.admit)
    .await?;
  Ok(Json(json!({ "result": result })))
}

#[derive(Deserialize)]
struct PeerQuery {
  peer: String,
}

/// What has arrived for a peer. Asking also tells the server it's still here.
async fn inbox(
  State(app): State<AppState>,
  Me(person): Me,
  Query(query): Query<PeerQuery>,
) -> Result<Json<Value>, ApiError> {
  let messages: Vec<Value> = app
    .rooms
    .poll(&person.id, &query.peer)
    .await?
    .iter()
    .filter_map(|text| serde_json::from_str(text).ok())
    .collect();
  Ok(Json(json!({ "messages": messages })))
}

#[derive(Deserialize)]
struct SignalBody {
  peer: String,
  data: Value,
}

/// Passes a WebRTC offer or answer to the other person in the room.
async fn signal(
  State(app): State<AppState>,
  Me(person): Me,
  Json(body): Json<SignalBody>,
) -> Result<StatusCode, ApiError> {
  let message = json!({ "type": "signal", "data": body.data }).to_string();
  if message.len() > SIGNAL_MAX_BYTES {
    return Err(ApiError::TooLarge(
      "That's too big for a handshake.".to_owned(),
    ));
  }
  app.rooms.signal(&person.id, &body.peer, &message).await?;
  Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PeerBody {
  peer: String,
}

async fn reopen(
  State(app): State<AppState>,
  Me(person): Me,
  Json(body): Json<PeerBody>,
) -> Result<Json<Value>, ApiError> {
  let status = app.rooms.reopen(&person.id, &body.peer).await?;
  Ok(Json(json!({ "status": status })))
}

#[derive(Deserialize)]
struct LeaveBody {
  peer: String,
  /// Sent by a closing page: an invite host who is alone keeps the room.
  #[serde(default)]
  keep: bool,
}

/// Takes the body as bytes: a closing page sends it with `sendBeacon`, which
/// may not label it as JSON.
async fn leave(
  State(app): State<AppState>,
  Me(person): Me,
  body: Bytes,
) -> Result<StatusCode, ApiError> {
  let body: LeaveBody = serde_json::from_slice(&body)
    .map_err(|_| ApiError::BadRequest("Say which peer is leaving.".to_owned()))?;
  app.rooms.leave(&person.id, &body.peer, body.keep).await?;
  Ok(StatusCode::NO_CONTENT)
}

/// The dev server's stand-in for Aero's consent screen.
fn dev_sign_in_page(state: &str) -> String {
  format!(
    r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Sign in (local)</title>
<style>body{{font:14px system-ui,sans-serif;background:#008080;margin:0;display:grid;place-items:center;min-height:100vh}}
form{{background:#c0c0c0;padding:16px 20px;border:2px outset #fff;display:grid;gap:10px;min-width:280px}}
input,button{{font:inherit;padding:4px 6px}}</style></head>
<body><form method="post" action="/api/dev/aero">
<strong>Local sign-in</strong>
<span>Stands in for Aero on the dev server. Any email works.</span>
<input type="email" name="email" required autofocus placeholder="you@example.com" autocomplete="email">
<input type="hidden" name="state" value="{state}">
<button type="submit">Sign in</button>
</form></body></html>"#
  )
}

#[derive(Deserialize)]
struct DevAero {
  email: String,
  state: String,
}

/// Mints the token Aero would, and answers the way Aero does: back to the
/// callback with the token in the fragment.
async fn dev_aero(
  State(app): State<AppState>,
  Form(form): Form<DevAero>,
) -> Result<Response, ApiError> {
  let email = form.email.trim();
  if !email.contains('@') || email.len() > 254 || !rooms::is_id(&form.state) {
    return Err(ApiError::BadRequest("Give an email.".to_owned()));
  }
  let token = app.verifier.mint(email).ok_or(ApiError::Internal)?;
  Ok(
    Redirect::to(&format!(
      "/auth/aero#aero_identity={token}&state={}",
      form.state
    ))
    .into_response(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cookies_are_found_by_name_among_others() {
    let mut headers = HeaderMap::new();
    headers.append(
      COOKIE,
      HeaderValue::from_static("a=1; drawme_session=v4.local.abc; b=2"),
    );
    headers.append(
      COOKIE,
      HeaderValue::from_static("drawme_sign_in=s|#invite:x"),
    );
    assert_eq!(cookie(&headers, SESSION_COOKIE), Some("v4.local.abc"));
    assert_eq!(cookie(&headers, SIGN_IN_COOKIE), Some("s|#invite:x"));
    assert_eq!(cookie(&headers, "drawme"), None);
  }

  #[test]
  fn a_sign_in_comes_back_only_to_a_fragment() {
    for good in [
      "",
      "#invite:0123456789abcdef0123456789abcdef",
      "#local:x_y-z",
    ] {
      assert!(valid_return(good), "{good:?}");
    }
    for bad in [
      "/",
      "//evil.example",
      "https://evil.example",
      "#a b",
      "#a;b",
      "#a,b",
      "#a|b",
      &format!("#{}", "a".repeat(100)),
    ] {
      assert!(!valid_return(bad), "{bad:?}");
    }
  }

  #[test]
  fn nicknames_are_trimmed_and_kept_short() {
    assert_eq!(nickname("  Ada  "), Some("Ada"));
    assert_eq!(
      nickname("🎨".repeat(24).as_str()),
      Some("🎨".repeat(24).as_str())
    );
    assert_eq!(nickname(&"🎨".repeat(25)), None);
    assert_eq!(nickname("   "), None);
    assert_eq!(nickname("a\nb"), None);
  }

  #[test]
  fn cookies_are_http_only_and_secure_when_asked() {
    let value = set_cookie(SESSION_COOKIE, "t", "/", 60, true);
    assert_eq!(
      value,
      "drawme_session=t; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
    );
    let value = set_cookie(SESSION_COOKIE, "t", "/", 60, false);
    assert_eq!(
      value,
      "drawme_session=t; Path=/; Max-Age=60; HttpOnly; SameSite=Lax"
    );
  }
}
