use super::tokens;
use averroes_core::work::WorkWindowLayout;

/// Native traffic lights and the sidebar toggle share a reserved titlebar row.
pub struct WindowChrome;

impl WindowChrome {
    pub const TITLEBAR_HEIGHT: f32 = 54.0;

    pub fn traffic_light_position() -> (f32, f32) {
        (
            ShellLayout::GUTTER + tokens::SPACE_16,
            ShellLayout::GUTTER + tokens::SPACE_20,
        )
    }

    pub fn sidebar_action_position(sidebar_width: f32) -> (f32, f32) {
        let x = if sidebar_width > 0.0 {
            ShellLayout::GUTTER + sidebar_width - tokens::SPACE_8 - tokens::CONTROL_ICON
        } else {
            Self::traffic_light_position().0 + 3.0 * tokens::CONTROL_SMALL + tokens::SPACE_12
        };
        let y = ShellLayout::GUTTER + (Self::TITLEBAR_HEIGHT - tokens::CONTROL_ICON) / 2.0;
        (x, y)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShellLayout {
    pub sidebar_width: f32,
    pub sidebar_gap: f32,
    pub workspace_width: f32,
    pub inspector_width: f32,
    pub inspector_slot_width: f32,
    pub reading_width: f32,
}

impl ShellLayout {
    pub const GUTTER: f32 = 9.0;
    pub const GAP: f32 = 7.0;
    pub const INSPECTOR_INSET: f32 = 8.0;
    pub const READING_MIN: f32 = 360.0;

    pub fn sidebar_material_bounds(
        self,
        viewport_height: f32,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        let height = viewport_height - 2.0 * Self::GUTTER;
        if self.sidebar_width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some(gpui::Bounds {
            origin: gpui::point(gpui::px(Self::GUTTER), gpui::px(Self::GUTTER)),
            size: gpui::size(gpui::px(self.sidebar_width), gpui::px(height)),
        })
    }

    pub fn resolve(width: f32, requested: WorkWindowLayout, inspector_open: bool) -> Self {
        let requested = requested.normalized();
        let available = (width - 2.0 * Self::GUTTER).max(0.0);
        let mut sidebar = if requested.sidebar_visible {
            requested.sidebar_width as f32
        } else {
            0.0
        };
        let mut gap = if sidebar > 0.0 { Self::GAP } else { 0.0 };
        let reserve = Self::READING_MIN
            + if inspector_open {
                WorkWindowLayout::INSPECTOR_MIN as f32 + 2.0 * Self::INSPECTOR_INSET
            } else {
                0.0
            };
        let limit = available - gap - reserve;
        if sidebar > limit {
            if limit >= WorkWindowLayout::SIDEBAR_MIN as f32 {
                sidebar = limit;
            } else {
                sidebar = 0.0;
                gap = 0.0;
            }
        }
        let workspace = (available - sidebar - gap).max(0.0);
        let inspector = if inspector_open {
            (requested.inspector_width as f32)
                .min((workspace - Self::READING_MIN - 2.0 * Self::INSPECTOR_INSET).max(0.0))
        } else {
            0.0
        };
        let slot = if inspector > 0.0 {
            inspector + 2.0 * Self::INSPECTOR_INSET
        } else {
            0.0
        };
        Self {
            sidebar_width: sidebar,
            sidebar_gap: gap,
            workspace_width: workspace,
            inspector_width: inspector,
            inspector_slot_width: slot,
            reading_width: (workspace - slot).max(0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeTarget {
    Sidebar,
    Inspector,
}

#[derive(Debug, Clone, Copy)]
pub struct ResizeGesture {
    pub target: ResizeTarget,
    initial_width: f32,
    start_x: f32,
}

impl ResizeGesture {
    pub fn new(target: ResizeTarget, initial_width: f32, start_x: f32) -> Self {
        Self {
            target,
            initial_width,
            start_x,
        }
    }

    pub fn width_at(self, pointer_x: f32) -> i32 {
        resized_width(self.target, self.initial_width, pointer_x - self.start_x)
    }
}

pub fn resized_width(target: ResizeTarget, initial: f32, delta_x: f32) -> i32 {
    let width = initial
        + match target {
            ResizeTarget::Sidebar => delta_x,
            ResizeTarget::Inspector => -delta_x,
        };
    let (min, max) = match target {
        ResizeTarget::Sidebar => (WorkWindowLayout::SIDEBAR_MIN, WorkWindowLayout::SIDEBAR_MAX),
        ResizeTarget::Inspector => (
            WorkWindowLayout::INSPECTOR_MIN,
            WorkWindowLayout::INSPECTOR_MAX,
        ),
    };
    (width.round() as i32).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use averroes_core::work::WorkWindowLayout;

    #[test]
    fn titlebar_actions_clear_native_traffic_lights_when_sidebar_is_hidden() {
        let (x, y) = WindowChrome::traffic_light_position();
        for button_size in [12.0, 14.0, 16.0] {
            assert!(
                y + button_size + tokens::SPACE_12
                    <= ShellLayout::GUTTER + WindowChrome::TITLEBAR_HEIGHT
            );
            for sidebar in [0.0, 220.0, 246.0, 380.0] {
                let (toggle_x, toggle_y) = WindowChrome::sidebar_action_position(sidebar);
                assert!(
                    (toggle_y + tokens::CONTROL_ICON / 2.0 - (y + button_size / 2.0)).abs() <= 1.0
                );
                let traffic_right = x + 3.0 * button_size + 2.0 * tokens::SPACE_8;
                assert!(toggle_x >= traffic_right + tokens::SPACE_12);
                if sidebar > 0.0 {
                    assert!(
                        toggle_x + tokens::CONTROL_ICON
                            <= ShellLayout::GUTTER + sidebar - tokens::SPACE_8
                    );
                }
            }
        }
    }

    #[test]
    fn supported_widths_preserve_reading_space_and_requested_sizes() {
        for width in [980.0, 1100.0, 1440.0, 2200.0] {
            for sidebar_width in [220, 246, 380] {
                for inspector_width in [320, 356, 700] {
                    let requested = WorkWindowLayout {
                        sidebar_width,
                        inspector_width,
                        sidebar_visible: true,
                    };
                    let layout = ShellLayout::resolve(width, requested, true);
                    assert!(layout.reading_width >= 360.0);
                    assert!(layout.inspector_width >= 320.0);
                    assert!(layout.sidebar_width >= 220.0);
                    assert!(
                        (layout.sidebar_width
                            + layout.sidebar_gap
                            + layout.workspace_width
                            + 2.0 * ShellLayout::GUTTER
                            - width)
                            .abs()
                            < 0.01
                    );
                    let restored = ShellLayout::resolve(2200.0, requested, true);
                    assert_eq!(restored.sidebar_width, sidebar_width as f32);
                    assert_eq!(restored.inspector_width, inspector_width as f32);
                }
            }
        }
    }

    #[test]
    fn hidden_panels_return_space_to_the_conversation() {
        let requested = WorkWindowLayout {
            sidebar_visible: false,
            ..Default::default()
        };
        let layout = ShellLayout::resolve(980.0, requested, false);
        assert_eq!(layout.sidebar_width, 0.0);
        assert_eq!(layout.inspector_width, 0.0);
        assert_eq!(layout.reading_width, 980.0 - 2.0 * ShellLayout::GUTTER);
    }

    #[test]
    fn floating_inspector_reserves_spacing_without_losing_reading_space() {
        for width in [980.0, 1440.0] {
            for sidebar_visible in [false, true] {
                let requested = WorkWindowLayout {
                    sidebar_visible,
                    ..Default::default()
                };
                let layout = ShellLayout::resolve(width, requested, true);
                assert!(layout.reading_width >= ShellLayout::READING_MIN);
                let external_space =
                    layout.workspace_width - layout.reading_width - layout.inspector_width;
                assert!(external_space > 0.0);
                let closed = ShellLayout::resolve(width, requested, false);
                assert_eq!(closed.workspace_width, closed.reading_width);
            }
        }
    }

    #[test]
    fn resize_uses_opposite_edges_and_bounded_requested_widths() {
        assert_eq!(resized_width(ResizeTarget::Sidebar, 246.0, 30.0), 276);
        assert_eq!(resized_width(ResizeTarget::Inspector, 356.0, 30.0), 326);
        assert_eq!(resized_width(ResizeTarget::Inspector, 356.0, -1000.0), 700);
        assert_eq!(resized_width(ResizeTarget::Sidebar, 246.0, -1000.0), 220);
    }

    #[test]
    fn resize_gesture_does_not_jump_when_panels_are_inset() {
        for start_x in [246.0, 396.0, 915.0, 1600.0] {
            let sidebar = ResizeGesture::new(ResizeTarget::Sidebar, 246.0, start_x);
            assert_eq!(sidebar.width_at(start_x), 246);
            assert_eq!(sidebar.width_at(start_x + 20.0), 266);
            let inspector = ResizeGesture::new(ResizeTarget::Inspector, 420.0, start_x);
            assert_eq!(inspector.width_at(start_x), 420);
            assert_eq!(inspector.width_at(start_x + 20.0), 400);
        }
    }

    #[test]
    fn native_sidebar_material_is_inset_and_releases_the_hidden_pane() {
        let layout = ShellLayout::resolve(1440.0, WorkWindowLayout::default(), false);
        let bounds = layout.sidebar_material_bounds(900.0).unwrap();
        assert!(f32::from(bounds.origin.x) > 0.0);
        assert!(f32::from(bounds.origin.y) > 0.0);
        assert_eq!(f32::from(bounds.size.width), layout.sidebar_width);
        assert!(f32::from(bounds.bottom_right().y) < 900.0);
        assert!(layout.sidebar_width + layout.workspace_width < 1440.0);
        let hidden = ShellLayout::resolve(
            1440.0,
            WorkWindowLayout {
                sidebar_visible: false,
                ..Default::default()
            },
            false,
        );
        assert!(hidden.sidebar_material_bounds(900.0).is_none());
    }
}
