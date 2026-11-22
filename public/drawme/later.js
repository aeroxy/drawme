// Scripts jspaint needs only when someone does something, not to start. They
// load once the page has loaded and gone quiet, so they are off the path to the
// first dialog.
//
// pdf.js (406 KB): only File > Open of a PDF uses it. jspaint reads
// window["pdfjs-dist/build/pdf"] when one is opened, not before.
//
// AnyPalette, which looks like it could join it, can't: file-format-data.js
// reads AnyPalette.formats the moment it loads.

addEventListener("load", () => {
  const load = () => {
    const script = document.createElement("script");
    script.src = "lib/pdf.js/build/pdf.js";
    document.head.append(script);
  };
  if ("requestIdleCallback" in window) requestIdleCallback(load, { timeout: 5000 });
  else setTimeout(load, 2000);
});
