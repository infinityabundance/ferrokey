//! `zwlr_layer_shell_v1` layer-surface creation.
//!
//! The OSK is created as a layer surface with:
//!
//! ```text
//! layer = overlay
//! anchor = bottom | left   (margins drive free positioning)
//! keyboard_interactivity = none
//! ```
//!
//! The protocol explicitly states that `keyboard_interactivity = none` means
//! the compositor *should never assign keyboard focus to the surface*, while
//! layer surfaces keep receiving pointer/touch events. That is precisely the
//! OSK primitive Ferrokey needs: the focused target stays focused, Ferrokey
//! stays interactive, and injected keys land in the target.
//!
//! Anchoring is deliberately a SINGLE corner (`bottom | left`) rather than
//! an edge span: with a corner anchor the surface's position is fully
//! determined by `set_margin`, so the app can own and move the OSK
//! (interactive drag) and always knows where it is. An edge-span anchor
//! (bottom | left | right) would leave placement to the compositor and make
//! the current position unknowable.

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

/// The layer-shell namespace Ferrokey registers under.
pub const NAMESPACE: &str = "ferrokey-osk";

/// Create the layer surface on `surface` with the OSK semantics:
/// overlay layer, anchored to the bottom-left corner (positioned by
/// margins), and `keyboard_interactivity = none`.
///
/// `output` may be `None` (the compositor picks the output; anchored edges
/// then apply to all outputs).
pub fn create_layer_surface<D>(
    layer_shell: &ZwlrLayerShellV1,
    surface: &WlSurface,
    output: Option<&WlOutput>,
    width: u32,
    height: u32,
    qh: &QueueHandle<D>,
) -> ZwlrLayerSurfaceV1
where
    D: Dispatch<ZwlrLayerSurfaceV1, ()> + 'static,
{
    let layer_surface = layer_shell.get_layer_surface(
        surface,
        output,
        Layer::Overlay,
        NAMESPACE.to_string(),
        qh,
        (),
    );
    layer_surface.set_anchor(Anchor::Bottom | Anchor::Left);
    layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer_surface.set_size(width, height);
    // Default position: flush to the bottom-left corner until the output
    // size is known; WaylandSurface::connect re-centers once it arrives.
    layer_surface.set_margin(0, 0, 0, 0);
    layer_surface
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_constants_are_stable() {
        assert_eq!(NAMESPACE, "ferrokey-osk");
        // Sanity: the protocol constants we rely on exist and are distinct.
        assert_ne!(Anchor::Bottom.bits(), 0);
        assert_ne!(Layer::Overlay as u32, Layer::Background as u32);
        assert_ne!(
            KeyboardInteractivity::None as u32,
            KeyboardInteractivity::Exclusive as u32
        );
    }
}
