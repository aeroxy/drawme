// Google Analytics, the same property as Aero's other sites and champt's. It
// starts from code rather than from <head>, as champt's does: a sign-in lands
// on /auth/aero with its token in the address, so the tag has to wait until
// auth.html has taken the token out (this script loads after the one that
// does). The page is also told its address without query or fragment, so the
// first page view carries neither a token nor an invite link (#invite:<room>).
// Not on a local server: running it there isn't a visit.

(() => {
  const MEASUREMENT_ID = "G-91SNPT9G03";
  if (/^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname)) return;
  window.dataLayer = window.dataLayer || [];
  // gtag.js reads `arguments` objects off the data layer, not arrays.
  window.gtag = function gtag() {
    window.dataLayer.push(arguments);
  };
  window.gtag("js", new Date());
  window.gtag("config", MEASUREMENT_ID, { page_location: location.origin + location.pathname });
  const script = document.createElement("script");
  script.async = true;
  script.src = `https://www.googletagmanager.com/gtag/js?id=${MEASUREMENT_ID}`;
  document.head.append(script);
})();
