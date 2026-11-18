---
title: Local remotes
summary: Serve remotes from a folder on disk, for apps that install remotes with their own updater or installer.
read_when:
  - changing packages/tauri-plugin/src/local.rs
  - an app verifies, signs or bundles its remotes itself and wants to load them from disk
---

# Local remotes

Some apps install remotes themselves: an updater that checks signatures before installing a release, or an offline installer that ships the remotes with the app. Those apps want the host to read remotes from disk, not from the network.

The Tauri plugin can map a remote URL prefix to a folder. The host config does not change: it keeps its `http(s)` entry, and the runtime plugin rewrites it to `mf://` as usual. The Tauri plugin then reads every request under that prefix from the folder.

```rust
use tauri::path::BaseDirectory;

tauri::Builder::default()
    .plugin(
        tauri_plugin_module_federation::Builder::new()
            // https://cdn.example.com/guest/remoteEntry.js -> <app data dir>/remotes/guest/remoteEntry.js
            .local_remote("https://cdn.example.com/guest/", BaseDirectory::AppData, "remotes/guest")
            .build(),
    )
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
```

- The prefix is the URL of a folder. A missing trailing `/` is added, so `https://cdn.example.com/guest` covers `/guest/...` but not `/guest-2/...`. Its query string is ignored.
- `path` is resolved against the `BaseDirectory`. Use `BaseDirectory::Resource` for remotes bundled with the installer and `BaseDirectory::AppData` for remotes your updater downloads. An absolute path is used as is.
- When prefixes overlap, the longest one wins.
- The folder may not exist yet. Missing files answer `404`, and the Module Federation runtime reports a failed remote.

## Switching folders at runtime

The plugin puts a `LocalRemotes` in the app state. An updater that installs each release in its own folder can switch to it once the files are in place:

```rust
use tauri::Manager;
use tauri_plugin_module_federation::LocalRemotes;

fn on_update_installed(app: &tauri::AppHandle, dir: std::path::PathBuf) -> tauri_plugin_module_federation::Result<()> {
    app.state::<LocalRemotes>().set("https://cdn.example.com/guest/", dir)
}
```

The new folder is used for the next request. Remotes that already loaded keep running the old code until the page reloads. `LocalRemotes::remove(prefix)` sends that prefix to the network again.

## Development and release builds

A common setup is to load the dev server in development and the installed copy in release builds. Keep one entry URL and map it only in release builds:

```rust
let mut federation = tauri_plugin_module_federation::Builder::new();
if !cfg!(debug_assertions) {
    federation = federation.local_remote("https://cdn.example.com/guest/", BaseDirectory::AppData, "remotes/guest");
}
```

Or pick the entry per mode in the host config, such as `import.meta.env.DEV ? 'http://localhost:5173/remoteEntry.js' : 'https://cdn.example.com/guest/remoteEntry.js'`, and map the production URL.

The URL in a local prefix is never requested, so it can also be a name that only identifies the remote, such as `https://guest.remotes.invalid/`.

## What changes for local remotes

| | Network remotes | Local remotes |
|---|---|---|
| Source | The network, then the cache | The folder only |
| Offline | Cached copy | Always available |
| Cache | Written on every 2xx | Not used |
| Rollback | Last-known-good entry | Not used. `mf-commit` answers `204`, and a failed load is reported to the app. |
| Headers | Upstream headers | `Content-Type` from the file extension, `Access-Control-Allow-Origin: *`, `Cache-Control: no-store`, `x-mf-source: local` |
| Methods | Any | `GET` and `HEAD`. Others answer `405`. |

Your updater owns the files, so it also owns failed releases: keep the previous folder until a new one has loaded, and switch back if it does not.

## Security

Requests cannot leave the folder. Paths with `..`, encoded separators (`%2f`, `%5c`) or `:` answer `404`, and so do symlinks that resolve outside the folder.

Anything in the folder runs as code in the host's webview, so only install files you trust there. Signature checks belong in your updater, before it writes to the folder.
