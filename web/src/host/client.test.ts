import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { HostClient, HostError, liveUrl, readToken, stripHash } from "./client";

interface Call {
  url: string;
  method: string;
  token: string | null;
  contentType: string | null;
}

function fakeFetch(calls: Call[], body: unknown = { ok: true }): typeof fetch {
  return (async (input: RequestInfo | URL, init?: RequestInit) => {
    const headers = (init?.headers ?? {}) as Record<string, string>;
    calls.push({
      url: String(input),
      method: init?.method ?? "GET",
      token: headers["X-R5ED-Token"] ?? headers["x-r5ed-token"] ?? null,
      contentType: headers["Content-Type"] ?? null,
    });
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
}

const TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

describe("host token handling", () => {
  it("reads the token from the fragment and strips the hash from the url", () => {
    expect(readToken(`#t=${TOKEN}`)).toBe(TOKEN);
    expect(readToken(`#other=1&t=${TOKEN}`)).toBe(TOKEN);
    expect(readToken(`#t=${encodeURIComponent("a b/c+d")}`)).toBe("a b/c+d");
    expect(readToken("#nothing=1")).toBe("");
    expect(readToken("")).toBe("");

    let cleaned = "";
    const url = `http://127.0.0.1:7797/index.html#t=${TOKEN}`;
    const result = stripHash(url, (u) => {
      cleaned = u;
    });
    expect(result).toBe("http://127.0.0.1:7797/index.html");
    expect(cleaned).toBe("http://127.0.0.1:7797/index.html");
    expect(cleaned).not.toContain(TOKEN);
    expect(stripHash("http://127.0.0.1:7797/", () => undefined)).toBe("http://127.0.0.1:7797/");
  });

  it("never leaves the token in location after the app reads it", () => {
    window.location.hash = `#t=${TOKEN}`;
    const token = readToken(window.location.hash);
    expect(token).toBe(TOKEN);
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
    expect(window.location.hash).toBe("");
    expect(window.location.href).not.toContain(TOKEN);
  });
});

describe("host client", () => {
  it("puts the token header on every call", async () => {
    const calls: Call[] = [];
    const host = new HostClient(TOKEN, { fetch: fakeFetch(calls) });
    await host.session();
    await host.projects();
    await host.createProject("my_map");
    await host.projectFiles("my_map");
    await host.readProjectFile("my_map", "map.json");
    await host.writeProjectFile("my_map", "map.json", new Uint8Array([1, 2, 3]));
    await host.assetSearch("crate", "model", 0, 20);
    await host.assetModelInfo("mdl/a.rmdl");
    await host.assetMaterial("materials/b.rmdl");
    await host.assetTexture("1234");
    await host.assetThumbnail("mdl/a.rmdl");
    await host.surfaceProps();
    await host.rebuildAssets();
    await host.maps();
    await host.mapReference("mp_rr_test");
    await host.mapChunk("mp_rr_test", 3);
    await host.schema();
    await host.createJob("compile", "my_map", { targets: ["client"] });
    await host.jobs();
    await host.job("job-1");
    await host.cancelJob("job-1");
    expect(calls).toHaveLength(21);
    for (const call of calls) {
      expect(call.token).toBe(TOKEN);
      expect(call.url).not.toContain(TOKEN);
    }
  });

  it("uses the method the host router accepts for each route", async () => {
    const calls: Call[] = [];
    const host = new HostClient(TOKEN, { fetch: fakeFetch(calls) });
    await host.readProjectFile("p", "map.json");
    await host.writeProjectFile("p", "map.json", new Uint8Array(0));
    await host.createProject("p");
    await host.createJob("bake", "p", {});
    await host.cancelJob("j1");
    await host.rebuildAssets();
    await host.jobs();
    expect(calls.map((c) => `${c.method} ${c.url.split("?")[0]}`)).toEqual([
      "GET /api/projects/p/file",
      "PUT /api/projects/p/file",
      "POST /api/projects",
      "POST /api/jobs",
      "POST /api/jobs/j1/cancel",
      "POST /api/assets/rebuild",
      "GET /api/jobs",
    ]);
  });

  it("encodes query parameters and paths", async () => {
    const calls: Call[] = [];
    const host = new HostClient(TOKEN, { fetch: fakeFetch(calls) });
    await host.assetSearch("a b&c", "model", 10, 5);
    expect(calls[0].url).toBe("/api/assets/search?q=a%20b%26c&kind=model&offset=10&limit=5");
    await host.readProjectFile("proj", "meshes/a b.glb");
    expect(calls[1].url).toBe("/api/projects/proj/file?path=meshes%2Fa%20b.glb");
  });

  it("sends the right content type per body kind", async () => {
    const calls: Call[] = [];
    const host = new HostClient(TOKEN, { fetch: fakeFetch(calls) });
    await host.createProject("p");
    expect(calls[0].contentType).toBe("application/json");
    await host.writeProjectFile("p", "map.json", new Uint8Array(0));
    expect(calls[1].contentType).toBe("application/octet-stream");
  });

  it("without a token sends no token header and leaves auth to the session cookie", async () => {
    let seen: Record<string, string> = {};
    const capture = (async (_url: string, init: RequestInit) => {
      seen = (init.headers ?? {}) as Record<string, string>;
      return new Response("[]", { status: 200, headers: { "content-type": "application/json" } });
    }) as unknown as typeof fetch;
    await new HostClient("", { fetch: capture }).projects();
    expect(Object.keys(seen).map((k) => k.toLowerCase())).not.toContain("x-r5ed-token");
  });

  it("surfaces a host error with its status", async () => {
    const failing = (async () => new Response("no", { status: 401, statusText: "Unauthorized" })) as unknown as typeof fetch;
    const host = new HostClient(TOKEN, { fetch: failing });
    await expect(host.session()).rejects.toThrow(HostError);
  });

  it("puts the token in the live socket url, not the http one", () => {
    const url = liveUrl("http://127.0.0.1:7797", TOKEN);
    expect(url.startsWith("ws://127.0.0.1:7797/api/live")).toBe(true);
    expect(url).toContain(`token=${TOKEN}`);
  });
});

describe("location cleanup", () => {
  const original = window.location.hash;
  beforeEach(() => {
    window.location.hash = "";
  });
  afterEach(() => {
    window.location.hash = original;
  });

  it("leaves an empty hash when there is no token", () => {
    expect(readToken(window.location.hash)).toBe("");
  });
});

describe("a restarted host", () => {
  it("refreshes the page cookie, drops the stale token and retries once", async () => {
    const seen: { url: string; token: string | null }[] = [];
    let issued = false;
    const impl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const headers = (init?.headers ?? {}) as Record<string, string>;
      const url = String(input);
      seen.push({ url, token: headers["X-R5ED-Token"] ?? null });
      if (url === "/") {
        issued = true;
        return new Response("<html></html>", { status: 200, headers: { "content-type": "text/html" } });
      }
      // The old token is refused; after the page load the cookie answers.
      if (!issued || headers["X-R5ED-Token"]) return new Response("the request carried no valid session token", { status: 401 });
      return new Response(JSON.stringify({ name: "x" }), { status: 200, headers: { "content-type": "application/json" } });
    }) as unknown as typeof fetch;
    const host = new HostClient(TOKEN, { fetch: impl });
    await expect(host.createProject("x")).resolves.toEqual({ name: "x" });
    expect(seen.map((s) => s.url)).toEqual(["/api/projects", "/", "/api/projects"]);
    expect(seen[2].token).toBeNull();
  });
});
