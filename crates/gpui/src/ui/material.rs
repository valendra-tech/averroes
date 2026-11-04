use averroes_core::config::{AppearanceSection, MaterialPreference, MotionPreference};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialMode {
    SidebarVibrancy,
    Opaque,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemVisualOptions {
    pub reduce_transparency: bool,
    pub reduce_motion: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveVisuals {
    pub material: MaterialMode,
    pub reduced_motion: bool,
    pub window_background: gpui::WindowBackgroundAppearance,
}

impl gpui::Global for EffectiveVisuals {}

impl EffectiveVisuals {
    pub const WINDOW_BACKGROUND: gpui::WindowBackgroundAppearance =
        gpui::WindowBackgroundAppearance::Blurred;

    pub fn resolve(preference: AppearanceSection, system: SystemVisualOptions) -> Self {
        let material =
            if system.reduce_transparency || preference.material == MaterialPreference::Opaque {
                MaterialMode::Opaque
            } else {
                MaterialMode::SidebarVibrancy
            };
        Self {
            material,
            reduced_motion: system.reduce_motion || preference.motion == MotionPreference::Reduced,
            window_background: if material == MaterialMode::Opaque {
                gpui::WindowBackgroundAppearance::Opaque
            } else {
                Self::WINDOW_BACKGROUND
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use averroes_core::config::{AppearanceSection, MaterialPreference, MotionPreference};

    #[test]
    fn accessibility_reduction_wins_over_automatic_material() {
        let system = SystemVisualOptions {
            reduce_transparency: true,
            reduce_motion: true,
        };
        let effective = EffectiveVisuals::resolve(AppearanceSection::default(), system);
        assert_eq!(effective.material, MaterialMode::Opaque);
        assert!(effective.reduced_motion);
    }

    #[test]
    fn material_and_motion_preferences_are_independent() {
        let system = SystemVisualOptions::default();
        let effective = EffectiveVisuals::resolve(
            AppearanceSection {
                material: MaterialPreference::Opaque,
                motion: MotionPreference::FollowSystem,
            },
            system,
        );
        assert_eq!(effective.material, MaterialMode::Opaque);
        assert!(!effective.reduced_motion);
        let effective = EffectiveVisuals::resolve(
            AppearanceSection {
                material: MaterialPreference::Automatic,
                motion: MotionPreference::Reduced,
            },
            system,
        );
        assert_eq!(effective.material, MaterialMode::SidebarVibrancy);
        assert!(effective.reduced_motion);
        assert_eq!(
            EffectiveVisuals::resolve(AppearanceSection::default(), SystemVisualOptions::default())
                .material,
            MaterialMode::SidebarVibrancy
        );
    }

    #[test]
    fn automatic_blurs_the_common_base_and_opacity_overrides_disable_it() {
        for theme in [crate::ui::UiTheme::light(), crate::ui::UiTheme::dark()] {
            let system = SystemVisualOptions::default();
            let automatic = AppearanceSection::default();
            let visual = EffectiveVisuals::resolve(automatic, system);
            let mode = visual.material;
            assert_eq!(
                theme.navigation_fill(mode).a,
                0.0,
                "Automatic must leave opacity to AppKit"
            );
            assert_eq!(
                visual.window_background,
                gpui::WindowBackgroundAppearance::Blurred
            );
            assert_eq!(
                visual.window_background,
                EffectiveVisuals::WINDOW_BACKGROUND
            );
            assert!(theme.canvas_fill(mode).a > 0.0 && theme.canvas_fill(mode).a < 1.0);

            let opaque = AppearanceSection {
                material: MaterialPreference::Opaque,
                ..automatic
            };
            let visual = EffectiveVisuals::resolve(opaque, system);
            assert_eq!(theme.navigation_fill(visual.material).a, 1.0);
            assert_eq!(
                visual.window_background,
                gpui::WindowBackgroundAppearance::Opaque
            );
            assert_eq!(theme.canvas_fill(visual.material).a, 1.0);

            let system = SystemVisualOptions {
                reduce_transparency: true,
                ..system
            };
            let visual = EffectiveVisuals::resolve(automatic, system);
            assert_eq!(theme.navigation_fill(visual.material).a, 1.0);
            assert_eq!(
                visual.window_background,
                gpui::WindowBackgroundAppearance::Opaque
            );
            assert_eq!(theme.canvas_fill(visual.material).a, 1.0);
        }
    }

    #[test]
    fn automatic_navigation_leaves_native_material_visible() {
        for theme in [crate::ui::UiTheme::light(), crate::ui::UiTheme::dark()] {
            assert_eq!(
                theme.navigation_fill(MaterialMode::SidebarVibrancy).a,
                0.0,
                "Navigation must not paint over the system material"
            );
            assert_eq!(theme.navigation_fill(MaterialMode::Opaque).a, 1.0);
        }
    }
}
