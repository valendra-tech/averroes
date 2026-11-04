//! AppKit material ownership. All view operations stay on GPUI's main thread.
use super::material::{MaterialMode, SystemVisualOptions};
use block2::RcBlock;
use gpui::{Bounds, Pixels, Window};
use objc2::{rc::Retained, runtime::ProtocolObject, MainThreadMarker};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSTitlebarSeparatorStyle, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    NSWorkspace, NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue, NSPoint, NSRect,
    NSSize,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::ptr::NonNull;

pub fn system_visual_options() -> SystemVisualOptions {
    let workspace = NSWorkspace::sharedWorkspace();
    SystemVisualOptions {
        reduce_transparency: workspace.accessibilityDisplayShouldReduceTransparency(),
        reduce_motion: workspace.accessibilityDisplayShouldReduceMotion(),
    }
}

fn native_view(window: &Window) -> Option<&NSView> {
    MainThreadMarker::new()?;
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI supplies its live AppKit view. The reference cannot outlive
    // the borrowed Window, and the main-thread check precedes dereferencing it.
    Some(unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() })
}

/// GPUI owns the common window backdrop and its background color. Keep the
/// native outer shadow and titlebar treatment without overriding that material.
pub fn configure_window(window: &Window) {
    if let Some(native_window) = native_view(window).and_then(NSView::window) {
        native_window.setHasShadow(true);
        native_window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
    }
}

/// A semantic sidebar material, bounded to its pane and owned on the main thread.
#[derive(Default)]
pub struct SidebarMaterialHost {
    view: Option<Retained<NSVisualEffectView>>,
    radius: Option<f32>,
}

impl SidebarMaterialHost {
    pub fn update(
        &mut self,
        window: &Window,
        mode: MaterialMode,
        bounds: Option<Bounds<Pixels>>,
        radius: f32,
    ) -> bool {
        if mode == MaterialMode::Opaque || bounds.is_none() {
            self.clear();
            return true;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some(native) = native_view(window) else {
            return false;
        };
        // SAFETY: GPUI's native view belongs to an AppKit NSView hierarchy.
        let Some(parent) = (unsafe { native.superview() }) else {
            return false;
        };
        let bounds = bounds.expect("visible sidebar bounds checked above");
        let source = native.bounds();
        let width = f32::from(bounds.size.width) as f64;
        let height = f32::from(bounds.size.height) as f64;
        let top = f32::from(bounds.origin.y) as f64;
        let y = if native.isFlipped() {
            source.origin.y + top
        } else {
            source.origin.y + source.size.height - top - height
        };
        let frame = native.convertRect_toView(
            NSRect::new(
                NSPoint::new(source.origin.x + f32::from(bounds.origin.x) as f64, y),
                NSSize::new(width, height),
            ),
            Some(&parent),
        );
        let view = self.view.get_or_insert_with(|| {
            let view = NSVisualEffectView::new(mtm);
            view.setMaterial(NSVisualEffectMaterial::Sidebar);
            view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            view.setState(NSVisualEffectState::FollowsWindowActiveState);
            view
        });
        view.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
        // SAFETY: this view was created and attached as an AppKit NSView above.
        let same_parent = unsafe { view.superview() }
            .as_deref()
            .is_some_and(|current| std::ptr::eq(current, &*parent));
        if !same_parent {
            view.removeFromSuperview();
            // Keep semantic sidebar material above GPUI's full-window blur,
            // while both remain behind its Metal-rendered text and controls.
            parent.addSubview_positioned_relativeTo(
                view,
                NSWindowOrderingMode::Below,
                Some(native),
            );
        }
        if view.frame() != frame {
            view.setFrame(frame);
        }
        if self.radius != Some(radius) {
            view.setWantsLayer(true);
            if let Some(layer) = view.layer() {
                layer.setCornerRadius(radius as f64);
                layer.setMasksToBounds(true);
                self.radius = Some(radius);
            }
        }
        true
    }

    pub fn clear(&mut self) {
        if let Some(view) = self.view.take() {
            view.removeFromSuperview();
        }
        self.radius = None;
    }
}

impl Drop for SidebarMaterialHost {
    fn drop(&mut self) {
        self.clear();
    }
}

pub struct AccessibilityObserver {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    _block: RcBlock<dyn Fn(NonNull<NSNotification>)>,
}

pub fn observe_system_visual_options(
    on_change: impl Fn() + Send + Sync + 'static,
) -> AccessibilityObserver {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let queue = NSOperationQueue::mainQueue();
    let block: RcBlock<dyn Fn(NonNull<NSNotification>)> = RcBlock::new(move |_| on_change());
    // SAFETY: the callback is sendable; delivery uses the main queue. The token
    // and block are owned until removal from this exact notification center.
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
            None,
            Some(&queue),
            &block,
        )
    };
    AccessibilityObserver {
        center,
        token,
        _block: block,
    }
}

impl Drop for AccessibilityObserver {
    fn drop(&mut self) {
        // SAFETY: token was registered with this notification center above.
        unsafe {
            self.center
                .removeObserver(AsRef::<objc2::runtime::AnyObject>::as_ref(&*self.token));
        }
    }
}
