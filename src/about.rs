//! The "About Camera Studio" dialog.

use adw::prelude::*;

use crate::window::Window;
use crate::{APP_ID, VERSION};

pub const AUTHOR: &str = "Hồ Sỹ Thế";
pub const AUTHOR_URL: &str = "https://github.com/hsthe29";
pub const REPOSITORY: &str = "https://github.com/hsthe29/camera-studio";

const RELEASE_NOTES: &str = "\
<p>First release.</p>
<ul>
<li>GPU-accelerated from end to end: VA-API MJPEG decoding, H.264/VP9 encoding and a \
zero-copy preview.</li>
<li>Photos with self-timer and burst mode, video recording with sound, QR code scanning.</li>
<li>26 live effects with previews of the current scene, and sharp bicubic digital zoom.</li>
<li>Webcam hardware controls, hot-plug detection and automatic reconnection.</li>
<li>Built-in gallery and video player.</li>
</ul>";

pub fn show(parent: Option<&gtk::Window>, window: Option<&Window>) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Camera Studio")
        .application_icon(APP_ID)
        .version(VERSION)
        .developer_name(AUTHOR)
        .developers([format!("{AUTHOR} {AUTHOR_URL}")])
        .copyright(format!("© 2026 {AUTHOR}"))
        .license_type(gtk::License::MitX11)
        .website(REPOSITORY)
        .issue_url(format!("{REPOSITORY}/issues"))
        .comments(
            "A GPU-accelerated camera app for the Linux desktop: photos, videos, QR code \
             scanning, 26 live effects and webcam hardware controls.",
        )
        .release_notes_version(VERSION)
        .release_notes(RELEASE_NOTES)
        .debug_info(debug_info(window))
        .debug_info_filename("camera-studio-debug.txt")
        .build();
    dialog.add_link("Source Code", REPOSITORY);
    dialog.add_acknowledgement_section(
        Some("Built with"),
        &[
            "Rust https://www.rust-lang.org",
            "GTK https://www.gtk.org",
            "libadwaita https://gitlab.gnome.org/GNOME/libadwaita",
            "GStreamer https://gstreamer.freedesktop.org",
            "gtk-rs, gstreamer-rs https://gtk-rs.org",
            "Intel Media Driver (VA-API) https://github.com/intel/media-driver",
            "ZBar https://github.com/mchehab/zbar",
        ],
    );
    dialog.add_legal_section(
        "GTK, libadwaita",
        Some("© The GTK and GNOME developers"),
        gtk::License::Lgpl21,
        None,
    );
    dialog.add_legal_section(
        "GStreamer",
        Some("© The GStreamer developers"),
        gtk::License::Lgpl21,
        None,
    );
    dialog.add_legal_section(
        "gtk-rs, gstreamer-rs",
        Some("© The gtk-rs and gstreamer-rs developers"),
        gtk::License::MitX11,
        None,
    );
    dialog.present(parent);
}

/// Plain-text system report for the "Troubleshooting" page (copy/save it
/// into an issue report).
pub fn debug_info(window: Option<&Window>) -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("PRETTY_NAME=")
                    .map(|v| v.trim_matches('"').to_string())
            })
        })
        .unwrap_or_else(|| "unknown".into());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "-".into());
    let mut out = vec![
        format!(
            "Camera Studio {VERSION} ({})",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        ),
        String::new(),
        "System".into(),
        format!("- OS: {os}, kernel {kernel}"),
        format!(
            "- Session: {} / {}",
            env("XDG_SESSION_TYPE"),
            env("XDG_CURRENT_DESKTOP")
        ),
        format!("- Locale: {} (LANGUAGE={})", env("LANG"), env("LANGUAGE")),
        format!(
            "- GTK {}.{}.{}, libadwaita {}.{}.{}, {}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version(),
            gst::version_string()
        ),
    ];
    if let Some(w) = window {
        out.push(String::new());
        out.extend(w.debug_report());
    }
    out.join("\n")
}
