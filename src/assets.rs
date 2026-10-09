use anyhow::anyhow;
use gpui_kit::{AssetSource, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
struct IconAssets;

#[derive(RustEmbed)]
#[folder = "fonts"]
struct FontAssets;

/// Combined asset source: jot's fonts and icons, then GPUI Kit's icon set.
pub struct Assets;

impl Assets {
    fn load_from_icons(path: &str) -> Option<Cow<'static, [u8]>> {
        IconAssets::get(path).map(|f| f.data)
    }

    fn load_from_fonts(path: &str) -> Option<Cow<'static, [u8]>> {
        if let Some(font_path) = path.strip_prefix("fonts/") {
            FontAssets::get(font_path).map(|f| f.data)
        } else {
            None
        }
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>, anyhow::Error> {
        if path.is_empty() {
            return Ok(None);
        }

        if let Some(data) = Self::load_from_fonts(path) {
            return Ok(Some(data));
        }

        if let Some(data) = Self::load_from_icons(path) {
            return Ok(Some(data));
        }

        if let Ok(Some(data)) = gpui_kit::assets::Assets.load(path) {
            return Ok(Some(data));
        }

        Err(anyhow!("Could not find asset at path \"{}\"", path))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>, anyhow::Error> {
        let mut results: Vec<SharedString> = Vec::new();

        results.extend(
            IconAssets::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| p.into()),
        );

        for kit_path in gpui_kit::assets::Assets.list(path)? {
            if !results.contains(&kit_path) {
                results.push(kit_path);
            }
        }

        let fonts_prefix = "fonts/";
        if path.is_empty() || fonts_prefix.starts_with(path) || path.starts_with(fonts_prefix) {
            let search_path = path.strip_prefix(fonts_prefix).unwrap_or("");
            results.extend(
                FontAssets::iter()
                    .filter(|p| p.starts_with(search_path))
                    .map(|p| format!("fonts/{}", p).into()),
            );
        }

        Ok(results)
    }
}
