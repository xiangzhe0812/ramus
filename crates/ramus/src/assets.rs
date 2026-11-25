//! Files the app carries inside its binary: the icons and the fonts.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// The icons (a solid set), compiled in so the app is a single file with nothing to find at run time.
const ICONS: [(&str, &[u8]); 7] = [
    (
        "icons/repository.svg",
        include_bytes!("../assets/icons/repository.svg"),
    ),
    (
        "icons/worktree.svg",
        include_bytes!("../assets/icons/worktree.svg"),
    ),
    (
        "icons/branch.svg",
        include_bytes!("../assets/icons/branch.svg"),
    ),
    (
        "icons/search.svg",
        include_bytes!("../assets/icons/search.svg"),
    ),
    (
        "icons/clone.svg",
        include_bytes!("../assets/icons/clone.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../assets/icons/close.svg"),
    ),
    (
        "icons/check.svg",
        include_bytes!("../assets/icons/check.svg"),
    ),
];

/// The fonts, compiled in for the same reason: Inter for the interface, JetBrains Mono for
/// code, branch names and hashes. Both are under the SIL Open Font License; the licence
/// texts sit next to the files in `assets/fonts`.
const FONTS: [&[u8]; 6] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
];

/// Every bundled font, to register with the text system at start-up.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect()
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_and_is_an_svg() {
        for (name, _) in ICONS {
            let bytes = Assets.load(name).unwrap().expect(name);
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.starts_with("<svg"), "{name}");
        }
    }

    #[test]
    fn an_unknown_path_is_not_an_error() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len());
    }

    #[test]
    fn the_solid_icon_set_is_bundled() {
        for name in [
            "repository",
            "worktree",
            "branch",
            "search",
            "clone",
            "close",
            "check",
        ] {
            let path = format!("icons/{name}.svg");
            assert!(Assets.load(&path).unwrap().is_some(), "{path} is missing");
        }
    }

    #[test]
    fn the_ui_and_code_fonts_are_bundled() {
        let fonts = fonts();
        assert_eq!(fonts.len(), 6);
        assert!(fonts.iter().all(|bytes| bytes.starts_with(&[0, 1, 0, 0])));
    }
}
