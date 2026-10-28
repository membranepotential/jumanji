// The password in front of a site published with `jumanji publish`
// (docs/publish/README.md, D15). The binary embeds this file
// (src/publish/wrangler.rs) and deploys it with a generated wrangler config.
//
// `run_worker_first` sends every request here before a file is served. Without
// the right password the answer is 401 with `WWW-Authenticate: Basic`, which
// makes the browser ask for one; with it, the request goes on to the static
// files (the `ASSETS` binding). The password is the Worker secret
// `SITE_PASSWORD`; the user name is ignored. With the secret unset, nobody
// gets in.
//
// Every response carries `X-Robots-Tag: noindex, nofollow`, the 401 included,
// so the site stays out of search indexes even if the check is ever switched
// off. This script is the one place response headers are set.

const REALM = "jumanji";
const NO_INDEX = "noindex, nofollow";

export default {
  async fetch(request, env) {
    if (!(await authorized(request, env.SITE_PASSWORD))) {
      return new Response("Password required.\n", {
        status: 401,
        headers: {
          "WWW-Authenticate": `Basic realm="${REALM}", charset="UTF-8"`,
          "X-Robots-Tag": NO_INDEX,
          "Cache-Control": "no-store",
          "Content-Type": "text/plain; charset=utf-8",
        },
      });
    }
    const asset = await env.ASSETS.fetch(request);
    const response = new Response(asset.body, asset);
    response.headers.set("X-Robots-Tag", NO_INDEX);
    // Static assets come marked `public`; a page behind a password must not
    // be kept by a shared cache and handed to someone without one.
    response.headers.set("Cache-Control", "private, max-age=0, must-revalidate");
    return response;
  },
};

// Whether `request` carries Basic credentials whose password is `password`.
// The two are compared as SHA-256 digests, which have equal length whatever
// was typed, so `timingSafeEqual` applies and the time taken says nothing
// about the password.
export async function authorized(request, password) {
  if (typeof password !== "string" || password === "") {
    return false;
  }
  const match = /^Basic\s+([A-Za-z0-9+/=]+)\s*$/i.exec(
    request.headers.get("Authorization") ?? "",
  );
  if (match === null) {
    return false;
  }
  let credentials;
  try {
    const bytes = Uint8Array.from(atob(match[1]), (c) => c.charCodeAt(0));
    credentials = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return false;
  }
  const colon = credentials.indexOf(":");
  if (colon < 0) {
    return false;
  }
  const [given, expected] = await Promise.all([
    sha256(credentials.slice(colon + 1)),
    sha256(password),
  ]);
  return crypto.subtle.timingSafeEqual(given, expected);
}

function sha256(text) {
  return crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
}
