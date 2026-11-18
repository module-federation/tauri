import { registerGlobalPlugins } from "@module-federation/runtime";

/**
 * @typedef {import("@module-federation/runtime").ModuleFederationRuntimePlugin} RuntimePlugin
 * @typedef {Parameters<NonNullable<RuntimePlugin["afterResolve"]>>[0]} LoadRemoteMatch
 * @typedef {Parameters<NonNullable<RuntimePlugin["errorLoadRemote"]>>[0]} ErrorLoadRemoteArgs
 * @typedef {Parameters<NonNullable<RuntimePlugin["onLoad"]>>[0]} OnLoadArgs
 */

const SCHEME = "mf";
/** Tell the Tauri plugin that an entry loaded, so it keeps that version as last-known-good. */
const COMMIT_PARAM = "mf-commit";
/** Ask the Tauri plugin to reject this launch's new, unconfirmed entry versions. */
const ROLLBACK_PARAM = "mf-rollback";
const FULL_URL_PARAM = "fullUrl";
const HOST_PLUGIN = "tauri-module-federation-host";
const GLOBAL_PLUGIN = "tauri-module-federation-global";
const LOG_PREFIX = "[@module-federation/tauri]";

/**
 * How this webview reaches the `mf` custom scheme, or `null` outside Tauri.
 *
 * - macOS, iOS and Linux load custom schemes directly: `mf://<authority>/...`.
 * - Windows (WebView2) and Android cannot. Tauri serves custom schemes there as
 *   `http(s)://mf.<authority>/...` and hands them to the plugin as `mf://<authority>/...`.
 *
 * Tauri's own `convertFileSrc` knows which form applies, so it decides here too.
 * @returns {{ prefix: string, workaround: boolean } | null}
 */
function protocol() {
	const convert = globalThis.__TAURI_INTERNALS__?.convertFileSrc;
	if (typeof convert !== "function") return null;
	// "mf://localhost/" or "http(s)://mf.localhost/"
	const base = String(convert("", SCHEME));
	const workaround = base.match(/^(https?):\/\/mf\.localhost\//i);
	if (workaround) return { prefix: `${workaround[1]}://${SCHEME}.`, workaround: true };
	return { prefix: `${SCHEME}://`, workaround: false };
}

/**
 * The authority of a remote in a workaround URL. The webview treats `mf.<authority>` as a
 * real host name, so it must be a valid one: IP addresses get a name, and anything that
 * is not under `localhost` is moved under it, so no request can leave the machine if the
 * webview does not intercept it. The Tauri plugin maps it back through `fullUrl`.
 * @param {URL} url
 */
function workaroundAuthority(url) {
	let host = url.hostname.toLowerCase();
	if (host.startsWith("[")) host = `ipv6-${host.slice(1, -1).replace(/[:.]/g, "-")}`;
	if (host !== "localhost" && !host.endsWith(".localhost")) host = `${host}.localhost`;
	return url.port ? `${host}:${url.port}` : host;
}

/**
 * Rewrites an http(s) URL to the `mf` scheme. The path is kept so that relative chunk
 * URLs resolve under it, and the original URL travels encoded in `fullUrl`.
 * @param {string} entry
 * @param {{ prefix: string, workaround: boolean }} target
 */
function toProtocolUrl(entry, target) {
	const url = new URL(entry);
	const authority = target.workaround ? workaroundAuthority(url) : url.host;
	const query = new URLSearchParams({ [FULL_URL_PARAM]: url.href });
	return `${target.prefix}${authority}${url.pathname}?${query}`;
}

/**
 * Whether `entry` was already rewritten by this plugin. The host and global plugins can
 * both run on one instance, so `afterResolve` can see its own output.
 * @param {string | undefined} entry
 * @param {{ prefix: string } | null} target
 */
function isProtocolUrl(entry, target) {
	if (!target || !entry?.startsWith(target.prefix)) return false;
	try {
		return new URL(entry).searchParams.has(FULL_URL_PARAM);
	} catch {
		return false;
	}
}

/**
 * Route remote entries through the Tauri plugin.
 * @param {LoadRemoteMatch} args
 */
function afterResolve(args) {
	const target = protocol();
	const { entry } = args.remoteInfo;
	// Only http(s) entries are rewritten, and only inside Tauri. Entries already rewritten
	// and other schemes pass through unchanged.
	if (target && /^https?:\/\//i.test(entry) && !isProtocolUrl(entry, target)) {
		args.remoteInfo.entry = toProtocolUrl(entry, target);
	}
	return args;
}

/** Entries already committed during this page load. */
const committed = new Set();

/**
 * A remote loaded: the version of its entry that was served is known good.
 * @param {OnLoadArgs} args
 */
function onLoad(args) {
	const entry = args.moduleInstance?.remoteInfo?.entry;
	if (!isProtocolUrl(entry, protocol()) || committed.has(entry)) return;
	committed.add(entry);
	fetch(`${entry}&${COMMIT_PARAM}=1`).catch((error) =>
		console.warn(`${LOG_PREFIX} Could not mark ${entry} as working.`, error),
	);
}

let rollback = null;

/**
 * A remote failed to load. If this launch served new versions of remote entries that
 * have not loaded successfully yet, one of them is likely the cause: the Tauri plugin
 * rejects them, and the page reloads with the last versions that worked. Reloading
 * also resets shared modules a broken release may have registered.
 * @param {ErrorLoadRemoteArgs} args
 */
async function errorLoadRemote(args) {
	if (args.lifecycle !== "onLoad" && args.lifecycle !== "beforeLoadShare") return;
	const target = protocol();
	if (!target) return undefined;

	rollback ??= fetch(`${target.prefix}localhost/?${ROLLBACK_PARAM}=1`)
		.then((response) => response.text())
		.then(Number)
		.catch((error) => {
			console.warn(`${LOG_PREFIX} Could not roll back remotes.`, error);
			return 0;
		});
	const rolledBack = await rollback;
	rollback = null;

	// The Tauri plugin caps rollbacks per launch, so this cannot reload forever.
	if (rolledBack > 0) {
		console.warn(
			`${LOG_PREFIX} A remote failed to load. Reloading with the last versions that worked (${rolledBack} rolled back).`,
			args.error,
		);
		location.reload();
		// Keep the failed load pending until the page reloads, instead of surfacing the error.
		return new Promise(() => {});
	}
	return undefined;
}

/** @type {Omit<RuntimePlugin, "name">} */
const hooks = { afterResolve, onLoad, errorLoadRemote };

/**
 * Remotes create their own runtime instances for nested remotes. A global plugin covers
 * those too. Registered once, even if this package is loaded more than once.
 */
function registerGlobal() {
	const globals = globalThis.__FEDERATION__?.__GLOBAL_PLUGIN__ ?? [];
	if (globals.some((plugin) => plugin?.name === GLOBAL_PLUGIN)) return;
	registerGlobalPlugins([{ name: GLOBAL_PLUGIN, ...hooks }]);
}

const hostPlugin = () => {
	// Lets the Tauri plugin's debug check see that the runtime plugin is in use.
	globalThis.__MF_TAURI_RUNTIME_PLUGIN__ = true;

	/** @type {RuntimePlugin} */
	const plugin = {
		name: HOST_PLUGIN,
		...hooks,
		// Runs whenever the plugin is registered, including instance.registerPlugins()
		// after createInstance(), when beforeInit has already happened.
		apply: () => {
			registerGlobal();
		},
		// Runtimes without `apply` support.
		beforeInit: (args) => {
			registerGlobal();
			return args;
		},
	};
	return plugin;
};

export default hostPlugin;
