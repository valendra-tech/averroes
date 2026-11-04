use gpui::{ease_out_quint, Animation, AnimationExt, ElementId, IntoElement, Styled};
use std::time::Duration;

pub const MESSAGE_FADE_DURATION: Duration = Duration::from_millis(220);
pub const ATTACHMENT_FADE_DURATION: Duration = Duration::from_millis(180);
pub const STREAM_TEXT_FADE_DURATION: Duration = Duration::from_millis(140);
pub const STATE_FADE_DURATION: Duration = Duration::from_millis(160);

pub fn fade_opacity(delta: f32) -> f32 {
    delta.clamp(0.0, 1.0)
}

pub fn reduced_motion(cx: &gpui::App) -> bool {
    cx.has_global::<super::material::EffectiveVisuals>()
        && cx
            .global::<super::material::EffectiveVisuals>()
            .reduced_motion
}

pub fn animate_if<E>(
    element: E,
    id: impl Into<ElementId>,
    animation: Animation,
    enabled: bool,
    animator: impl Fn(E, f32) -> E + 'static,
) -> gpui::AnyElement
where
    E: IntoElement + 'static,
{
    if enabled {
        element
            .with_animation(id, animation, animator)
            .into_any_element()
    } else {
        element.into_any_element()
    }
}

pub fn fade_in<E>(
    element: E,
    id: impl Into<ElementId>,
    duration: Duration,
    reduced: bool,
) -> gpui::AnyElement
where
    E: IntoElement + Styled + 'static,
{
    animate_if(
        element,
        id,
        Animation::new(duration).with_easing(ease_out_quint()),
        !reduced,
        |element, delta| element.opacity(fade_opacity(delta)),
    )
}

#[cfg(test)]
mod tests {
    use super::{fade_opacity, ATTACHMENT_FADE_DURATION, STREAM_TEXT_FADE_DURATION};
    use std::time::Duration;

    #[test]
    fn uses_short_durations_for_subtle_content_motion() {
        assert_eq!(ATTACHMENT_FADE_DURATION, Duration::from_millis(180));
        assert_eq!(STREAM_TEXT_FADE_DURATION, Duration::from_millis(140));
    }

    #[test]
    fn fade_opacity_stays_between_start_and_end() {
        assert_eq!(fade_opacity(0.0), 0.0);
        assert_eq!(fade_opacity(1.0), 1.0);
        assert!((0.0..=1.0).contains(&fade_opacity(0.5)));
    }
}
