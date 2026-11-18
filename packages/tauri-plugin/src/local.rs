//! Remotes served from local folders instead of the network.
//!
//! A local remote maps a URL prefix, such as `https://cdn.example.com/guest/`, to a folder.
//! Requests under that prefix are read from the folder: no network, no cache, no
//! rollback. The app (or its updater) owns the files and decides which version is there.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use tauri::http::{header, HeaderValue, Method, Request, Response, StatusCode};
use url::Url;

use crate::Error;

/// Header added to responses read from a local folder, for debugging.
const SOURCE_HEADER: &str = "x-mf-source";

#[derive(Debug)]
struct Mount {
    /// Origin and path of the prefix. The path always ends with `/`.
    prefix: Url,
    dir: PathBuf,
}

/// The local folders that remotes are served from.
///
/// Configure them with [`Builder::local_remote`](crate::Builder::local_remote), or at
/// runtime through this type, which the plugin puts in the app state:
///
/// ```no_run
/// use tauri::Manager;
/// use tauri_plugin_module_federation::LocalRemotes;
///
/// # fn example(app: &tauri::AppHandle) -> tauri_plugin_module_federation::Result<()> {
/// // After an updater has installed a new version of the remote:
/// app.state::<LocalRemotes>()
///     .set("https://cdn.example.com/guest/", "/path/to/remotes/guest/1.4.0")?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct LocalRemotes {
    mounts: Arc<RwLock<Vec<Mount>>>,
}

impl LocalRemotes {
    /// Serves every remote URL under `prefix` from `dir`.
    ///
    /// `prefix` is an `http(s)` URL of a folder. `https://cdn.example.com/guest` and
    /// `https://cdn.example.com/guest/` both cover `https://cdn.example.com/guest/remoteEntry.js`
    /// and its chunks. Setting a prefix again replaces its folder. When prefixes overlap,
    /// the longest one wins.
    ///
    /// The folder does not need to exist yet. Requests for missing files answer `404`.
    pub fn set(&self, prefix: &str, dir: impl Into<PathBuf>) -> crate::Result<()> {
        let prefix = parse_prefix(prefix)?;
        let mut mounts = self.write();
        mounts.retain(|mount| mount.prefix != prefix);
        mounts.push(Mount {
            prefix,
            dir: dir.into(),
        });
        // Longest prefix first, so the most specific folder wins.
        mounts.sort_by_key(|mount| std::cmp::Reverse(mount.prefix.as_str().len()));
        Ok(())
    }

    /// Stops serving `prefix` from a folder; its requests go to the network again.
    /// Returns whether the prefix was set.
    pub fn remove(&self, prefix: &str) -> bool {
        let Ok(prefix) = parse_prefix(prefix) else {
            return false;
        };
        let mut mounts = self.write();
        let before = mounts.len();
        mounts.retain(|mount| mount.prefix != prefix);
        mounts.len() != before
    }

    /// The folder and the path inside it that `url` maps to, if it is under a local prefix.
    pub(crate) fn lookup(&self, url: &Url) -> Option<(PathBuf, String)> {
        let mounts = self.mounts.read().unwrap_or_else(|p| p.into_inner());
        mounts.iter().find_map(|mount| {
            if url.origin() != mount.prefix.origin() {
                return None;
            }
            let rest = url.path().strip_prefix(mount.prefix.path())?;
            Some((mount.dir.clone(), rest.to_string()))
        })
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<Mount>> {
        self.mounts.write().unwrap_or_else(|p| p.into_inner())
    }
}

fn parse_prefix(prefix: &str) -> crate::Result<Url> {
    let mut url = Url::parse(prefix)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .ok_or_else(|| Error::InvalidLocalPrefix(prefix.to_string()))?;
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Reads `rest` (a URL path, still percent-encoded) from `dir`.
pub(crate) fn serve(request: &Request<Vec<u8>>, dir: &Path, rest: &str) -> Response<Vec<u8>> {
    let method = request.method();
    if method != Method::GET && method != Method::HEAD {
        return response(StatusCode::METHOD_NOT_ALLOWED, Vec::new(), None);
    }
    let Some(path) = file_path(dir, rest) else {
        return response(StatusCode::NOT_FOUND, Vec::new(), None);
    };
    match fs::read(&path) {
        Ok(body) => {
            let body = if method == Method::HEAD {
                Vec::new()
            } else {
                body
            };
            response(StatusCode::OK, body, Some(content_type(&path)))
        }
        Err(error) => {
            log::warn!("failed to read {}: {error}", path.display());
            response(StatusCode::NOT_FOUND, Vec::new(), None)
        }
    }
}

/// Maps a URL path to a file inside `dir`. Returns `None` for paths that would leave the
/// folder (`..`, encoded separators, symlinks pointing outside) and for missing files.
fn file_path(dir: &Path, rest: &str) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    for segment in rest.split('/') {
        let segment = percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .ok()?;
        let unsafe_segment =
            matches!(segment.as_ref(), "" | "." | "..") || segment.contains(['/', '\\', ':', '\0']);
        if unsafe_segment {
            return None;
        }
        path.push(segment.as_ref());
    }

    let root = dir.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    (path.starts_with(&root) && path.is_file()).then_some(path)
}

fn response(
    status: StatusCode,
    body: Vec<u8>,
    content_type: Option<&'static str>,
) -> Response<Vec<u8>> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    // ES module remotes are loaded in CORS mode.
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    // The folder can change while the app runs (an updater switching versions), so the
    // webview must not keep its own copy.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(SOURCE_HEADER, HeaderValue::from_static("local"));
    if let Some(content_type) = content_type {
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    response
}

/// Webviews refuse to run ES modules served with a non-JavaScript MIME type.
fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tauri-plugin-mf-local-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn get(path: &str) -> Request<Vec<u8>> {
        Request::builder()
            .uri(format!("mf://cdn.example.com{path}"))
            .body(Vec::new())
            .unwrap()
    }

