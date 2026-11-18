use std::{path::PathBuf, time::Duration};

use tauri::{
    path::BaseDirectory,
    plugin::{Builder as PluginBuilder, TauriPlugin},
    Manager, Runtime,
};

pub use models::*;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

mod cache;
mod error;
mod local;
mod models;
mod protocol;

pub use error::{Error, Result};
pub use local::LocalRemotes;

#[cfg(desktop)]
use desktop::TauriPluginModuleFederation;
#[cfg(mobile)]
use mobile::TauriPluginModuleFederation;

use protocol::Protocol;

/// Extensions to [`tauri::App`], [`tauri::AppHandle`] and [`tauri::Window`] to access the tauri-plugin-module-federation APIs.
pub trait TauriPluginModuleFederationExt<R: Runtime> {
    fn tauri_plugin_module_federation(&self) -> &TauriPluginModuleFederation<R>;
}

impl<R: Runtime, T: Manager<R>> crate::TauriPluginModuleFederationExt<R> for T {
    fn tauri_plugin_module_federation(&self) -> &TauriPluginModuleFederation<R> {
        self.state::<TauriPluginModuleFederation<R>>().inner()
    }
}

/// The custom protocol that `@module-federation/tauri` rewrites remote entries to.
const SCHEME: &str = "mf";

/// Configures the plugin.
///
/// ```no_run
/// use std::time::Duration;
///
/// let app = tauri::Builder::<tauri::Wry>::default().plugin(
///     tauri_plugin_module_federation::Builder::new()
///         .cache_folder("remotes")
///         .connect_timeout(Duration::from_secs(5))
///         .build(),
/// );
/// ```
#[derive(Debug, Clone)]
pub struct Builder {
    cache_folder: String,
    connect_timeout: Duration,
    read_timeout: Duration,
    rollback: bool,
    local_remotes: Vec<(String, BaseDirectory, PathBuf)>,
}

impl Default for Builder {
    fn default() -> Self {
        Self {
            cache_folder: "module-federation".into(),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(10),
            rollback: !cfg!(debug_assertions),
            local_remotes: Vec::new(),
        }
    }
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Folder inside the app cache dir that holds cached remotes. Defaults to `module-federation`.
    pub fn cache_folder(mut self, folder: impl Into<String>) -> Self {
        self.cache_folder = folder.into();
        self
    }

    /// How long to wait for a connection to a remote before serving it from the cache.
    /// Defaults to 10 seconds.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// How long a remote may stay silent while sending a response before it is served from
    /// the cache. This is an inactivity timeout, so large downloads on slow networks are
    /// not cut off. Defaults to 10 seconds.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Whether a new version of a remote entry that fails to load is rolled back to the
    /// last version that loaded successfully, in this launch and the following ones.
    ///
    /// Defaults to `true` in release builds and `false` in debug builds, so that during
    /// development a broken remote shows its error instead of an older version.
    /// Serving cached copies when the network fails is always enabled.
    pub fn rollback(mut self, enabled: bool) -> Self {
        self.rollback = enabled;
        self
    }

    /// Serves every remote URL under `prefix` from a local folder instead of the network.
    ///
    /// `path` is resolved against `base` (an absolute `path` is used as is). The host keeps
    /// its usual `http(s)` entry, for example `https://cdn.example.com/guest/remoteEntry.js`
    /// with the prefix `https://cdn.example.com/guest/`. Its requests are read from the
    /// folder: no network, no cache and no rollback, since the app owns those files.
    ///
    /// ```no_run
    /// use tauri::path::BaseDirectory;
    ///
    /// let app = tauri::Builder::<tauri::Wry>::default().plugin(
    ///     tauri_plugin_module_federation::Builder::new()
    ///         // <app data dir>/remotes/guest/remoteEntry.js
    ///         .local_remote("https://cdn.example.com/guest/", BaseDirectory::AppData, "remotes/guest")
    ///         .build(),
    /// );
    /// ```
    ///
    /// To change folders while the app runs, use [`LocalRemotes`] from the app state.
    pub fn local_remote(
        mut self,
        prefix: impl Into<String>,
        base: BaseDirectory,
        path: impl Into<PathBuf>,
    ) -> Self {
        self.local_remotes.push((prefix.into(), base, path.into()));
        self
    }

