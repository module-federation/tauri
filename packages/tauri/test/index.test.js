import assert from "node:assert/strict";
import { afterEach, describe, test } from "node:test";

import { createInstance } from "@module-federation/runtime";

import tauriPlugin from "../index.js";

/** What Tauri's `convertFileSrc("", "mf")` returns on each platform. */
const PLATFORMS = {
	macos: "mf://localhost/",
	windows: "http://mf.localhost/",
	windowsHttps: "https://mf.localhost/",
};

function onPlatform(base) {
	globalThis.__TAURI_INTERNALS__ = {
		convertFileSrc: (path, scheme) => base.replace("mf", scheme) + encodeURIComponent(path),
	};
}

function resolveEntry(entry) {
	const args = { remoteInfo: { entry } };
	tauriPlugin().afterResolve(args);
	return args.remoteInfo.entry;
}

/** What wry hands the Tauri plugin for a Windows and Android workaround URL. */
function revertWorkaround(url) {
	return url.replace(/^https?:\/\/mf\./, "mf://");
}

afterEach(() => {
	delete globalThis.__TAURI_INTERNALS__;
});

describe("afterResolve", () => {
	test("leaves entries alone outside Tauri", () => {
		assert.equal(
			resolveEntry("http://localhost:3002/remoteEntry.js"),
			"http://localhost:3002/remoteEntry.js",
		);
	});

	test("uses mf:// where the webview loads custom schemes directly", () => {
		onPlatform(PLATFORMS.macos);
		assert.equal(
			resolveEntry("http://localhost:3002/remoteEntry.js?v=1"),
			"mf://localhost:3002/remoteEntry.js?fullUrl=http%3A%2F%2Flocalhost%3A3002%2FremoteEntry.js%3Fv%3D1",
		);
	});

	test("uses Tauri's http(s)://mf.<host> form on Windows and Android", () => {
		onPlatform(PLATFORMS.windows);
		const cases = {
			"http://localhost:3002/remoteEntry.js": "http://mf.localhost:3002/remoteEntry.js",
			"https://cdn.example.com/app/remoteEntry.js":
				"http://mf.cdn.example.com.localhost/app/remoteEntry.js",
			"http://127.0.0.1:3002/remoteEntry.js": "http://mf.127.0.0.1.localhost:3002/remoteEntry.js",
			"http://[::1]:3002/remoteEntry.js": "http://mf.ipv6---1.localhost:3002/remoteEntry.js",
		};
		for (const [entry, expected] of Object.entries(cases)) {
			const resolved = resolveEntry(entry);
			const url = new URL(resolved);
			assert.equal(`${url.origin}${url.pathname}`, expected, entry);
			assert.equal(url.searchParams.get("fullUrl"), new URL(entry).href, entry);
			// The plugin receives a URL its handler can parse, with the remote in fullUrl.
			assert.match(revertWorkaround(resolved), /^mf:\/\/[^/]+\//, entry);
		}
	});

	test("follows the webview's http or https scheme", () => {
		onPlatform(PLATFORMS.windowsHttps);
		assert.match(resolveEntry("http://localhost:3002/remoteEntry.js"), /^https:\/\/mf\.localhost:3002\//);
	});

	test("does not rewrite its own output", () => {
		for (const base of Object.values(PLATFORMS)) {
			onPlatform(base);
			const once = resolveEntry("https://cdn.example.com/remoteEntry.js");
			assert.equal(resolveEntry(once), once, base);
		}
	});

	test("rewrites a real remote whose host starts with mf.", () => {
		onPlatform(PLATFORMS.windows);
		assert.equal(
			new URL(resolveEntry("http://mf.example.com/remoteEntry.js")).host,
			"mf.mf.example.com.localhost",
		);
	});
});

describe("control requests", () => {
	test("rollback and commit use the platform's form", async () => {
		const requested = [];
		const realFetch = globalThis.fetch;
		const realLocation = globalThis.location;
		globalThis.fetch = async (url) => {
			requested.push(String(url));
			return new Response("0");
		};
		globalThis.location = { reload() {} };
		try {
			onPlatform(PLATFORMS.windows);
			const plugin = tauriPlugin();
			const entry = resolveEntry("http://localhost:3003/remoteEntry.js");
			plugin.onLoad({ moduleInstance: { remoteInfo: { entry } } });
			await plugin.errorLoadRemote({ lifecycle: "onLoad", error: new Error("x") });

			assert.deepEqual(requested, [`${entry}&mf-commit=1`, "http://mf.localhost/?mf-rollback=1"]);
		} finally {
			globalThis.fetch = realFetch;
			globalThis.location = realLocation;
		}
	});
});

describe("registration", () => {
	test("registering after createInstance still covers nested remotes", () => {
		const instance = createInstance({ name: "late_host", remotes: [] });
		instance.registerPlugins([tauriPlugin()]);

		assert.ok("tauri-module-federation-host" in instance.hooks.registerPlugins);
		const globals = globalThis.__FEDERATION__.__GLOBAL_PLUGIN__.map((plugin) => plugin.name);
		assert.deepEqual(
			globals.filter((name) => name === "tauri-module-federation-global"),
			["tauri-module-federation-global"],
		);
		assert.equal(globalThis.__MF_TAURI_RUNTIME_PLUGIN__, true);

		// A nested remote's instance, created afterwards, picks up the global plugin.
		const nested = createInstance({ name: "nested_remote", remotes: [] });
		assert.ok("tauri-module-federation-global" in nested.hooks.registerPlugins);
	});
});
