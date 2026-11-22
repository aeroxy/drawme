//! The page and the API on one local port, for development, against the
//! Redis that `REDIS_URL` names. Keys go under `dev:`, so they never mix with
//! the deployment's even in the same database.
//!
//! Serves what Vercel serves: files in `public/` first, then jspaint's own
//! folders from jsDelivr at the commit `vercel.json` pins, and `/auth/aero`
//! as the sign-in callback. Signing in is a local form instead of Aero's
//! consent screen, since Aero only answers on the deployed callback; it
//! mints the same kind of token and goes through the same callback page.

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::header::CONTENT_TYPE;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// The jspaint commit both this server and `vercel.json` serve.
pub const JSPAINT_COMMIT: &str = "53be67ab8c47cc0d2168899e7481bc04839c4c81";
/// jspaint's folders and files that come from jsDelivr, in `vercel.json` too.
const JSPAINT_FOLDERS: &[&str] = &[
  "src",
  "lib",
  "images",
  "styles",
  "localization",
  "help",
  "audio",
];
const JSPAINT_FILES: &[&str] = &["favicon.ico"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
  dotenvy::dotenv().ok();
  let url = std::env::var("REDIS_URL").map_err(|_| "set REDIS_URL (see .env.example)")?;
  let listen = std::env::var("LISTEN").unwrap_or_else(|_| "127.0.0.1:8787".to_owned());
  let rooms = drawme::Rooms::connect(&url, "dev:").await?;
  let options = drawme::Options {
    secure_cookies: false,
    dev_sign_in: true,
  };
  let files = Files::default();
  let app = drawme::app(
    rooms,
    drawme::Verifier::local(),
    drawme::Turn::from_env(),
    options,
  )
  .fallback(get(move |uri: Uri| {
    let files = files.clone();
    async move { files.serve(uri.path()).await }
  }));
  let listener = tokio::net::TcpListener::bind(&listen).await?;
  println!("drawme on http://{listen}");
  axum::serve(listener, app).await?;
  Ok(())
}

#[derive(Clone, Default)]
struct Files {
  http: reqwest::Client,
  /// jspaint's files, kept once fetched: the commit is pinned, so they never change.
  cache: Arc<Mutex<HashMap<String, (String, Bytes)>>>,
}

impl Files {
  async fn serve(&self, path: &str) -> Response {
    let path = match path {
      "/" => "index.html",
      "/auth/aero" => "auth.html",
      other => other.trim_start_matches('/'),
    };
    if path.is_empty()
      || path
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
      || path.contains('\\')
    {
      return StatusCode::NOT_FOUND.into_response();
    }
    // Read on every request, so an edit shows on reload.
    if let Ok(body) = tokio::fs::read(format!("public/{path}")).await {
      return ([(CONTENT_TYPE, content_type(path).to_owned())], body).into_response();
    }
    let top = path.split('/').next().unwrap_or_default();
    if JSPAINT_FOLDERS.contains(&top) || JSPAINT_FILES.contains(&path) {
      return self.jspaint(path).await;
    }
    StatusCode::NOT_FOUND.into_response()
  }

  async fn jspaint(&self, path: &str) -> Response {
    if let Some((kind, body)) = self.cache.lock().unwrap().get(path).cloned() {
      return ([(CONTENT_TYPE, kind)], body).into_response();
    }
    let url = format!("https://cdn.jsdelivr.net/gh/1j01/jspaint@{JSPAINT_COMMIT}/{path}");
    let answer = match self.http.get(&url).send().await {
      Ok(answer) if answer.status().is_success() => answer,
      Ok(answer) => {
        return answer
          .status()
          .as_u16()
          .try_into()
          .unwrap_or(StatusCode::BAD_GATEWAY)
          .into_response();
      }
      Err(err) => {
        eprintln!("{url}: {err}");
        return StatusCode::BAD_GATEWAY.into_response();
      }
    };
    let kind = answer
      .headers()
      .get(CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .unwrap_or(content_type(path))
      .to_owned();
    let Ok(body) = answer.bytes().await else {
      return StatusCode::BAD_GATEWAY.into_response();
    };
    self
      .cache
      .lock()
      .unwrap()
      .insert(path.to_owned(), (kind.clone(), body.clone()));
    ([(CONTENT_TYPE, kind)], body).into_response()
  }
}

fn content_type(path: &str) -> &'static str {
  match path.rsplit('.').next().unwrap_or_default() {
    "html" | "htm" => "text/html; charset=utf-8",
    "js" | "mjs" => "text/javascript; charset=utf-8",
    "css" => "text/css; charset=utf-8",
    "json" | "webmanifest" => "application/json",
    "png" => "image/png",
    "gif" => "image/gif",
    "jpg" | "jpeg" => "image/jpeg",
    "svg" => "image/svg+xml",
    "ico" => "image/x-icon",
    "wav" => "audio/wav",
    "txt" | "hhc" | "hhk" => "text/plain; charset=utf-8",
    _ => "application/octet-stream",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// `vercel.json` and this server must serve the same jspaint, from the
  /// same folders.
  #[test]
  fn vercel_json_pins_the_same_jspaint() {
    let config =
      std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/vercel.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let base = format!("https://cdn.jsdelivr.net/gh/1j01/jspaint@{JSPAINT_COMMIT}/");
    let rewrites = config["rewrites"].as_array().unwrap();
    let external: Vec<(&str, &str)> = rewrites
      .iter()
      .filter_map(|rule| Some((rule["source"].as_str()?, rule["destination"].as_str()?)))
      .filter(|(_, destination)| destination.starts_with("https://"))
      .collect();
    for (source, destination) in &external {
      assert!(
        destination.starts_with(&base),
        "{source} goes to {destination}"
      );
    }
    for folder in JSPAINT_FOLDERS {
      let source = format!("/{folder}/:path*");
      assert!(
        external
          .iter()
          .any(|(s, d)| *s == source && *d == format!("{base}{folder}/:path*")),
        "{source}"
      );
    }
    for file in JSPAINT_FILES {
      let source = format!("/{file}");
      assert!(
        external
          .iter()
          .any(|(s, d)| *s == source && *d == format!("{base}{file}")),
        "{source}"
      );
    }
    assert_eq!(external.len(), JSPAINT_FOLDERS.len() + JSPAINT_FILES.len());
  }
}
