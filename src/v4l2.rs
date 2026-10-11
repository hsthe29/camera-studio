//! Minimal V4L2 control access through ioctl.
//!
//! A V4L2 node may be opened several times; only streaming is exclusive, so
//! controls can be changed while GStreamer streams from the same device.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

#[repr(C)]
#[derive(Default)]
struct QueryCtrl {
    id: u32,
    type_: u32,
    name: [u8; 32],
    minimum: i32,
    maximum: i32,
    step: i32,
    default_value: i32,
    flags: u32,
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct Control {
    id: u32,
    value: i32,
}

#[repr(C, packed)]
struct QueryMenu {
    id: u32,
    index: u32,
    name: [u8; 32], // union { name[32]; s64 value; }
    reserved: u32,
}

const fn iowr(nr: u32, size: usize) -> libc::c_ulong {
    ((3u32 << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as libc::c_ulong
}

const VIDIOC_G_CTRL: libc::c_ulong = iowr(27, std::mem::size_of::<Control>());
const VIDIOC_S_CTRL: libc::c_ulong = iowr(28, std::mem::size_of::<Control>());
const VIDIOC_QUERYCTRL: libc::c_ulong = iowr(36, std::mem::size_of::<QueryCtrl>());
const VIDIOC_QUERYMENU: libc::c_ulong = iowr(37, std::mem::size_of::<QueryMenu>());

const FLAG_NEXT_CTRL: u32 = 0x8000_0000;

pub const TYPE_INTEGER: u32 = 1;
pub const TYPE_BOOLEAN: u32 = 2;
pub const TYPE_MENU: u32 = 3;
pub const TYPE_INTEGER_MENU: u32 = 9;

const FLAG_DISABLED: u32 = 0x0001;
const FLAG_GRABBED: u32 = 0x0002;
const FLAG_READ_ONLY: u32 = 0x0004;
const FLAG_INACTIVE: u32 = 0x0010;

#[derive(Clone, Debug)]
pub struct Ctrl {
    pub id: u32,
    pub type_: u32,
    pub name: String,
    pub minimum: i32,
    pub maximum: i32,
    pub step: i32,
    pub default: i32,
    pub flags: u32,
    pub menu: Vec<(i32, String)>,
}

impl Ctrl {
    pub fn inactive(&self) -> bool {
        self.flags & (FLAG_INACTIVE | FLAG_GRABBED) != 0
    }
    pub fn read_only(&self) -> bool {
        self.flags & FLAG_READ_ONLY != 0
    }
    /// Settings key, stable across versions (e.g. "0x980900").
    pub fn key(&self) -> String {
        format!("{:#x}", self.id)
    }
}

pub struct Device {
    file: File,
}

impl Device {
    pub fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self { file })
    }

    fn ioctl<T>(&self, request: libc::c_ulong, arg: &mut T) -> std::io::Result<()> {
        // SAFETY: `arg` is a properly sized #[repr(C)] struct matching `request`.
        let r = unsafe { libc::ioctl(self.file.as_raw_fd(), request as _, arg as *mut T) };
        if r < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn raw_query(&self, id: u32) -> Option<QueryCtrl> {
        let mut q = QueryCtrl {
            id,
            ..Default::default()
        };
        self.ioctl(VIDIOC_QUERYCTRL, &mut q).ok().map(|_| q)
    }

    fn menu(&self, id: u32, type_: u32, lo: i32, hi: i32) -> Vec<(i32, String)> {
        let mut items = Vec::new();
        for index in lo.max(0)..=hi {
            let mut m = QueryMenu {
                id,
                index: index as u32,
                name: [0; 32],
                reserved: 0,
            };
            if self.ioctl(VIDIOC_QUERYMENU, &mut m).is_err() {
                continue; // holes are allowed
            }
            let name = m.name;
            let label = if type_ == TYPE_INTEGER_MENU {
                i64::from_ne_bytes(name[..8].try_into().unwrap()).to_string()
            } else {
                CStr::from_bytes_until_nul(&name)
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            items.push((index, label));
        }
        items
    }

    pub fn query(&self, id: u32) -> Option<Ctrl> {
        let q = self.raw_query(id)?;
        let name = CStr::from_bytes_until_nul(&q.name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let menu = if matches!(q.type_, TYPE_MENU | TYPE_INTEGER_MENU) {
            self.menu(q.id, q.type_, q.minimum, q.maximum)
        } else {
            Vec::new()
        };
        Some(Ctrl {
            id: q.id,
            type_: q.type_,
            name,
            minimum: q.minimum,
            maximum: q.maximum,
            step: q.step,
            default: q.default_value,
            flags: q.flags,
            menu,
        })
    }

    /// All user-adjustable controls (integer, boolean, menu).
    pub fn controls(&self) -> Vec<Ctrl> {
        let mut result = Vec::new();
        let mut next = FLAG_NEXT_CTRL;
        while let Some(q) = self.raw_query(next) {
            next = q.id | FLAG_NEXT_CTRL;
            if q.flags & FLAG_DISABLED != 0
                || !matches!(
                    q.type_,
                    TYPE_INTEGER | TYPE_BOOLEAN | TYPE_MENU | TYPE_INTEGER_MENU
                )
            {
                continue;
            }
            if let Some(c) = self.query(q.id)
                && !c.read_only()
            {
                result.push(c);
            }
        }
        result
    }

    pub fn get(&self, id: u32) -> std::io::Result<i32> {
        let mut c = Control { id, value: 0 };
        self.ioctl(VIDIOC_G_CTRL, &mut c)?;
        Ok(c.value)
    }

    pub fn set(&self, id: u32, value: i32) -> std::io::Result<()> {
        let mut c = Control { id, value };
        self.ioctl(VIDIOC_S_CTRL, &mut c)
    }
}

/// Friendlier names for the V4L2 controls whose kernel name reads awkwardly;
/// everything else is shown as the driver names it.
pub fn control_label(name: &str) -> &str {
    match name {
        "Power Line Frequency" => "Anti-Flicker",
        "White Balance, Automatic" | "White Balance Temperature, Auto" => "Auto White Balance",
        "Auto Exposure" | "Exposure, Auto" => "Exposure Mode",
        "Exposure Time, Absolute" | "Exposure (Absolute)" => "Exposure Time",
        "Exposure, Dynamic Framerate" => "Dynamic Frame Rate",
        "Exposure, Auto Priority" => "Auto Exposure Priority",
        "Focus, Automatic Continuous" | "Focus, Auto" => "Autofocus",
        "Focus (absolute)" | "Focus, Absolute" => "Focus",
        "Zoom, Absolute" => "Optical Zoom",
        "Pan, Absolute" => "Pan",
        "Tilt, Absolute" => "Tilt",
        "Hue, Automatic" => "Auto Hue",
        other => other,
    }
}

pub fn menu_label(text: &str) -> &str {
    match text {
        "Disabled" => "Off",
        "Auto" | "Auto Mode" => "Auto",
        "Manual Mode" => "Manual",
        "Shutter Priority Mode" => "Shutter Priority",
        // On UVC webcams "Aperture Priority" is simply the automatic exposure mode.
        "Aperture Priority Mode" => "Auto",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_abi() {
        assert_eq!(std::mem::size_of::<QueryCtrl>(), 68);
        assert_eq!(std::mem::size_of::<QueryMenu>(), 44);
        assert_eq!(VIDIOC_QUERYCTRL, 0xC044_5624);
        assert_eq!(VIDIOC_S_CTRL, 0xC008_561C);
    }
}
