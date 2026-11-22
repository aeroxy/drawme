//! The Vercel Function. `vercel.json` rewrites every `/api/*` request here,
//! and the router still sees the original path.

use tower::ServiceBuilder;
use vercel_runtime::{Error, axum::VercelLayer};

#[tokio::main]
async fn main() -> Result<(), Error> {
  let url = std::env::var("REDIS_URL").map_err(|_| "REDIS_URL is not set")?;
  let rooms = drawme::Rooms::connect(&url, "drawme:").await?;
  let options = drawme::Options {
    secure_cookies: true,
    dev_sign_in: false,
  };
  let app = ServiceBuilder::new()
    .layer(VercelLayer::new())
    .service(drawme::app(
      rooms,
      drawme::Verifier::aero(),
      drawme::Turn::from_env(),
      options,
    ));
  vercel_runtime::run(app).await
}
