use anyhow::anyhow;
use gpui_kit::{AssetSource, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
struct IconAssets;

/// Combined asset source: jot's icons, then GPUI Kit's icon set. The fonts
/// come from GPUI Kit's `fonts` feature.
pub struct Assets;

impl Assets {
    fn load_from_icons(path: &str) -> Option<Cow<'static, [u8]>> {
        IconAssets::get(path).map(|f| f.data)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>, anyhow::Error> {
        if path.is_empty() {
            return Ok(None);
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

        Ok(results)
    }
}
