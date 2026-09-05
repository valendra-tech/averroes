use gpui::{ease_out_quint, Animation, AnimationElement, AnimationExt, Div, ElementId, Styled};
use std::time::Duration;

pub const MESSAGE_FADE_DURATION: Duration = Duration::from_millis(220);
pub const ATTACHMENT_FADE_DURATION: Duration = Duration::from_millis(180);
pub const STREAM_LINE_FADE_DURATION: Duration = Duration::from_millis(140);
pub const STATE_FADE_DURATION: Duration = Duration::from_millis(160);

pub fn fade_opacity(delta: f32) -> f32 {
    delta.clamp(0.0, 1.0)
}

pub fn fade_in(
    element: Div,
    id: impl Into<ElementId>,
    duration: Duration,
) -> AnimationElement<Div> {
    element.with_animation(
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        |element, delta| element.opacity(fade_opacity(delta)),
    )
}

#[cfg(test)]
mod tests {
    use super::{fade_opacity, ATTACHMENT_FADE_DURATION, STREAM_LINE_FADE_DURATION};
    use std::time::Duration;

    #[test]
    fn uses_short_durations_for_subtle_content_motion() {
        assert_eq!(ATTACHMENT_FADE_DURATION, Duration::from_millis(180));
        assert_eq!(STREAM_LINE_FADE_DURATION, Duration::from_millis(140));
    }

    #[test]
    fn fade_opacity_stays_between_start_and_end() {
        assert_eq!(fade_opacity(0.0), 0.0);
        assert_eq!(fade_opacity(1.0), 1.0);
        assert!((0.0..=1.0).contains(&fade_opacity(0.5)));
    }
}
