// `curl -fsSL https://clanka.jfa.dev/install.sh | sh`: the installer is
// hermes/install.sh, served as it is, so there is one script to keep.
import script from "../../../hermes/install.sh?raw";

export const prerender = true;

export function GET() {
  return new Response(script, {
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
}
