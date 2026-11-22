//! Sign in with Aero. Identity only, and not OAuth: Aero's consent screen
//! sends the browser back to `/auth/aero` with a PASETO `v4.local` token,
//! the visitor's email encrypted with a key only Aero and drawme hold. The
//! page hands it straight to the API, which keeps it in an HttpOnly cookie
//! and decrypts it on every call.
//!
//! Nothing here may log a token's contents or an email.

use std::time::Duration;

use pasetors::claims::{Claims, ClaimsValidationRules};
use pasetors::keys::SymmetricKey;
use pasetors::token::UntrustedToken;
use pasetors::{Local, local, version4::V4};
use uuid::Uuid;

/// drawme's registration on Aero gives a client id and a key together, and
/// registering again replaces both, so both live in the backend's
/// environment. The key is a PASERK `k4.local.…` string and secret.
pub const CLIENT_ID_VAR: &str = "AERO_CLIENT_ID";
pub const KEY_VAR: &str = "AERO_LOGIN_KEY";

/// Aero's consent screen, where a sign-in starts.
const AERO_LOGIN_URL: &str = "https://aero.oops.wtf/login-with-aero";

/// How long a sign-in lasts: 30 days. The token is the session, and Aero
/// can't recall one before it runs out.
pub const SESSION_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Turns an email into a person id. Changing it would make everyone a new
/// person (and forget who a room has let in), so it never changes.
const PERSON_NAMESPACE: Uuid = Uuid::from_u128(0x6d3f2a14_9b7c_4e21_a8d5_3c0e9f71b2a6);

/// Someone signed in.
#[derive(Debug, Clone)]
pub struct Person {
  /// Stable for the same email in any case, so rooms can tell people apart
  /// without handling emails.
  pub id: Uuid,
  /// Lowercased. Shown only to a host deciding whether to let them in.
  pub email: String,
}

pub struct Verifier {
  /// None when the key is missing or unreadable: then nobody can sign in.
  key: Option<SymmetricKey<V4>>,
  /// None when missing or not base64url: the same.
  client_id: Option<String>,
}

impl Verifier {
  /// Checks tokens encrypted with the PASERK `key` (`k4.local.…`) and
  /// addressed to `client_id`.
  pub fn new(key: &str, client_id: &str) -> Result<Verifier, pasetors::errors::Error> {
    let key = SymmetricKey::<V4>::try_from(key)?;
    Ok(Verifier {
      key: Some(key),
      client_id: usable(client_id),
    })
  }

  /// The real thing, from the environment. Without both values nobody can
  /// sign in, and the API says why once.
  pub fn aero() -> Verifier {
    let (key, client_id) = from_env();
    if key.is_none() {
      eprintln!("{KEY_VAR} is missing or not a k4.local key; sign-ins will be refused");
    }
    if client_id.is_none() {
      eprintln!("{CLIENT_ID_VAR} is missing or malformed; sign-ins will be refused");
    }
    Verifier { key, client_id }
  }

  /// For the dev server and tests: the environment's key and id when set,
  /// else a fixed key of its own, so a sign-in can be minted locally and
  /// lasts across restarts. That key isn't secret and needn't be: its
  /// tokens are addressed to `drawme-dev`, which no deployment accepts, and
  /// the dev server lets anyone sign in as anyone anyway.
  pub fn local() -> Verifier {
    const DEV_KEY: &[u8; 32] = b"drawme local dev key, not secret";
    let (key, client_id) = from_env();
    let key = key.or_else(|| SymmetricKey::<V4>::from(DEV_KEY).ok());
    Verifier {
      key,
      client_id: client_id.or_else(|| Some("drawme-dev".to_owned())),
    }
  }

  /// Aero's consent screen for a new sign-in, carrying `state` back with
  /// the answer. None until both the client id and the key are set.
  /// `state` must already be URL-safe.
  pub fn sign_in_url(&self, state: &str) -> Option<String> {
    self.key.as_ref()?;
    let client_id = self.client_id.as_deref()?;
    Some(format!(
      "{AERO_LOGIN_URL}?client_id={client_id}&state={state}&ttl={SESSION_TTL_SECONDS}"
    ))
  }

  /// Who a token names, if it was made with drawme's key, is addressed to
  /// drawme and is currently valid. A token that decrypts with the key was
  /// made by Aero (or by drawme), so there is no signature to check besides.
  pub fn person(&self, token: &str) -> Option<Person> {
    let key = self.key.as_ref()?;
    let client_id = self.client_id.as_deref()?;
    // `iat`, `nbf` and `exp` are checked by default, and `exp` is required.
    let mut rules = ClaimsValidationRules::new();
    rules.validate_audience_with(client_id);
    let untrusted = UntrustedToken::<Local, V4>::try_from(token).ok()?;
    let trusted = local::decrypt(key, &untrusted, &rules, None, None).ok()?;
    let email = trusted
      .payload_claims()?
      .get_claim("email")?
      .as_str()?
      .trim()
      .to_lowercase();
    if email.is_empty() {
      return None;
    }
    let id = Uuid::new_v5(&PERSON_NAMESPACE, email.as_bytes());
    Some(Person { id, email })
  }

