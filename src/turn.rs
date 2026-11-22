//! The ICE servers a browser connects through. STUN finds each browser's
//! public address, which is enough for most pairs to connect directly. Some
//! networks (strict firewalls, some mobile carriers) block that, and need a
//! TURN relay. `TURN_PROVIDER` says whose, one at a time:
//!
//! | `TURN_PROVIDER` | Also needs | What happens on each `/api/ice` |
//! |---|---|---|
//! | `cloudflare` | `TURN_KEY_ID`, `TURN_KEY_API_TOKEN` | mints credentials that last a day; the key never leaves the server |
//! | `metered` | `METERED_APP`, `METERED_API_KEY` | fetches the server list of one credential made in Metered's dashboard |
//!
//! Unset, or `none`, there is no relay and browsers get STUN only. So they do
//! when the provider is misconfigured, or doesn't answer, or its quota is
//! gone: a relay that fails never takes the app down with it.

use std::time::Duration;

use serde_json::{Value, json};

pub const PROVIDER_VAR: &str = "TURN_PROVIDER";
pub const KEY_ID_VAR: &str = "TURN_KEY_ID";
pub const TOKEN_VAR: &str = "TURN_KEY_API_TOKEN";
pub const METERED_APP_VAR: &str = "METERED_APP";
pub const METERED_KEY_VAR: &str = "METERED_API_KEY";

/// How long minted Cloudflare credentials last.
const CREDENTIAL_TTL_SECONDS: u64 = 24 * 60 * 60;

/// How long to wait for a provider before giving up and answering STUN only.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A TURN provider that is set up. Each holds the whole endpoint it calls, so
/// a test can point one at a server of its own.
pub enum Turn {
  Cloudflare { url: String, token: String },
  Metered { url: String, api_key: String },
}

impl Turn {
  /// The provider `TURN_PROVIDER` names, if it and its settings are in the
  /// environment. Says why on stderr when they're not.
  pub fn from_env() -> Option<Turn> {
    Turn::from_vars(|name| std::env::var(name).ok())
  }

