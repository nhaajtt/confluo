// Applies the saved look before the first paint, so the window never flashes the wrong theme.
(function () {
  try {
    var root = document.documentElement;
    var theme = localStorage.getItem("confluo.theme");
    var accent = localStorage.getItem("confluo.accent");
    if (theme === "light" || theme === "dark") root.dataset.theme = theme;
    if (accent) root.dataset.accent = accent;
    if (localStorage.getItem("confluo.motion") === "reduced") root.dataset.motion = "reduced";
  } catch (e) {
    /* storage unavailable: keep the defaults */
  }
})();
