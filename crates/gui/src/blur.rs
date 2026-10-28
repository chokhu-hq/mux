use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;

pub struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore WlCompositor);
wayland_client::delegate_noop!(State: ignore WlRegion);
wayland_client::delegate_noop!(State: ignore ExtBackgroundEffectManagerV1);
wayland_client::delegate_noop!(State: ignore ExtBackgroundEffectSurfaceV1);

// the compositor's blur behind part of the window, kept for the window's lifetime
pub struct Blur {
    connection: Connection,
    queue: EventQueue<State>,
    compositor: WlCompositor,
    effect: ExtBackgroundEffectSurfaceV1,
    // the sidebar width and window height the region was last set for
    shape: Option<(i32, i32)>,
}

// asks the compositor for background blur through the standard ext-background-effect protocol
pub fn request(window: &(impl HasWindowHandle + HasDisplayHandle)) -> Result<Blur, String> {
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    let display = window.display_handle().map_err(|e| e.to_string())?;
    let (RawWindowHandle::Wayland(handle), RawDisplayHandle::Wayland(display)) =
        (handle.as_raw(), display.as_raw())
    else {
        return Err("blur needs a wayland session".to_owned());
    };

    // the pointers come from the live window that winit owns, so they stay valid while it does
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let surface_id =
        unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }
            .map_err(|e| e.to_string())?;
    let surface = WlSurface::from_id(&connection, surface_id).map_err(|e| e.to_string())?;

    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let manager: ExtBackgroundEffectManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "the compositor doesn't offer background blur".to_owned())?;
    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).map_err(|e| e.to_string())?;
    let effect = manager.get_background_effect(&surface, &qh, ());
    queue.roundtrip(&mut State).map_err(|e| e.to_string())?;
    Ok(Blur {
        connection,
        queue,
        compositor,
        effect,
        shape: None,
    })
}

impl Blur {
    // blurs behind the sidebar only, leaving out the rounded corners so no square blur peeks past them
    pub fn fit(&mut self, sidebar: f32, height: f32, corner: f32) {
        let shape = (sidebar.round() as i32, height.round() as i32);
        if self.shape == Some(shape) {
            return;
        }
        self.shape = Some(shape);
        let (w, h) = shape;
        let r = corner.round() as i32;
        if w < 1 || h < 1 {
            self.effect.set_blur_region(None);
        } else {
            let region = self.compositor.create_region(&self.queue.handle(), ());
            region.add(0, r, w, (h - 2 * r).max(0));
            region.add(r, 0, (w - r).max(0), r);
            region.add(r, h - r, (w - r).max(0), r);
            self.effect.set_blur_region(Some(&region));
            region.destroy();
        }
        // applied with the window's next frame, which the renderer is about to send anyway
        let _ = self.connection.flush();
    }
}
