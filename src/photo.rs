//! Still photos: JPEG encoding off the UI thread.

use std::path::Path;

use anyhow::{Result, anyhow};
use gst::prelude::*;
use gtk::{gdk, glib};

/// Blocking: write one RGBA frame as JPEG with EXIF date/software/model tags.
pub fn encode_jpeg(
    sample: &gst::Sample,
    path: &Path,
    quality: u32,
    mirror: bool,
    camera: Option<&str>,
) -> Result<()> {
    let flip = if mirror {
        "videoflip method=horizontal-flip ! "
    } else {
        ""
    };
    let desc = format!(
        // RGB in: libjpeg does the JFIF (full-range BT.601) conversion itself, which
        // keeps colours exact; a video-range YUV step here would wash them out.
        "appsrc name=src format=time ! videoconvert ! {flip}video/x-raw,format=RGB \
         ! jpegenc quality={quality} ! jifmux name=mux ! filesink name=sink"
    );
    let pipeline = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .unwrap();
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    src.set_caps(sample.caps().map(|c| c.to_owned()).as_ref());
    pipeline
        .by_name("sink")
        .unwrap()
        .set_property("location", path.to_str().unwrap_or_default());

    let mut tags = gst::TagList::new();
    {
        let tags = tags.get_mut().unwrap();
        if let Ok(now) = glib::DateTime::now_local() {
            tags.add::<gst::tags::DateTime>(&gst::DateTime::from(now), gst::TagMergeMode::Replace);
        }
        tags.add::<gst::tags::ApplicationName>(&"Camera Studio", gst::TagMergeMode::Replace);
        if let Some(name) = camera {
            // EXIF strings are ASCII.
            let ascii: String = name
                .chars()
                .map(ascii_fold)
                .filter(|c| c.is_ascii())
                .collect();
            tags.add::<gst::tags::DeviceModel>(&ascii.as_str(), gst::TagMergeMode::Replace);
        }
    }
    if let Ok(setter) = pipeline
        .by_name("mux")
        .unwrap()
        .dynamic_cast::<gst::TagSetter>()
    {
        setter.merge_tags(&tags, gst::TagMergeMode::Replace);
    }

    let result = (|| -> Result<()> {
        pipeline.set_state(gst::State::Playing)?;
        let mut buffer = sample
            .buffer_owned()
            .ok_or_else(|| anyhow!("empty frame"))?;
        {
            let b = buffer.make_mut();
            b.set_pts(gst::ClockTime::ZERO);
            b.set_dts(gst::ClockTime::NONE);
        }
        src.push_buffer(buffer)?;
        src.end_of_stream()?;
        let msg = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(10),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .ok_or_else(|| anyhow!("encoding the photo timed out"))?;
        if let gst::MessageView::Error(e) = msg.view() {
            return Err(anyhow!("{}", e.error()));
        }
        Ok(())
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result?;
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) == 0 {
        return Err(anyhow!("the photo file is empty"));
    }
    Ok(())
}

fn ascii_fold(c: char) -> char {
    const MAP: &[(&str, char)] = &[
        ("àáạảãâầấậẩẫăằắặẳẵ", 'a'),
        ("èéẹẻẽêềếệểễ", 'e'),
        ("ìíịỉĩ", 'i'),
        ("òóọỏõôồốộổỗơờớợởỡ", 'o'),
        ("ùúụủũưừứựửữ", 'u'),
        ("ỳýỵỷỹ", 'y'),
        ("đ", 'd'),
        ("ÀÁẠẢÃÂẦẤẬẨẪĂẰẮẶẲẴ", 'A'),
        ("ÈÉẸẺẼÊỀẾỆỂỄ", 'E'),
        ("ÌÍỊỈĨ", 'I'),
        ("ÒÓỌỎÕÔỒỐỘỔỖƠỜỚỢỞỠ", 'O'),
        ("ÙÚỤỦŨƯỪỨỰỬỮ", 'U'),
        ("ỲÝỴỶỸ", 'Y'),
        ("Đ", 'D'),
    ];
    MAP.iter()
        .find(|(set, _)| set.contains(c))
        .map(|(_, r)| *r)
        .unwrap_or(c)
}

/// RGBA sample -> texture (used for the instant gallery thumbnail).
pub fn sample_to_texture(sample: &gst::Sample) -> Option<gdk::Texture> {
    let caps = sample.caps()?;
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let buffer = sample.buffer()?;
    let map = buffer.map_readable().ok()?;
    let bytes = glib::Bytes::from(map.as_slice());
    let format = match info.format() {
        gst_video::VideoFormat::Rgba => gdk::MemoryFormat::R8g8b8a8,
        gst_video::VideoFormat::Rgbx => gdk::MemoryFormat::R8g8b8x8,
        gst_video::VideoFormat::Bgrx => gdk::MemoryFormat::B8g8r8x8,
        gst_video::VideoFormat::Bgra => gdk::MemoryFormat::B8g8r8a8,
        _ => return None,
    };
    Some(
        gdk::MemoryTexture::new(
            info.width() as i32,
            info.height() as i32,
            format,
            &bytes,
            info.stride()[0] as usize,
        )
        .upcast(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold() {
        let s: String = "Nguồn thử nghiệm Đà".chars().map(ascii_fold).collect();
        assert_eq!(s, "Nguon thu nghiem Da");
    }

    #[test]
    fn encodes_jpeg() {
        gst::init().unwrap();
        let caps = gst::Caps::from_str("video/x-raw,format=RGBA,width=64,height=48,framerate=30/1")
            .unwrap();
        let buffer = gst::Buffer::from_mut_slice(vec![200u8; 64 * 48 * 4]);
        let sample = gst::Sample::builder().buffer(&buffer).caps(&caps).build();
        let path = std::env::temp_dir().join(format!("cs-photo-{}.jpg", std::process::id()));
        encode_jpeg(&sample, &path, 90, true, Some("Nguồn thử")).unwrap();
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..2], &[0xFF, 0xD8]);
        assert!(data.windows(4).any(|w| w == b"Exif"));
        std::fs::remove_file(path).ok();
    }

    use std::str::FromStr;
}
