// Applied before first paint so the page never flashes the wrong theme.
(function () {
  var saved = null;
  try {
    saved = localStorage.getItem("theme");
  } catch (e) {
    /* storage unavailable: dark */
  }
  document.documentElement.dataset.theme = saved === "light" ? "light" : "dark";
})();
