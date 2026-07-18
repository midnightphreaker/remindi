use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use thiserror::Error;

const MAX_CUSTOM_CSS_BYTES: u64 = 256 * 1024;
const MAX_LOGO_BYTES: u64 = 2 * 1024 * 1024;
const MAX_FAVICON_BYTES: u64 = 512 * 1024;

const INDEX: &str = include_str!("static/index.html");
const APP_CSS: &[u8] = include_bytes!("static/app.css");
const APP_JS: &[u8] = include_bytes!("static/app.js");
const DEFAULT_LOGO: &[u8] = include_bytes!("static/logo.svg");
const DEFAULT_FAVICON: &[u8] = include_bytes!("static/favicon.svg");

/// Optional read-once WebUI presentation overrides.
#[derive(Clone, Debug, Default)]
pub struct AssetOverrides {
    /// Administrator-mounted custom stylesheet.
    pub custom_css: Option<PathBuf>,
    /// Administrator-mounted brand logo.
    pub logo: Option<PathBuf>,
    /// Administrator-mounted favicon.
    pub favicon: Option<PathBuf>,
}

/// Fail-closed custom asset validation errors.
#[derive(Debug, Error)]
pub enum AssetError {
    /// The configured asset could not be inspected or read.
    #[error("unable to read configured WebUI {kind}")]
    Read {
        /// Safe asset kind.
        kind: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The path is not an absolute regular file.
    #[error("configured WebUI {kind} must be an absolute regular file")]
    InvalidFile {
        /// Safe asset kind.
        kind: &'static str,
    },
    /// The file may be changed by untrusted local users.
    #[error("configured WebUI {kind} must not be world-writable")]
    WorldWritable {
        /// Safe asset kind.
        kind: &'static str,
    },
    /// The asset exceeds its source-defined limit.
    #[error("configured WebUI {kind} exceeds its size limit")]
    TooLarge {
        /// Safe asset kind.
        kind: &'static str,
    },
    /// The image extension or content is not approved.
    #[error("configured WebUI {kind} has an unsupported image type")]
    UnsupportedType {
        /// Safe asset kind.
        kind: &'static str,
    },
    /// Custom CSS must be valid UTF-8 text.
    #[error("configured WebUI custom CSS must be valid UTF-8")]
    InvalidCss,
}

/// Immutable WebUI bytes loaded before serving begins.
pub struct WebUiAssets {
    index: Vec<u8>,
    custom_css: Vec<u8>,
    logo: Vec<u8>,
    logo_content_type: &'static str,
    favicon: Vec<u8>,
    favicon_content_type: &'static str,
}

impl WebUiAssets {
    /// Returns the embedded defaults.
    #[must_use]
    pub fn embedded(title: &str) -> Self {
        Self {
            index: render_index(title),
            custom_css: Vec::new(),
            logo: DEFAULT_LOGO.to_vec(),
            logo_content_type: "image/svg+xml",
            favicon: DEFAULT_FAVICON.to_vec(),
            favicon_content_type: "image/svg+xml",
        }
    }

    /// Reads configured overrides exactly once and rejects unsafe files.
    ///
    /// Empty custom files retain their embedded defaults.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError`] for missing, unsafe, oversized, or invalid files.
    pub fn load(title: &str, overrides: AssetOverrides) -> Result<Self, AssetError> {
        let mut assets = Self::embedded(title);
        if let Some(path) = overrides.custom_css {
            let bytes = read_asset(&path, "custom CSS", MAX_CUSTOM_CSS_BYTES)?;
            if !bytes.is_empty() {
                std::str::from_utf8(&bytes).map_err(|_| AssetError::InvalidCss)?;
                assets.custom_css = bytes;
            }
        }
        if let Some(path) = overrides.logo {
            let bytes = read_asset(&path, "logo", MAX_LOGO_BYTES)?;
            if !bytes.is_empty() {
                assets.logo_content_type = image_content_type(&path, &bytes, "logo")?;
                assets.logo = bytes;
            }
        }
        if let Some(path) = overrides.favicon {
            let bytes = read_asset(&path, "favicon", MAX_FAVICON_BYTES)?;
            if !bytes.is_empty() {
                assets.favicon_content_type = image_content_type(&path, &bytes, "favicon")?;
                assets.favicon = bytes;
            }
        }
        Ok(assets)
    }

    pub(crate) fn index(&self) -> &[u8] {
        &self.index
    }
    pub(crate) const fn app_css(&self) -> &'static [u8] {
        APP_CSS
    }
    pub(crate) const fn app_js(&self) -> &'static [u8] {
        APP_JS
    }
    /// Returns the optional immutable custom CSS bytes.
    #[must_use]
    pub fn custom_css(&self) -> &[u8] {
        &self.custom_css
    }
    pub(crate) fn logo(&self) -> &[u8] {
        &self.logo
    }
    pub(crate) fn logo_content_type(&self) -> &'static str {
        self.logo_content_type
    }
    pub(crate) fn favicon(&self) -> &[u8] {
        &self.favicon
    }
    pub(crate) fn favicon_content_type(&self) -> &'static str {
        self.favicon_content_type
    }
}

fn render_index(title: &str) -> Vec<u8> {
    INDEX.replace("{{TITLE}}", &escape_html(title)).into_bytes()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn read_asset(path: &Path, kind: &'static str, maximum: u64) -> Result<Vec<u8>, AssetError> {
    if !path.is_absolute() {
        return Err(AssetError::InvalidFile { kind });
    }
    let metadata = fs::metadata(path).map_err(|source| AssetError::Read { kind, source })?;
    if !metadata.is_file() {
        return Err(AssetError::InvalidFile { kind });
    }
    if metadata.mode() & 0o002 != 0 {
        return Err(AssetError::WorldWritable { kind });
    }
    if metadata.len() > maximum {
        return Err(AssetError::TooLarge { kind });
    }
    fs::read(path).map_err(|source| AssetError::Read { kind, source })
}

fn image_content_type(
    path: &Path,
    bytes: &[u8],
    kind: &'static str,
) -> Result<&'static str, AssetError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let detected = match extension.as_str() {
        "svg" if looks_like_svg(bytes) => Some("image/svg+xml"),
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some("image/png"),
        "jpg" | "jpeg" if bytes.starts_with(b"\xff\xd8\xff") => Some("image/jpeg"),
        "gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => Some("image/gif"),
        "webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            Some("image/webp")
        }
        "ico" if kind == "favicon" && bytes.starts_with(b"\0\0\x01\0") => Some("image/x-icon"),
        _ => None,
    };
    detected.ok_or(AssetError::UnsupportedType { kind })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| {
        let lowercase = text.trim_start().to_ascii_lowercase();
        (lowercase.starts_with("<svg") || lowercase.starts_with("<?xml"))
            && lowercase.contains("<svg")
            && !lowercase.contains("<script")
            && !lowercase.contains("javascript:")
            && !lowercase.contains("<foreignobject")
    })
}
