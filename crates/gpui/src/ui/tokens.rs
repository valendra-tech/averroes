use gpui::FontWeight;

// Type scale (macOS base body is 13 px).
pub const TEXT_CAPTION: f32 = 11.0;
pub const TEXT_SMALL: f32 = 12.0;
pub const TEXT_BODY: f32 = 13.0;
pub const TEXT_TITLE3: f32 = 15.0;
pub const TEXT_TITLE2: f32 = 17.0;
pub const TEXT_TITLE1: f32 = 20.0;
pub const TEXT_LARGE: f32 = 24.0;

pub const WEIGHT_MEDIUM: FontWeight = FontWeight::MEDIUM;
pub const WEIGHT_SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;

// Spacing grid: the suffix is the pixel value.
pub const SPACE_2: f32 = 2.0;
pub const SPACE_4: f32 = 4.0;
pub const SPACE_6: f32 = 6.0;
pub const SPACE_8: f32 = 8.0;
pub const SPACE_12: f32 = 12.0;
pub const SPACE_16: f32 = 16.0;
pub const SPACE_20: f32 = 20.0;
pub const SPACE_24: f32 = 24.0;
pub const SPACE_32: f32 = 32.0;

pub const RADIUS_CONTROL: f32 = 6.0;
pub const RADIUS_CARD: f32 = 10.0;
pub const RADIUS_SHEET: f32 = 14.0;

pub const CONTROL_SMALL: f32 = 22.0;
pub const CONTROL_REGULAR: f32 = 28.0;
pub const CONTROL_LARGE: f32 = 32.0;
pub const ROW_HEIGHT: f32 = 32.0;
pub const NAV_HEIGHT: f32 = 36.0;
pub const TOOLBAR_HEIGHT: f32 = 44.0;

pub const SIDEBAR_WIDTH: f32 = 300.0;
pub const SIDEBAR_GUTTER: f32 = 12.0;
pub const ICON_SIZE: f32 = 16.0;
pub const ICON_SMALL: f32 = 13.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_scale_is_strictly_increasing() {
        let scale = [
            TEXT_CAPTION,
            TEXT_SMALL,
            TEXT_BODY,
            TEXT_TITLE3,
            TEXT_TITLE2,
            TEXT_TITLE1,
            TEXT_LARGE,
        ];
        for pair in scale.windows(2) {
            assert!(pair[0] < pair[1], "type scale must be strictly increasing");
        }
        assert_eq!(TEXT_BODY, 13.0);
    }

    #[test]
    fn spacing_grid_is_strictly_increasing() {
        let scale = [
            SPACE_2, SPACE_4, SPACE_6, SPACE_8, SPACE_12, SPACE_16, SPACE_20, SPACE_24, SPACE_32,
        ];
        for pair in scale.windows(2) {
            assert!(
                pair[0] < pair[1],
                "spacing scale must be strictly increasing"
            );
        }
    }

    #[test]
    fn shape_tokens_match_the_spec() {
        assert_eq!(RADIUS_CONTROL, 6.0);
        assert_eq!(RADIUS_CARD, 10.0);
        assert_eq!(RADIUS_SHEET, 14.0);
        assert_eq!(ROW_HEIGHT, 32.0);
        assert_eq!(TOOLBAR_HEIGHT, 44.0);
        assert_eq!(SIDEBAR_WIDTH, 300.0);
    }
}
