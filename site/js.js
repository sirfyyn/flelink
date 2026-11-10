// Marks the page as "JavaScript runs" before it is drawn: style.css shows the squares, the machine picker and the tabs only
// then (html.js), and their plain-text fallbacks otherwise. A file of its own, loaded in the <head> without defer, because
// the server's Content-Security-Policy (script-src 'self') does not run inline scripts.
document.documentElement.classList.add("js");
