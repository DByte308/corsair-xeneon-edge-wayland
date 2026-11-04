// SPDX-License-Identifier: GPL-3.0-or-later
//! A transparent, input-free layer-shell surface reserves the Edge's left rail.
//! No desktop configuration, display mode, pixel capture or input devices change.
use anyhow::{Context, Result, bail};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer, wl_compositor, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool,
        wl_surface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1 as shell, zwlr_layer_surface_v1 as layer,
};

const WIDTH: u32 = 96;
static RUNNING: AtomicBool = AtomicBool::new(true);
extern "C" fn stop(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}
#[derive(Clone, Debug, Default)]
struct Output {
    proxy: Option<wl_output::WlOutput>,
    name: String,
    size: Option<(i32, i32)>,
    scale: i32,
    normal_transform: bool,
}
impl Output {
    fn is_edge(&self) -> bool {
        self.size == Some((2560, 720)) && self.scale == 1 && self.normal_transform
    }
}
struct Reservation {
    output: u32,
    surface: wl_surface::WlSurface,
    role: layer::ZwlrLayerSurfaceV1,
    buffer: Option<wl_buffer::WlBuffer>,
    memory: Option<File>,
    pending: Option<(u32, u32)>,
    closed: bool,
}
#[derive(Default)]
struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    shell: Option<shell::ZwlrLayerShellV1>,
    outputs: BTreeMap<u32, Output>,
    reservation: Option<Reservation>,
}
impl State {
    fn selected(&self) -> Option<u32> {
        let matches: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| o.is_edge())
            .map(|(id, _)| *id)
            .collect();
        if matches.len() == 1 {
            Some(matches[0])
        } else {
            None
        }
    }
    fn release(&mut self) {
        if let Some(old) = self.reservation.take() {
            old.role.destroy();
            old.surface.destroy();
            if let Some(buffer) = old.buffer {
                buffer.destroy();
            }
        }
    }
    fn reconcile(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let selected = self.selected();
        if self
            .reservation
            .as_ref()
            .is_some_and(|r| Some(r.output) != selected || r.closed)
        {
            self.release();
        }
        if self.reservation.is_none() {
            let Some(id) = selected else {
                return Ok(());
            };
            let compositor = self
                .compositor
                .as_ref()
                .context("Wayland compositor is unavailable")?;
            let shell = self
                .shell
                .as_ref()
                .context("KDE layer-shell protocol is unavailable")?;
            let surface = compositor.create_surface(qh, ());
            let empty = compositor.create_region(qh, ());
            surface.set_input_region(Some(&empty));
            empty.destroy();
            let role = shell.get_layer_surface(
                &surface,
                self.outputs[&id].proxy.as_ref(),
                shell::Layer::Top,
                "edgeline-workarea".into(),
                qh,
                (),
            );
            role.set_anchor(layer::Anchor::Left | layer::Anchor::Top | layer::Anchor::Bottom);
            role.set_size(WIDTH, 0);
            role.set_exclusive_zone(WIDTH as i32);
            role.set_keyboard_interactivity(layer::KeyboardInteractivity::None);
            self.reservation = Some(Reservation {
                output: id,
                surface,
                role,
                buffer: None,
                memory: None,
                pending: None,
                closed: false,
            });
            self.reservation.as_ref().unwrap().surface.commit();
        }
        if let Some(reservation) = &mut self.reservation
            && let Some((width, height)) = reservation.pending.take()
        {
            let width = if width == 0 { WIDTH } else { width };
            let height = if height == 0 { 720 } else { height };
            if width != WIDTH || height != 720 {
                bail!("Compositor assigned unexpected reservation size {width}x{height}");
            }
            let size = (width * height * 4) as i32;
            // A newly truncated memfd is zero-filled: every pixel is transparent.
            let fd =
                unsafe { libc::memfd_create(c"edgeline-workarea".as_ptr(), libc::MFD_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // Own the successful descriptor exactly once; server gets its own copy.
            let memory = unsafe { File::from_raw_fd(fd) };
            memory.set_len(size as u64)?;
            let pool = self
                .shm
                .as_ref()
                .context("Wayland shared memory is unavailable")?
                .create_pool(memory.as_fd(), size, qh, ());
            let buffer = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                (width * 4) as i32,
                wl_shm::Format::Argb8888,
                qh,
                (),
            );
            pool.destroy();
            reservation.surface.set_buffer_scale(1);
            reservation.surface.attach(Some(&buffer), 0, 0);
            reservation
                .surface
                .damage_buffer(0, 0, width as i32, height as i32);
            reservation.surface.commit();
            if let Some(old) = reservation.buffer.replace(buffer) {
                old.destroy();
            }
            reservation.memory = Some(memory);
            println!(
                "{}",
                serde_json::json!({"reserved":true,"output":self.outputs[&reservation.output].name,"width":WIDTH,"input":false,"transparent":true})
            );
        }
        Ok(())
    }
}
impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" if version >= 4 => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    state.shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_output" => {
                    let proxy = registry.bind(name, version.min(4), qh, name);
                    state.outputs.insert(
                        name,
                        Output {
                            proxy: Some(proxy),
                            scale: 1,
                            ..Output::default()
                        },
                    );
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(output) = state.outputs.remove(&name)
                    && let Some(proxy) = output.proxy
                    && proxy.version() >= 3
                {
                    proxy.release();
                }
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_output::WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(output) = state.outputs.get_mut(id) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Geometry { transform, .. } => {
                output.normal_transform =
                    matches!(transform, WEnum::Value(wl_output::Transform::Normal))
            }
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                ..
            } if flags.contains(wl_output::Mode::Current) => output.size = Some((width, height)),
            wl_output::Event::Scale { factor } => output.scale = factor,
            _ => {}
        }
    }
}
impl Dispatch<layer::ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        role: &layer::ZwlrLayerSurfaceV1,
        event: layer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(reservation) = state.reservation.as_mut() else {
            return;
        };
        if reservation.role.id() != role.id() {
            return;
        }
        match event {
            layer::Event::Configure {
                serial,
                width,
                height,
            } => {
                role.ack_configure(serial);
                reservation.pending = Some((width, height));
            }
            layer::Event::Closed => reservation.closed = true,
            _ => {}
        }
    }
}
delegate_noop!(State:ignore wl_compositor::WlCompositor);
delegate_noop!(State:ignore wl_surface::WlSurface);
delegate_noop!(State:ignore wl_region::WlRegion);
delegate_noop!(State:ignore wl_shm::WlShm);
delegate_noop!(State:ignore wl_shm_pool::WlShmPool);
delegate_noop!(State:ignore wl_buffer::WlBuffer);
delegate_noop!(State:ignore shell::ZwlrLayerShellV1);

