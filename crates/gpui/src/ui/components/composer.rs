use crate::ui::tokens;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComposerMetrics {
    pub max_width: f32,
    pub surface_radius: f32,
    pub text_min_height: f32,
    pub footer_height: f32,
    pub footer_horizontal_padding: f32,
    pub footer_bottom_padding: f32,
    pub control_gap: f32,
    pub send_size: f32,
    pub attachment_radius: f32,
    pub empty_logo_size: f32,
    pub empty_title_size: f32,
    pub empty_brand_gap: f32,
    pub empty_composer_gap: f32,
    pub footer_text_size: f32,
    pub model_width: f32,
    pub reasoning_width: f32,
    pub security_width: f32,
    pub privacy_icon_size: f32,
}

pub fn composer_metrics(compact: bool) -> ComposerMetrics {
    ComposerMetrics {
        max_width: if compact { 680.0 } else { 760.0 },
        surface_radius: tokens::RADIUS_SHEET,
        text_min_height: 76.0,
        footer_height: 46.0,
        footer_horizontal_padding: tokens::SPACE_12,
        footer_bottom_padding: tokens::SPACE_12,
        control_gap: tokens::SPACE_6,
        send_size: tokens::CONTROL_REGULAR,
        attachment_radius: tokens::RADIUS_CONTROL,
        empty_logo_size: 96.0,
        empty_title_size: tokens::TEXT_LARGE,
        empty_brand_gap: tokens::SPACE_8,
        empty_composer_gap: tokens::SPACE_24,
        footer_text_size: if compact {
            tokens::TEXT_CAPTION
        } else {
            tokens::TEXT_SMALL
        },
        model_width: if compact { 132.0 } else { 148.0 },
        reasoning_width: if compact { 60.0 } else { 68.0 },
        security_width: if compact { 94.0 } else { 108.0 },
        privacy_icon_size: tokens::ICON_SIZE,
    }
}
