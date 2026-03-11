use std::fs::OpenOptions;
use std::io::Write;

use anyhow::{Context, Result};
use input_linux::{
    AbsoluteAxis, AbsoluteInfo, AbsoluteInfoSetup, EventKind, InputId, Key, UInputHandle,
};

/// Virtual absolute-position mouse backed by uinput.
pub struct VirtualMouse {
    handle: UInputHandle<std::fs::File>,
    button_down: bool,
}

impl VirtualMouse {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .context("Cannot open /dev/uinput. Ensure the user has permission (try: sudo usermod -aG input $USER)")?;

        let handle = UInputHandle::new(file);
        handle.set_evbit(EventKind::Absolute).context("set EV_ABS")?;
        handle.set_evbit(EventKind::Key).context("set EV_KEY")?;
        handle.set_keybit(Key::ButtonLeft).context("set BTN_LEFT")?;
        handle.set_absbit(AbsoluteAxis::X).context("set ABS_X")?;
        handle.set_absbit(AbsoluteAxis::Y).context("set ABS_Y")?;

        let id = InputId {
            bustype: 0x06,
            vendor: 0x1234,
            product: 0x5680,
            version: 1,
        };

        let abs = [
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::X,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: width as i32 - 1,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::Y,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: height as i32 - 1,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
        ];

        handle
            .create(&id, b"kmsvnc-mouse", 0, &abs)
            .context("create uinput mouse device")?;

        tracing::info!("Created virtual mouse ({}x{})", width, height);
        std::thread::sleep(std::time::Duration::from_millis(100));

        Ok(Self {
            handle,
            button_down: false,
        })
    }

    pub fn handle_pointer(&mut self, button_mask: u8, x: u16, y: u16) -> Result<()> {
        let left_down = (button_mask & 1) != 0;
        let mut events = vec![
            make_event(EV_ABS, ABS_X, x as i32),
            make_event(EV_ABS, ABS_Y, y as i32),
        ];
        if left_down != self.button_down {
            events.push(make_event(EV_KEY, BTN_LEFT, if left_down { 1 } else { 0 }));
            self.button_down = left_down;
        }
        events.push(make_event(EV_SYN, SYN_REPORT, 0));
        self.write_events(&events)
    }

    fn write_events(&self, events: &[input_linux::sys::input_event]) -> Result<()> {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                events.as_ptr() as *const u8,
                std::mem::size_of_val(events),
            )
        };
        self.handle
            .as_inner()
            .write_all(bytes)
            .context("write events to uinput")?;
        Ok(())
    }
}

impl Drop for VirtualMouse {
    fn drop(&mut self) {
        if let Err(e) = self.handle.dev_destroy() {
            tracing::warn!("Failed to destroy mouse device: {e}");
        }
    }
}

const EV_SYN: u16 = input_linux::sys::EV_SYN as u16;
const EV_KEY: u16 = input_linux::sys::EV_KEY as u16;
const EV_ABS: u16 = input_linux::sys::EV_ABS as u16;
const SYN_REPORT: u16 = input_linux::sys::SYN_REPORT as u16;
const BTN_LEFT: u16 = input_linux::sys::BTN_LEFT as u16;
const ABS_X: u16 = input_linux::sys::ABS_X as u16;
const ABS_Y: u16 = input_linux::sys::ABS_Y as u16;

fn make_event(type_: u16, code: u16, value: i32) -> input_linux::sys::input_event {
    let mut ev: input_linux::sys::input_event = unsafe { std::mem::zeroed() };
    ev.type_ = type_;
    ev.code = code;
    ev.value = value;
    ev
}