    #[test]
    fn prefixes_match_by_origin_and_folder() {
        let local = LocalRemotes::default();
        local
            .set("https://cdn.example.com/guest", "/remotes/guest")
            .unwrap();

        let (dir, rest) = local
            .lookup(&url("https://cdn.example.com/guest/static/js/a.js?t=1"))
            .unwrap();
        assert_eq!(dir, PathBuf::from("/remotes/guest"));
        assert_eq!(rest, "static/js/a.js");

        assert!(local
            .lookup(&url("https://cdn.example.com/guest-2/remoteEntry.js"))
            .is_none());
        assert!(local
            .lookup(&url("http://cdn.example.com/guest/remoteEntry.js"))
            .is_none());
        assert!(local
            .lookup(&url("https://cdn.example.com:8443/guest/remoteEntry.js"))
            .is_none());
    }

    #[test]
    fn the_longest_prefix_wins_and_prefixes_can_be_replaced_or_removed() {
        let local = LocalRemotes::default();
        local.set("https://cdn.example.com/", "/all").unwrap();
        local
            .set("https://cdn.example.com/guest/", "/guest-1")
            .unwrap();
        local
            .set("https://cdn.example.com/guest/", "/guest-2")
            .unwrap();

        let entry = url("https://cdn.example.com/guest/remoteEntry.js");
        assert_eq!(local.lookup(&entry).unwrap().0, PathBuf::from("/guest-2"));

        assert!(local.remove("https://cdn.example.com/guest"));
        assert_eq!(local.lookup(&entry).unwrap().0, PathBuf::from("/all"));
        assert!(!local.remove("https://cdn.example.com/guest"));
    }

    #[test]
    fn invalid_prefixes_are_rejected() {
        let local = LocalRemotes::default();
        for prefix in ["file:///remotes", "not a url", "mf://localhost/"] {
            assert!(local.set(prefix, "/remotes").is_err(), "{prefix}");
        }
    }

    #[test]
    fn files_are_served_with_their_content_type_and_cors() {
        let dir = temp_dir("serve");
        fs::create_dir_all(dir.join("static/css")).unwrap();
        fs::write(dir.join("remoteEntry.js"), "export {}").unwrap();
        fs::write(dir.join("static/css/a b.css"), "body{}").unwrap();

        let entry = serve(&get("/remoteEntry.js"), &dir, "remoteEntry.js");
        assert_eq!(entry.status(), StatusCode::OK);
        assert_eq!(entry.body(), b"export {}");
        assert_eq!(
            entry.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(entry.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(entry.headers()[SOURCE_HEADER], "local");

        let css = serve(&get("/static/css/a%20b.css"), &dir, "static/css/a%20b.css");
        assert_eq!(css.body(), b"body{}");
        assert_eq!(
            css.headers()[header::CONTENT_TYPE],
            "text/css; charset=utf-8"
        );

        let mut head = get("/remoteEntry.js");
        *head.method_mut() = Method::HEAD;
        assert!(serve(&head, &dir, "remoteEntry.js").body().is_empty());

        let mut post = get("/remoteEntry.js");
        *post.method_mut() = Method::POST;
        assert_eq!(
            serve(&post, &dir, "remoteEntry.js").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[test]
    fn paths_cannot_leave_the_folder() {
        let parent = temp_dir("escape");
        let dir = parent.join("remote");
        fs::create_dir_all(&dir).unwrap();
        fs::write(parent.join("secret.txt"), "secret").unwrap();
        fs::write(dir.join("remoteEntry.js"), "export {}").unwrap();

        for rest in [
            "",
            "static/",
            "../secret.txt",
            "%2e%2e/secret.txt",
            "..%2fsecret.txt",
            "..%5csecret.txt",
            "missing.js",
        ] {
            assert_eq!(
                serve(&get("/x"), &dir, rest).status(),
                StatusCode::NOT_FOUND,
                "{rest}"
            );
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(parent.join("secret.txt"), dir.join("link.txt")).unwrap();
            assert_eq!(
                serve(&get("/link.txt"), &dir, "link.txt").status(),
                StatusCode::NOT_FOUND
            );
        }
    }
}
