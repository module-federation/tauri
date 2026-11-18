---
title: Architecture
summary: How the runtime plugin and the Tauri plugin route remote modules through the mf:// protocol.
read_when:
  - changing packages/tauri/index.js or packages/tauri-plugin/src
  - debugging a remote that does not load in a Tauri app
---

# Architecture

Two packages work together:

| Package | Path | Role |
|---|---|---|
| `@module-federation/tauri` | `packages/tauri` | Module Federation runtime plugin. Runs in the webview and rewrites remote entry URLs to `mf://`. |
| `tauri-plugin-module-federation` | `packages/tauri-plugin` | Tauri plugin. Registers the `mf` URI scheme, fetches remote files and caches them on disk. |

The runtime plugin decides *what* to load. The Tauri plugin handles *how*: network, cache, and which version of an entry is served. Neither works without the other.

## Request flow

Take a host with this remote:

```ts
remotes: { guest: 'guest@http://localhost:3002/remoteEntry.js' }
```

1. **Rewrite.** The runtime plugin's `afterResolve` hook changes the entry to:

   ```text
   mf://localhost:3002/remoteEntry.js?fullUrl=http%3A%2F%2Flocalhost%3A3002%2FremoteEntry.js
   ```

   The original URL travels URL-encoded in `fullUrl`, so its query string survives. The host and path are kept in the `mf://` URL itself, so relative chunk URLs resolve under the same path.

2. **Entry request.** The webview asks the Tauri plugin for that URL. The handler reads `fullUrl`, which must be an `http(s)` URL with a host (otherwise `400`). It remembers that `localhost:3002` uses `http`, and serves the request as an **entry**: versioned and eligible for rollback. See [Offline and failed releases](./offline-and-rollback.md).

3. **Chunk requests.** The entry loads its chunks with relative URLs, such as `mf://localhost:3002/static/js/123.abc.js`. These have no `fullUrl`. The handler looks up the origin it recorded for `localhost:3002` and fetches `http://localhost:3002/static/js/123.abc.js` as an **asset**: network first, cache on failure. An authority that never came through an entry request gets a `404`.

4. **Nested remotes.** A remote can consume remotes of its own, and those run in a separate runtime instance. When the host plugin is registered, it registers the same hooks as a global runtime plugin, so nested remotes are rewritten too. That happens in `apply`, which runs for config plugins and for `instance.registerPlugins()` after `createInstance`, and again in `beforeInit` for runtimes without `apply`.

Only `http(s)` entries are rewritten, and only inside Tauri (when `window.__TAURI_INTERNALS__` exists). Entries the plugin already rewrote pass through unchanged, because the host and global plugins can both run on one instance.

## Windows and Android

WebView2 and the Android webview cannot load custom schemes. Tauri serves them at `http(s)://<scheme>.<authority>/...` instead, and hands those requests to the plugin as `<scheme>://<authority>/...`. A page that requests `mf://...` directly fails with `URL scheme "mf" is not supported`.

The runtime plugin asks Tauri's `convertFileSrc` which form the webview uses, and builds URLs to match:

| Platform | Entry for `https://cdn.example.com/app/remoteEntry.js` |
|---|---|
| macOS, iOS, Linux | `mf://cdn.example.com/app/remoteEntry.js?fullUrl=...` |
| Windows, Android | `http://mf.cdn.example.com.localhost/app/remoteEntry.js?fullUrl=...` (`https` when the app uses `useHttpsScheme`) |

On Windows and Android the remote's host goes under `localhost`, so a request the webview fails to intercept never leaves the machine. IP addresses stay valid host names that way (`mf.127.0.0.1.localhost`), and IPv6 addresses become `ipv6-<address with dashes>`. Control requests use the same form, such as `http://mf.localhost/?mf-rollback=1`.

The Tauri plugin does not depend on the authority matching the remote. Each entry request records which origin its `mf` authority stands for, taken from `fullUrl`, and chunk requests are mapped back through that record.

## Runtime plugin hooks

| Hook | What it does |
|---|---|
| `afterResolve` | Rewrites `http(s)` entries to `mf://`. |
| `onLoad` | A remote loaded. Sends `<entry URL>&mf-commit=1` once per entry and page load, so the Tauri plugin keeps that version as last-known-good. |
| `errorLoadRemote` | A remote failed during `onLoad` or `beforeLoadShare`. Requests `mf://localhost/?mf-rollback=1`. If any versions were rolled back, it reloads the page and leaves the failed load pending. Otherwise the error goes on to the app. |
| `apply` | Runs when the plugin is registered. Registers the hooks above as a global plugin (`tauri-module-federation-global`), once. |
| `beforeInit` | Does the same for runtimes without `apply`. |

The hooks work with both call styles: webpack/Rsbuild bundler runtimes call `loadRemote(id, { loadFactory: false, from: 'build' })`, and `@module-federation/vite` calls `loadRemote(id)`.

## Control requests

The runtime plugin sends two kinds of control requests over `mf://`. Their responses carry `Access-Control-Allow-Origin: *` and `Cache-Control: no-store`.

| Request | Response |
|---|---|
| `<entry mf:// URL>&mf-commit=1` | `204` once the served version is last-known-good. `404` if the entry was not served during this launch. `409` if the cached copy changed after it was served. |
| `mf://localhost/?mf-rollback=1` | `200` with the number of entry versions rolled back, as plain text. `0` when there were no trials or the per-launch limit was reached. |

## Code map

| File | Contents |
|---|---|
| `packages/tauri/index.js` | The runtime plugin: URL rewriting and the commit and rollback hooks. |
| `packages/tauri-plugin/src/lib.rs` | `Builder`, `init`, scheme registration, and the debug warning shown when the runtime plugin is missing. |
| `packages/tauri-plugin/src/protocol.rs` | The request handler: URL resolution, network and cache fallback, entry versions, commit and rollback. |
| `packages/tauri-plugin/src/cache.rs` | On-disk cache: atomic writes, headers, last-known-good and rejected versions. |

## Debug builds

In debug builds the Tauri plugin injects a script that warns in the console when `@module-federation/tauri` is not in use. It checks 2 s after the page loads, so instances created at runtime count. It passes when the runtime plugin was created (it sets `window.__MF_TAURI_RUNTIME_PLUGIN__`), or when any instance or the global plugins include it.
