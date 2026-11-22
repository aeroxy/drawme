//! The ICE servers a browser connects through. STUN finds each browser's
//! public address, which is enough for most pairs to connect directly. Some
//! networks (strict firewalls, some mobile carriers) block that, and need a
//! TURN relay: Cloudflare's, when `TURN_KEY_ID` and `TURN_KEY_API_TOKEN` are
//! set. Credentials are minted per room and last a day; the key itself never
//! leaves the server.

use serde_json::{Value, json};

pub const KEY_ID_VAR: &str = "TURN_KEY_ID";
pub const TOKEN_VAR: &str = "TURN_KEY_API_TOKEN";

/// How long minted TURN credentials last.
const CREDENTIAL_TTL_SECONDS: u64 = 24 * 60 * 60;

pub struct Turn {
  key_id: String,
  token: String,
}

impl Turn {
  /// Cloudflare's TURN key from the environment, if both parts are set.
  pub fn from_env() -> Option<Turn> {
    let key_id = std::env::var(KEY_ID_VAR).ok()?.trim().to_owned();
    let token = std::env::var(TOKEN_VAR).ok()?.trim().to_owned();
    let fits = !key_id.is_empty()
      && key_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (fits && !token.is_empty()).then_some(Turn { key_id, token })
  }
}

/// Public STUN only: what a browser gets without TURN, or when minting fails.
pub fn stun_only() -> Value {
  json!([{ "urls": ["stun:stun.cloudflare.com:3478", "stun:stun.l.google.com:19302"] }])
}

/// The `iceServers` for one room: TURN when it's set up and answering, else STUN.
pub async fn ice_servers(http: &reqwest::Client, turn: Option<&Turn>) -> Value {
  let Some(turn) = turn else {
    return stun_only();
  };
  match mint(http, turn).await {
    Ok(servers) => servers,
    Err(err) => {
      eprintln!("TURN credentials failed, falling back to STUN: {err}");
      stun_only()
    }
  }
}

async fn mint(
  http: &reqwest::Client,
  turn: &Turn,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
  let url = format!(
    "https://rtc.live.cloudflare.com/v1/turn/keys/{}/credentials/generate-ice-servers",
    turn.key_id
  );
  let answer: Value = http
    .post(url)
    .bearer_auth(&turn.token)
    .json(&json!({ "ttl": CREDENTIAL_TTL_SECONDS }))
    .timeout(std::time::Duration::from_secs(5))
    .send()
    .await?
    .error_for_status()?
    .json()
    .await?;
  let servers = answer
    .get("iceServers")
    .cloned()
    .ok_or("no iceServers in the answer")?;
  Ok(without_port_53(servers))
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
  use super::*;

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
}