    pub fn build<R: Runtime>(self) -> TauriPlugin<R> {
        let builder = PluginBuilder::new("tauri-plugin-module-federation")
            .register_asynchronous_uri_scheme_protocol(SCHEME, |ctx, request, responder| {
                let app = ctx.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let response = match app.try_state::<Protocol>() {
                        Some(protocol) => protocol.handle(request).await,
                        None => protocol::text(
                            tauri::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "tauri-plugin-module-federation is not initialized",
                        ),
                    };
                    responder.respond(response);
                });
            });

        // Debug builds warn when nothing in the page registered @module-federation/tauri.
        // Apps can create their instance late (runtime API), so the check waits for the
        // page to load and looks at every instance, the global plugins, and the marker the
        // runtime plugin sets when it is created.
        #[cfg(debug_assertions)]
        let builder = builder.js_init_script(
            r#"
            (() => {
                if (window.top !== window) return;
                const NAMES = ['tauri-module-federation-host', 'tauri-module-federation-global'];
                const check = () => {
                    if (window.__MF_TAURI_RUNTIME_PLUGIN__) return;
                    const federation = window.__FEDERATION__;
                    const instances = federation?.__INSTANCES__ ?? [];
                    const registered =
                        instances.some((instance) =>
                            NAMES.some((name) => name in (instance?.hooks?.registerPlugins ?? {})),
                        ) || (federation?.__GLOBAL_PLUGIN__ ?? []).some((plugin) => NAMES.includes(plugin?.name));
                    if (registered) return;
                    console.warn(
                        instances.length
                            ? '[tauri-plugin-module-federation] @module-federation/tauri is not registered on any Module Federation instance. Add it to runtimePlugins, or call instance.registerPlugins([tauriPlugin()]).'
                            : '[tauri-plugin-module-federation] No Module Federation runtime found. Remotes load through mf:// only when @module-federation/tauri is registered.',
                    );
                };
                const schedule = () => setTimeout(check, 2000);
                if (document.readyState === 'complete') schedule();
                else window.addEventListener('load', schedule, { once: true });
            })();
            "#
            .to_string(),
        );

        builder
            .setup(move |app, api| {
                let cache_dir = app.path().app_cache_dir()?.join(&self.cache_folder);
                if let Err(error) = std::fs::create_dir_all(&cache_dir) {
                    log::warn!("failed to create {}: {error}", cache_dir.display());
                }
                let client = reqwest::Client::builder()
                    .connect_timeout(self.connect_timeout)
                    .read_timeout(self.read_timeout)
                    .build()?;
                let local_remotes = LocalRemotes::default();
                for (prefix, base, path) in &self.local_remotes {
                    local_remotes.set(prefix, app.path().resolve(path, *base)?)?;
                }
                app.manage(Protocol::new(
                    client,
                    cache::Cache::new(cache_dir),
                    local_remotes.clone(),
                    self.rollback,
                ));
                app.manage(local_remotes);

                #[cfg(mobile)]
                let tauri_plugin_module_federation = mobile::init(app, api)?;
                #[cfg(desktop)]
                let tauri_plugin_module_federation = desktop::init(app, api)?;
                app.manage(tauri_plugin_module_federation);

                Ok(())
            })
            .build()
    }
}

/// Initializes the plugin with default settings.
///
/// `cache_folder` is the folder inside the app cache dir that holds cached remotes
/// (`module-federation` when `None`). Use [`Builder`] to configure timeouts.
pub fn init<R: Runtime>(cache_folder: Option<&'static str>) -> TauriPlugin<R> {
    match cache_folder {
        Some(folder) => Builder::new().cache_folder(folder),
        None => Builder::new(),
    }
    .build()
}
