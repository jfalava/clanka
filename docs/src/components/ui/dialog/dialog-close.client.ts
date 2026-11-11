import { mount } from "@cloudflare/nimbus-docs/client";

mount("[data-dialog-close]", (btn) => {
  const closeDialog = () => btn.closest("dialog")?.close();
  btn.addEventListener("click", closeDialog);

  return () => btn.removeEventListener("click", closeDialog);
});