  fn from_vars(var: impl Fn(&str) -> Option<String>) -> Option<Turn> {
    let get = |name: &str| {
      var(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    };
    let provider = get(PROVIDER_VAR)?.to_lowercase();
    let (made, needs) = match provider.as_str() {
      "none" => return None,
      "cloudflare" => (
        Turn::cloudflare(get(KEY_ID_VAR), get(TOKEN_VAR)),
        format!("{KEY_ID_VAR} and {TOKEN_VAR}"),
      ),
      "metered" => (
        Turn::metered(get(METERED_APP_VAR), get(METERED_KEY_VAR)),
        format!("{METERED_APP_VAR} and {METERED_KEY_VAR}"),
      ),
      other => {
        eprintln!(
          "{PROVIDER_VAR} is {other:?}, which is not cloudflare, metered or none; browsers will get STUN only"
        );
        return None;
      }
    };
    if made.is_none() {
      eprintln!(
        "{PROVIDER_VAR} is {provider}, but {needs} are not both set and well formed; browsers will get STUN only"
      );
    }
    made
  }

  fn cloudflare(key_id: Option<String>, token: Option<String>) -> Option<Turn> {
    let key_id = key_id?;
    let fits = key_id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    fits.then_some(Turn::Cloudflare {
      url: format!(
        "https://rtc.live.cloudflare.com/v1/turn/keys/{key_id}/credentials/generate-ice-servers"
      ),
      token: token?,
    })
  }

  fn metered(app: Option<String>, api_key: Option<String>) -> Option<Turn> {
    let app = app?.to_lowercase();
    let api_key = api_key?;
    // The app name is the start of a host name, so it must be one label of
    // it and nothing that could point the request somewhere else.
    let label = app.len() <= 63
      && !app.starts_with('-')
      && !app.ends_with('-')
      && app.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (label && api_key.chars().all(|c| c.is_ascii_graphic())).then_some(Turn::Metered {
      url: format!("https://{app}.metered.live/api/v1/turn/credentials"),
      api_key,
    })
  }
}

/// Public STUN only: what a browser gets without TURN, or when the provider fails.
pub fn stun_only() -> Value {
  json!([{ "urls": ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"] }])
}

/// The `iceServers` for one room: the provider's when it's set up and
/// answering, else STUN.
pub async fn ice_servers(http: &reqwest::Client, turn: Option<&Turn>) -> Value {
  let Some(turn) = turn else {
    return stun_only();
  };
  match mint(http, turn).await {
    Ok(servers) => servers,
    Err(err) => {
      eprintln!("TURN servers failed, falling back to STUN: {err}");
      stun_only()
    }
  }
}

type Failure = Box<dyn std::error::Error + Send + Sync>;

async fn mint(http: &reqwest::Client, turn: &Turn) -> Result<Value, Failure> {
  match turn {
    Turn::Cloudflare { url, token } => {
      let request = http
        .post(url)
        .bearer_auth(token)
        .json(&json!({ "ttl": CREDENTIAL_TTL_SECONDS }));
      let answer = answer_of(request).await?;
      let servers = answer
        .get("iceServers")
        .cloned()
        .ok_or("no iceServers in the answer")?;
      Ok(without_port_53(servers))
    }
    Turn::Metered { url, api_key } => {
      let answer = answer_of(http.get(url).query(&[("apiKey", api_key)])).await?;
      Ok(metered_servers(answer)?)
    }
  }
}

/// A provider's JSON answer. A refusal comes back with the provider's own
/// words (Metered says "Invalid API Key"), which is what tells a person what
/// to fix. The message never carries the request's URL: for Metered that is
/// where the API key travels, and this message goes to the logs.
async fn answer_of(request: reqwest::RequestBuilder) -> Result<Value, Failure> {
  let response = request
    .timeout(TIMEOUT)
    .send()
    .await
    .map_err(reqwest::Error::without_url)?;
  let status = response.status();
  if !status.is_success() {
    let said = response.text().await.unwrap_or_default();
    return Err(
      format!(
        "HTTP {status}: {}",
        said.chars().take(200).collect::<String>()
      )
      .into(),
    );
  }
  Ok(response.json().await.map_err(reqwest::Error::without_url)?)
}

/// Metered answers with the `iceServers` array itself. Anything else, even
/// with a 200, is an error, not something to hand to a browser.
fn metered_servers(answer: Value) -> Result<Value, String> {
  let Some(list) = answer.as_array() else {
    let why = answer
      .get("error")
      .and_then(Value::as_str)
      .unwrap_or("not a list");
    return Err(format!("Metered did not send a server list ({why})"));
  };
  let servers = !list.is_empty()
    && list.iter().all(|server| {
      matches!(
        server.get("urls"),
        Some(Value::String(_)) | Some(Value::Array(_))
      )
    });
  if servers {
    Ok(answer)
  } else {
    Err("Metered's list is empty or has an entry without urls".to_owned())
  }
}

/// Drops TURN URLs on port 53: browsers block it, and since pages here wait
/// for every candidate before sending an offer, a URL that only times out
/// would hold up every connection.
fn without_port_53(mut servers: Value) -> Value {
  if let Some(list) = servers.as_array_mut() {
    for server in list.iter_mut() {
      match server.get_mut("urls") {
        Some(Value::Array(urls)) => urls.retain(|url| !url.as_str().is_some_and(on_port_53)),
        Some(Value::String(url)) if on_port_53(url) => {
          server["urls"] = json!([]);
        }
        _ => {}
      }
    }
    list.retain(|server| match server.get("urls") {
      Some(Value::Array(urls)) => !urls.is_empty(),
      _ => true,
    });
  }
  servers
}

fn on_port_53(url: &str) -> bool {
  let host_and_port = url.split('?').next().unwrap_or(url);
  host_and_port.ends_with(":53")
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use axum::extract::Query;
  use axum::http::{HeaderMap, StatusCode};
  use axum::routing::{get, post};
  use axum::{Json, Router};

  use super::*;

  fn settings(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
      .iter()
      .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
      .collect();
    move |name| map.get(name).cloned()
  }

  // ——— Choosing the provider ——————————————————————————————————————

  #[test]
  fn no_provider_means_no_relay_whatever_else_is_set() {
    let all_settings = [
      (KEY_ID_VAR, "key"),
      (TOKEN_VAR, "token"),
      (METERED_APP_VAR, "drawme-1998"),
      (METERED_KEY_VAR, "k"),
    ];
    assert!(Turn::from_vars(settings(&all_settings)).is_none());
    for off in ["", "  ", "none", "NONE", "mystery", "cloudflare,metered"] {
      let mut with = vec![(PROVIDER_VAR, off)];
      with.extend(all_settings);
      assert!(Turn::from_vars(settings(&with)).is_none(), "{off:?}");
    }
  }

  #[test]
  fn the_named_provider_is_the_one_used_even_with_both_set_up() {
    let both = [
      (KEY_ID_VAR, "abc-123"),
      (TOKEN_VAR, "tok"),
      (METERED_APP_VAR, "drawme-1998"),
      (METERED_KEY_VAR, "k3y"),
    ];
    let with = |provider| {
      let mut all = vec![(PROVIDER_VAR, provider)];
      all.extend(both);
      Turn::from_vars(settings(&all))
    };
    let Some(Turn::Metered { url, api_key }) = with(" Metered ") else {
      panic!("metered was named");
    };
    assert_eq!(
      url,
      "https://drawme-1998.metered.live/api/v1/turn/credentials"
    );
    assert_eq!(api_key, "k3y");
    let Some(Turn::Cloudflare { url, token }) = with("CloudFlare") else {
      panic!("cloudflare was named");
    };
    assert_eq!(
      url,
      "https://rtc.live.cloudflare.com/v1/turn/keys/abc-123/credentials/generate-ice-servers"
    );
    assert_eq!(token, "tok");
  }

  #[test]
  fn a_provider_without_all_its_settings_is_no_relay() {
    let cases: [&[(&str, &str)]; 6] = [
      &[(PROVIDER_VAR, "metered")],
      &[(PROVIDER_VAR, "metered"), (METERED_APP_VAR, "drawme-1998")],
      &[(PROVIDER_VAR, "metered"), (METERED_KEY_VAR, "k")],
      &[(PROVIDER_VAR, "cloudflare"), (KEY_ID_VAR, "abc")],
      &[(PROVIDER_VAR, "cloudflare"), (TOKEN_VAR, "tok")],
      // The other provider's settings don't count.
      &[
        (PROVIDER_VAR, "metered"),
        (KEY_ID_VAR, "abc"),
        (TOKEN_VAR, "tok"),
      ],
    ];
    for case in cases {
      assert!(Turn::from_vars(settings(case)).is_none(), "{case:?}");
    }
  }

  #[test]
  fn a_metered_app_name_can_only_be_one_host_name_label() {
    let build = |app: &str| {
      Turn::from_vars(settings(&[
        (PROVIDER_VAR, "metered"),
        (METERED_APP_VAR, app),
        (METERED_KEY_VAR, "k"),
      ]))
    };
    for bad in [
      "a.b",
      "evil.example/x",
      "a/b",
      "a@b",
      "a b",
      "a:80",
      "a#b",
      "a?b",
      "-a",
      "a-",
      &"x".repeat(64),
    ] {
      assert!(build(bad).is_none(), "{bad:?}");
    }
    assert!(build(&"x".repeat(63)).is_some());
    let Some(Turn::Metered { url, .. }) = build("Drawme-1998") else {
      panic!("a plain name is fine");
    };
    assert_eq!(
      url,
      "https://drawme-1998.metered.live/api/v1/turn/credentials"
    );
  }

  #[test]
  fn a_cloudflare_key_id_stays_in_its_path() {
    let build = |id: &str| {
      Turn::from_vars(settings(&[
        (PROVIDER_VAR, "cloudflare"),
        (KEY_ID_VAR, id),
        (TOKEN_VAR, "t"),
      ]))
    };
    for bad in ["a/b", "a b", "a?b", "../x", "a#b"] {
      assert!(build(bad).is_none(), "{bad:?}");
    }
    assert!(build("abc-123_X").is_some());
  }

  // ——— What the providers send ——————————————————————————————————————

  fn metered_sample() -> Value {
    json!([
      { "urls": "stun:stun.relay.metered.ca:80" },
      { "urls": "turn:global.relay.metered.ca:80", "username": "u", "credential": "c" },
      { "urls": "turn:global.relay.metered.ca:80?transport=tcp", "username": "u", "credential": "c" },
      { "urls": "turn:global.relay.metered.ca:443", "username": "u", "credential": "c" },
      { "urls": "turns:global.relay.metered.ca:443?transport=tcp", "username": "u", "credential": "c" }
    ])
  }

  #[test]
  fn metered_lists_are_checked_before_a_browser_sees_them() {
    assert_eq!(metered_servers(metered_sample()).unwrap(), metered_sample());
    // Urls may be a string or a list, as RTCPeerConnection allows.
    assert!(metered_servers(json!([{ "urls": ["stun:a:80"] }])).is_ok());
    for bad in [
      json!({ "error": "Invalid API Key" }),
      json!([]),
      json!([{ "username": "u" }]),
      json!([{ "urls": 5 }]),
      json!("hello"),
      json!(null),
    ] {
      assert!(metered_servers(bad.clone()).is_err(), "{bad}");
    }
    let why = metered_servers(json!({ "error": "Invalid API Key" })).unwrap_err();
    assert!(why.contains("Invalid API Key"), "{why}");
  }

  #[test]
  fn port_53_urls_are_dropped_and_empty_servers_with_them() {
    let servers = json!([
        { "urls": ["stun:stun.cloudflare.com:3478", "stun:stun.cloudflare.com:53"] },
        {
            "urls": [
                "turn:turn.cloudflare.com:3478?transport=udp",
                "turn:turn.cloudflare.com:53?transport=udp",
                "turns:turn.cloudflare.com:5349?transport=tcp"
            ],
            "username": "u",
            "credential": "c"
        },
        { "urls": "turn:turn.cloudflare.com:53?transport=tcp", "username": "u", "credential": "c" },
        { "urls": ["turn:turn.cloudflare.com:53"] }
    ]);
    assert_eq!(
      without_port_53(servers),
      json!([
          { "urls": ["stun:stun.cloudflare.com:3478"] },
          {
              "urls": [
                  "turn:turn.cloudflare.com:3478?transport=udp",
                  "turns:turn.cloudflare.com:5349?transport=tcp"
              ],
              "username": "u",
              "credential": "c"
          }
      ])
    );
  }

  #[test]
  fn port_5349_is_not_port_53() {
    assert!(!on_port_53("turns:turn.cloudflare.com:5349?transport=tcp"));
    assert!(on_port_53("turn:turn.cloudflare.com:53?transport=udp"));
  }

  // ——— The calls themselves, against servers of our own ——————————————————

  /// Serves `router` on a free local port, and gives its address.
  async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
  }

  fn fake_metered() -> Router {
    Router::new().route(
      "/api/v1/turn/credentials",
      get(|Query(query): Query<HashMap<String, String>>| async move {
        if query.get("apiKey").map(String::as_str) == Some("good key") {
          (StatusCode::OK, Json(metered_sample()))
        } else {
          (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid API Key" })),
          )
        }
      }),
    )
  }

  fn metered_at(base: &str, api_key: &str) -> Turn {
    Turn::Metered {
      url: format!("{base}/api/v1/turn/credentials"),
      api_key: api_key.to_owned(),
    }
  }

  #[tokio::test]
  async fn metered_servers_reach_the_browser_as_they_are() {
    let base = serve(fake_metered()).await;
    let servers = ice_servers(
      &reqwest::Client::new(),
      Some(&metered_at(&base, "good key")),
    )
    .await;
    // The key has a space in it, which the request must encode, not break on.
    assert_eq!(servers, metered_sample());
  }

  #[tokio::test]
  async fn a_refusing_or_broken_provider_means_stun_only() {
    let http = reqwest::Client::new();
    let base = serve(fake_metered()).await;
    // Wrong key: Metered answers 401.
    let wrong = ice_servers(&http, Some(&metered_at(&base, "bad key"))).await;
    assert_eq!(wrong, stun_only());
    // A 200 that isn't a list, an empty list, and not JSON at all.
    for body in [
      json!({ "error": "quota" }).to_string(),
      json!([]).to_string(),
      "<html>maintenance</html>".to_owned(),
    ] {
      let router = Router::new().route(
        "/api/v1/turn/credentials",
        get(move || {
          let body = body.clone();
          async move { ([("content-type", "application/json")], body) }
        }),
      );
      let base = serve(router).await;
      let got = ice_servers(&http, Some(&metered_at(&base, "good key"))).await;
      assert_eq!(got, stun_only());
    }
    // Nothing listening at all.
    let closed = {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      format!("http://{}", listener.local_addr().unwrap())
    };
    let got = ice_servers(&http, Some(&metered_at(&closed, "good key"))).await;
    assert_eq!(got, stun_only());
    // And no provider.
    assert_eq!(ice_servers(&http, None).await, stun_only());
  }

  #[tokio::test]
  async fn a_failure_says_what_the_provider_said_and_never_includes_the_key() {
    let http = reqwest::Client::new();
    let base = serve(fake_metered()).await;
    let refused = mint(&http, &metered_at(&base, "bad key"))
      .await
      .unwrap_err()
      .to_string();
    assert!(refused.contains("401"), "{refused}");
    assert!(refused.contains("Invalid API Key"), "{refused}");
    // Nothing listening: the message must not carry the URL, and the key is
    // in the URL's query.
    let closed = {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      format!("http://{}", listener.local_addr().unwrap())
    };
    let unreachable = mint(&http, &metered_at(&closed, "bad key"))
      .await
      .unwrap_err()
      .to_string();
    for message in [&refused, &unreachable] {
      for secret in ["apiKey", "bad key", "bad+key", "bad%20key", "127.0.0.1"] {
        assert!(!message.contains(secret), "{secret:?} in {message}");
      }
    }
  }

  #[tokio::test]
  async fn cloudflare_is_asked_with_its_token_and_a_day_and_port_53_is_dropped() {
    let router = Router::new().route(
      "/mint",
      post(|headers: HeaderMap, Json(body): Json<Value>| async move {
        let bearer = headers
          .get("authorization")
          .and_then(|value| value.to_str().ok());
        if bearer != Some("Bearer secret token") || body != json!({ "ttl": 86400 }) {
          return (StatusCode::FORBIDDEN, Json(json!({})));
        }
        (
          StatusCode::CREATED,
          Json(json!({ "iceServers": [
            { "urls": ["stun:stun.cloudflare.com:3478", "stun:stun.cloudflare.com:53"] },
            { "urls": ["turn:turn.cloudflare.com:3478?transport=udp", "turn:turn.cloudflare.com:53?transport=udp"],
              "username": "u", "credential": "c" }
          ] })),
        )
      }),
    );
    let base = serve(router).await;
    let http = reqwest::Client::new();
    let turn = Turn::Cloudflare {
      url: format!("{base}/mint"),
      token: "secret token".to_owned(),
    };
    assert_eq!(
      ice_servers(&http, Some(&turn)).await,
      json!([
        { "urls": ["stun:stun.cloudflare.com:3478"] },
        { "urls": ["turn:turn.cloudflare.com:3478?transport=udp"], "username": "u", "credential": "c" }
      ])
    );
    let wrong = Turn::Cloudflare {
      url: format!("{base}/mint"),
      token: "other".to_owned(),
    };
    assert_eq!(ice_servers(&http, Some(&wrong)).await, stun_only());
  }
}
