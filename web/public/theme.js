// jfa.dev's theme-init.js: paint the saved theme before the first frame. A file of its own, not
// a script in index.html, because the page's Content-Security-Policy runs no inline script.
(function () {
  var match = document.cookie.match(/(?:^|;\s*)jfa-theme=(light|dark|system)/);
  var theme = match ? match[1] : "system";
  var dark =
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
})();