fn process_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    if matches!(fields.next()?, "Z" | "X") {
        return None;
    }
    fields.nth(18).map(str::to_owned)
}
fn parent_alive(parent: &Option<(u32, String)>) -> bool {
    parent
        .as_ref()
        .is_none_or(|(pid, start)| process_identity(*pid).as_ref() == Some(start))
}
fn main() -> Result<()> {
    let mut check = false;
    let mut wait = false;
    let mut parent = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--wait" => wait = true,
            "--parent-pid" => {
                let pid = args
                    .next()
                    .context("--parent-pid requires a PID")?
                    .parse::<u32>()?;
                parent = Some((
                    pid,
                    process_identity(pid).context("Parent process is not running")?,
                ));
            }
            _ => bail!("Unknown option {arg}"),
        }
    }
    // Signal handler only flips a lock-free atomic; normal control flow cleans up.
    unsafe {
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
    }
    let _lock = if check {
        None
    } else {
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?,
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(runtime.join("edgeline-workarea.lock"))?;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(error) => {
                    if !wait {
                        println!(
                            "{}",
                            serde_json::json!({"reserved":false,"reason":"another reservation owner is active"})
                        );
                        return Ok(());
                    }
                    if !RUNNING.load(Ordering::Relaxed) || !parent_alive(&parent) {
                        return Ok(());
                    }
                    if !matches!(error, std::fs::TryLockError::WouldBlock) {
                        bail!("Could not lock reservation: {error}");
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
        Some(file)
    };
    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue::<State>();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    if state.compositor.is_none() || state.shm.is_none() || state.shell.is_none() {
        bail!("Required Wayland layer-shell/compositor/shared-memory protocols are unavailable");
    }
    if check {
        println!(
            "{}",
            serde_json::json!({"supported":true,"edge":state.selected().map(|id|serde_json::json!({"name":state.outputs[&id].name,"size":[2560,720],"reservedWidth":WIDTH})),"surfaceCreated":false})
        );
        return Ok(());
    }
    while RUNNING.load(Ordering::Relaxed) && parent_alive(&parent) {
        queue.dispatch_pending(&mut state)?;
        state.reconcile(&qh)?;
        connection.flush()?;
        if let Some(read) = queue.prepare_read() {
            let mut descriptor = libc::pollfd {
                fd: read.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // A single valid pollfd is borrowed only for this call.
            let ready = unsafe { libc::poll(&mut descriptor, 1, 250) };
            if ready > 0 {
                read.read()?;
            } else if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    state.release();
    let _ = connection.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unambiguous_unscaled_edge_is_selected() {
        let mut state = State::default();
        assert_eq!(state.selected(), None);
        state.outputs.insert(
            1,
            Output {
                size: Some((3840, 2160)),
                scale: 1,
                ..Output::default()
            },
        );
        assert_eq!(state.selected(), None);
        state.outputs.insert(
            2,
            Output {
                size: Some((2560, 720)),
                scale: 2,
                normal_transform: true,
                ..Output::default()
            },
        );
        assert_eq!(state.selected(), None);
        state.outputs.get_mut(&2).unwrap().scale = 1;
        assert_eq!(state.selected(), Some(2));
        state.outputs.get_mut(&2).unwrap().normal_transform = false;
        assert_eq!(state.selected(), None);
        state.outputs.get_mut(&2).unwrap().normal_transform = true;
        state.outputs.insert(
            3,
            Output {
                size: Some((2560, 720)),
                scale: 1,
                normal_transform: true,
                ..Output::default()
            },
        );
        assert_eq!(state.selected(), None);
        state.outputs.remove(&2);
        assert_eq!(state.selected(), Some(3));
        state.outputs.remove(&3);
        assert_eq!(state.selected(), None);
    }
    #[test]
    fn parent_monitor_uses_process_start_time() {
        let pid = std::process::id();
        let start = process_identity(pid).unwrap();
        assert!(parent_alive(&Some((pid, start))));
        assert!(!parent_alive(&Some((pid, "wrong".into()))));
        assert!(parent_alive(&None));
    }
}
