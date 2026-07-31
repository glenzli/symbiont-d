export function initTopbarUi() {
  const overflow = document.querySelector("#top-overflow");
  if (!overflow) return;

  overflow.addEventListener("click", (event) => {
    if (event.target.closest("[data-top-menu-action]")) {
      overflow.open = false;
    }
  });

  document.addEventListener("click", (event) => {
    if (overflow.open && !overflow.contains(event.target)) overflow.open = false;
  });

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") overflow.open = false;
  });
}