  /// A token as Aero would make it, for the dev server's sign-in and for
  /// tests. Never reachable from the deployed API.
  pub fn mint(&self, email: &str) -> Option<String> {
    let key = self.key.as_ref()?;
    let client_id = self.client_id.as_deref()?;
    let mut claims = Claims::new_expires_in(&Duration::from_secs(SESSION_TTL_SECONDS)).ok()?;
    claims.audience(client_id).ok()?;
    claims.add_additional("email", email).ok()?;
    local::encrypt(key, &claims, None, None).ok()
  }
}

fn from_env() -> (Option<SymmetricKey<V4>>, Option<String>) {
  let key = std::env::var(KEY_VAR)
    .ok()
    .and_then(|text| SymmetricKey::<V4>::try_from(text.trim()).ok());
  let client_id = std::env::var(CLIENT_ID_VAR)
    .ok()
    .and_then(|text| usable(&text));
  (key, client_id)
}

/// A client id as Aero issues them: base64url. Anything else is treated as
/// missing, since it would break the sign-in link.
fn usable(client_id: &str) -> Option<String> {
  let id = client_id.trim();
  let fits = !id.is_empty()
    && id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
  fits.then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;
  use pasetors::keys::Generate;
  use pasetors::paserk::FormatAsPaserk;

  fn paserk(key: &SymmetricKey<V4>) -> String {
    let mut text = String::new();
    key.fmt(&mut text).unwrap();
    text
  }

  fn token(key: &SymmetricKey<V4>, aud: &str, email: &str) -> String {
    let mut claims = Claims::new().unwrap();
    claims.audience(aud).unwrap();
    claims.add_additional("state", "s").unwrap();
    claims.add_additional("email", email).unwrap();
    local::encrypt(key, &claims, None, None).unwrap()
  }

  #[test]
  fn one_person_one_id_whatever_the_case() {
    let key = SymmetricKey::<V4>::generate().unwrap();
    let verifier = Verifier::new(&paserk(&key), "drawme").unwrap();
    let ada = verifier
      .person(&token(&key, "drawme", "Ada@Example.com"))
      .unwrap();
    assert_eq!(ada.email, "ada@example.com");
    let again = verifier
      .person(&token(&key, "drawme", " ada@example.com "))
      .unwrap();
    assert_eq!(again.id, ada.id);
    let grace = verifier
      .person(&token(&key, "drawme", "grace@example.com"))
      .unwrap();
    assert_ne!(grace.id, ada.id);
  }

  #[test]
  fn a_token_for_another_app_is_refused() {
    let key = SymmetricKey::<V4>::generate().unwrap();
    let verifier = Verifier::new(&paserk(&key), "drawme").unwrap();
    assert!(
      verifier
        .person(&token(&key, "champt", "ada@example.com"))
        .is_none()
    );
    assert!(verifier.person("v4.local.not-a-token").is_none());
  }

  #[test]
  fn nothing_gets_in_before_drawme_is_registered() {
    let key = SymmetricKey::<V4>::generate().unwrap();
    let verifier = Verifier::new(&paserk(&key), "").unwrap();
    assert!(
      verifier
        .person(&token(&key, "drawme", "ada@example.com"))
        .is_none()
    );
    assert!(verifier.sign_in_url("s").is_none());
  }

  #[test]
  fn a_sign_in_starts_at_aero_with_the_client_id_and_30_days() {
    let key = SymmetricKey::<V4>::generate().unwrap();
    let verifier = Verifier::new(&paserk(&key), " AbC-12_x ").unwrap();
    assert_eq!(
      verifier.sign_in_url("s-1").as_deref(),
      Some("https://aero.oops.wtf/login-with-aero?client_id=AbC-12_x&state=s-1&ttl=2592000")
    );
    for unusable in ["", "   ", "has space", "a&b=c"] {
      assert!(
        Verifier::new(&paserk(&key), unusable)
          .unwrap()
          .sign_in_url("s")
          .is_none(),
        "{unusable:?}"
      );
    }
  }

  #[test]
  fn a_minted_token_reads_back_as_its_person() {
    let verifier = Verifier::local();
    let token = verifier.mint("Ada@Example.com").unwrap();
    assert_eq!(verifier.person(&token).unwrap().email, "ada@example.com");
    // Another key can't read it.
    let other = Verifier::new(
      &paserk(&SymmetricKey::<V4>::generate().unwrap()),
      "drawme-dev",
    )
    .unwrap();
    assert!(other.person(&token).is_none());
  }

  #[test]
  fn a_key_must_be_a_k4_local_paserk() {
    assert!(Verifier::new("not-a-key", "drawme").is_err());
    assert!(Verifier::new("k4.public.AAAA", "drawme").is_err());
  }
}
