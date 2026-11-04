pub use averroes_core::branding::{APP_NAME, HTTP_CLIENT_NAME};

// Keep the established native identity so existing Keychain/privacy grants
// and persisted application state remain associated with this installation.
pub const BUNDLE_ID: &str = "com.valendra.averroes";
pub const APP_LOGO_ASSET: &str = "brand/qdivzero-harness.png";
pub const APP_LOGO_DARK_ASSET: &str = "brand/qdivzero-harness-dark.png";

/// The interface uses the unframed horizontal mark, not the macOS Dock tile.
pub fn logo(cx: &gpui::App, width: f32) -> gpui::AnyElement {
    use gpui::{IntoElement, Styled, StyledImage};
    let asset = if crate::ui::UiTheme::is_dark(cx) {
        APP_LOGO_DARK_ASSET
    } else {
        APP_LOGO_ASSET
    };
    gpui::img(asset)
        .w(gpui::px(width))
        .h(gpui::px(width * 833.0 / 1587.0))
        .object_fit(gpui::ObjectFit::Contain)
        .flex_none()
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_icon_decodes_with_transparent_corners_and_the_supplied_blue_mark() {
        let image =
            image::load_from_memory(include_bytes!("../../../assets/qdivzero-harness-icon.png"))
                .unwrap()
                .to_rgba8();
        assert_eq!(image.dimensions(), (1024, 1024));
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert!(image
            .pixels()
            .any(|pixel| pixel[3] > 200 && pixel[2] as u16 > pixel[0] as u16 + 40));
        assert!(include_bytes!("../../../assets/QDivZeroHarness.icns").starts_with(b"icns"));
    }
}
