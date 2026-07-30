// A minimal cookie-following user agent, for driving an OAuth redirect chain.
//
// WHY NOT PLAYWRIGHT. The e2e suite (`console/e2e/*.e2e.ts`) owns a real browser
// and the things only a real browser can answer — rendering, focus order, what
// actually reaches the DOM. What the OAuth flow needs is different and smaller:
// follow 302s across two origins, carry cookies per-origin, and let the test
// inspect every hop. Playwright would add a browser download to a suite that
// otherwise runs anywhere, and would hide the hops behind navigation.
//
// WHY NOT `redirect: "follow"`. The default follower drops cookies between hops
// and gives the test no way to assert on the intermediate redirect — which is
// where the authorization code and the `state` parameter live, and where a
// missing `iss` or a mismatched `redirect_uri` would show up.

export interface Hop {
  readonly method: string;
  readonly url: string;
  readonly status: number;
  readonly location: string | null;
}

export interface BrowserAgent {
  /** Follow redirects, carrying cookies, up to `maxHops`. */
  navigate(url: string, init?: RequestInit): Promise<Response>;
  /** One request, no redirect following. */
  request(url: string, init?: RequestInit): Promise<Response>;
  /** Every hop taken, in order. */
  readonly hops: Hop[];
  /** The cookie header this agent would send to `url`. */
  cookieHeaderFor(url: string): string;
  readonly cookieNames: () => string[];
}

/**
 * Cookies are stored per origin and without attribute handling (no `Domain`, no
 * `Path`, no `Expires`). That is deliberate: this agent drives a fixture on
 * `localhost` where every cookie is host-only and path-`/`, and a partial
 * implementation of cookie scoping would be a subtly wrong one. If a test ever
 * needs real scoping semantics, it belongs in the Playwright suite.
 */
export function createBrowserAgent(maxHops = 10): BrowserAgent {
  const jars = new Map<string, Map<string, string>>();
  const hops: Hop[] = [];

  function jarFor(origin: string): Map<string, string> {
    let jar = jars.get(origin);
    if (jar === undefined) {
      jar = new Map();
      jars.set(origin, jar);
    }
    return jar;
  }

  function storeCookies(origin: string, response: Response): void {
    const jar = jarFor(origin);
    // `getSetCookie` is the only correct way to read multiple Set-Cookie
    // headers; `headers.get` folds them into one comma-joined string that
    // cannot be split unambiguously (an `Expires` value contains a comma).
    for (const raw of response.headers.getSetCookie()) {
      const pair = raw.split(";", 1)[0] ?? "";
      const eq = pair.indexOf("=");
      if (eq <= 0) continue;
      const name = pair.slice(0, eq).trim();
      const value = pair.slice(eq + 1).trim();
      if (value === "" || /(^|;)\s*max-age=0(;|$)/i.test(raw)) {
        jar.delete(name);
        continue;
      }
      jar.set(name, value);
    }
  }

  function cookieHeaderFor(url: string): string {
    const jar = jars.get(new URL(url).origin);
    if (jar === undefined || jar.size === 0) return "";
    return [...jar.entries()].map(([name, value]) => `${name}=${value}`).join("; ");
  }

  async function request(url: string, init: RequestInit = {}): Promise<Response> {
    const origin = new URL(url).origin;
    const headers = new Headers(init.headers);
    const cookies = cookieHeaderFor(url);
    if (cookies !== "") headers.set("cookie", cookies);

    const response = await fetch(url, { ...init, headers, redirect: "manual" });
    storeCookies(origin, response);
    hops.push({
      method: (init.method ?? "GET").toUpperCase(),
      url,
      status: response.status,
      location: response.headers.get("location"),
    });
    return response;
  }

  async function navigate(url: string, init: RequestInit = {}): Promise<Response> {
    let current = url;
    let currentInit = init;
    for (let hop = 0; hop < maxHops; hop += 1) {
      const response = await request(current, currentInit);
      const location = response.headers.get("location");
      if (response.status < 300 || response.status >= 400 || location === null) {
        return response;
      }
      current = new URL(location, current).toString();
      // A redirect is always followed as a GET with no body — anything else
      // would replay a POST body across an origin boundary.
      currentInit = {};
    }
    throw new Error(`redirect loop: more than ${maxHops} hops starting at ${url}`);
  }

  return {
    navigate,
    request,
    hops,
    cookieHeaderFor,
    cookieNames: () => [...jars.values()].flatMap((jar) => [...jar.keys()]),
  };
}
