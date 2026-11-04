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
    pub empty_logo_width: f32,
    pub empty_title_size: f32,
    pub empty_brand_gap: f32,
    pub empty_composer_gap: f32,
    pub footer_text_size: f32,
    pub model_width: f32,
    pub reasoning_width: f32,
    pub privacy_icon_size: f32,
}

pub fn composer_metrics(compact: bool, available: f32) -> ComposerMetrics {
    let (model_width, reasoning_width) = composer_control_widths(available);
    ComposerMetrics {
        max_width: if compact { 680.0 } else { 760.0 },
        surface_radius: tokens::RADIUS_COMPOSER,
        text_min_height: 60.0,
        footer_height: 44.0,
        footer_horizontal_padding: tokens::SPACE_12,
        footer_bottom_padding: tokens::SPACE_12,
        control_gap: tokens::SPACE_6,
        send_size: tokens::CONTROL_SEND,
        attachment_radius: tokens::RADIUS_CONTROL,
        empty_logo_width: 144.0,
        empty_title_size: tokens::TEXT_LARGE,
        empty_brand_gap: tokens::SPACE_8,
        empty_composer_gap: tokens::SPACE_24,
        footer_text_size: if compact {
            tokens::TEXT_CAPTION
        } else {
            tokens::TEXT_SMALL
        },
        model_width: model_width.min(if compact { 210.0 } else { 240.0 }),
        reasoning_width,
        privacy_icon_size: tokens::ICON_SIZE,
    }
}

fn composer_control_widths(available: f32) -> (f32, f32) {
    let reasoning = if available < 340.0 {
        48.0
    } else if available < 500.0 {
        64.0
    } else {
        88.0
    };
    let reserved = 3.0 * tokens::CONTROL_COMPACT
        + tokens::CONTROL_SEND
        + 2.0 * tokens::SPACE_12
        + 2.0 * tokens::SPACE_4
        + 5.0 * tokens::SPACE_6
        + tokens::SPACE_2;
    let model = (available - reasoning - reserved).clamp(80.0, 240.0);
    (model, reasoning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_and_reasoning_controls_fit_beside_options_and_privacy_at_narrow_widths() {
        for available in [310.0, 400.0, 700.0, 1200.0] {
            let (model, reasoning) = composer_control_widths(available);
            assert!(model >= 80.0 && model <= 240.0);
            assert!(reasoning >= 48.0);
            // Attachment, options, privacy and Send/Stop now share the footer.
            assert!(model + reasoning + 180.0 <= available);
        }
    }
}
