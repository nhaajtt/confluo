// Applies the saved look before the first paint, so the window never flashes the wrong theme.
(function () {
  try {
    var root = document.documentElement;
    var theme = localStorage.getItem("weft.theme");
    var accent = localStorage.getItem("weft.accent");
    if (theme === "light" || theme === "dark") root.dataset.theme = theme;
    if (accent) root.dataset.accent = accent;
    if (localStorage.getItem("weft.motion") === "reduced") root.dataset.motion = "reduced";
  } catch (e) {
    /* storage unavailable: keep the defaults */
  }
})();
