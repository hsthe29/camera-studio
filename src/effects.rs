//! Live effects as one GLSL fragment shader.
//!
//! All effects and the digital zoom live in a single shader and are selected
//! through uniforms, so switching effect or zoom never recompiles anything and
//! never renegotiates the pipeline — it is a few bytes sent to the GPU.

pub struct Effect {
    pub id: &'static str,
    pub label: &'static str,
    /// Value of the `effect` uniform.
    pub code: u32,
}

pub const EFFECTS: &[Effect] = &[
    Effect {
        id: "none",
        label: "Original",
        code: 0,
    },
    Effect {
        id: "mono",
        label: "Mono",
        code: 1,
    },
    Effect {
        id: "noir",
        label: "Noir",
        code: 2,
    },
    Effect {
        id: "vivid",
        label: "Vivid",
        code: 3,
    },
    Effect {
        id: "faded",
        label: "Faded",
        code: 4,
    },
    Effect {
        id: "warm",
        label: "Warm",
        code: 5,
    },
    Effect {
        id: "cool",
        label: "Cool",
        code: 6,
    },
    Effect {
        id: "sepia",
        label: "Sepia",
        code: 7,
    },
    Effect {
        id: "vintage",
        label: "Vintage",
        code: 8,
    },
    Effect {
        id: "xpro",
        label: "Cross-process",
        code: 9,
    },
    Effect {
        id: "heat",
        label: "Thermal",
        code: 10,
    },
    Effect {
        id: "xray",
        label: "X-Ray",
        code: 11,
    },
    Effect {
        id: "negative",
        label: "Negative",
        code: 12,
    },
    Effect {
        id: "sketch",
        label: "Sketch",
        code: 13,
    },
    Effect {
        id: "comic",
        label: "Comic",
        code: 14,
    },
    Effect {
        id: "solarize",
        label: "Solarize",
        code: 15,
    },
    Effect {
        id: "pixel",
        label: "Pixel",
        code: 16,
    },
    Effect {
        id: "kaleido",
        label: "Kaleidoscope",
        code: 17,
    },
    Effect {
        id: "mirror",
        label: "Mirror",
        code: 18,
    },
    Effect {
        id: "fisheye",
        label: "Fisheye",
        code: 19,
    },
    Effect {
        id: "bulge",
        label: "Bulge",
        code: 20,
    },
    Effect {
        id: "pinch",
        label: "Pinch",
        code: 21,
    },
    Effect {
        id: "twirl",
        label: "Twirl",
        code: 22,
    },
    Effect {
        id: "ripple",
        label: "Ripple",
        code: 23,
    },
    Effect {
        id: "vignette",
        label: "Vignette",
        code: 24,
    },
    Effect {
        id: "night",
        label: "Night Vision",
        code: 25,
    },
];

pub fn get(id: &str) -> &'static Effect {
    EFFECTS.iter().find(|e| e.id == id).unwrap_or(&EFFECTS[0])
}

pub const SHADER: &str = r#"#version 100
#ifdef GL_ES
precision highp float;
#endif
varying vec2 v_texcoord;
uniform sampler2D tex;
uniform float zoom;
uniform float effect;
uniform float width;
uniform float height;
uniform float time;

float luma(vec3 c) { return dot(c, vec3(0.299, 0.587, 0.114)); }
float hash(vec2 p) { return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453); }
vec3 saturate3(vec3 c, float s) { return mix(vec3(luma(c)), c, s); }
vec4 px(vec2 uv) { return texture2D(tex, clamp(uv, 0.0, 1.0)); }

// Catmull-Rom (bicubic) sampling in 9 bilinear taps: digital zoom stays much
// sharper than with plain bilinear filtering.
vec4 bicubic(vec2 uv) {
    vec2 size = vec2(width, height);
    vec2 pos = uv * size;
    vec2 p1 = floor(pos - 0.5) + 0.5;
    vec2 f = pos - p1;
    vec2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    vec2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    vec2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    vec2 w3 = f * f * (-0.5 + 0.5 * f);
    vec2 w12 = w1 + w2;
    vec2 t0 = (p1 - 1.0) / size;
    vec2 t3 = (p1 + 2.0) / size;
    vec2 t12 = (p1 + w2 / w12) / size;
    vec4 r = px(vec2(t0.x, t0.y)) * w0.x * w0.y
           + px(vec2(t12.x, t0.y)) * w12.x * w0.y
           + px(vec2(t3.x, t0.y)) * w3.x * w0.y
           + px(vec2(t0.x, t12.y)) * w0.x * w12.y
           + px(vec2(t12.x, t12.y)) * w12.x * w12.y
           + px(vec2(t3.x, t12.y)) * w3.x * w12.y
           + px(vec2(t0.x, t3.y)) * w0.x * w3.y
           + px(vec2(t12.x, t3.y)) * w12.x * w3.y
           + px(vec2(t3.x, t3.y)) * w3.x * w3.y;
    return clamp(r, 0.0, 1.0);
}

vec2 radial(vec2 uv, float k, float radius, float aspect) {
    vec2 p = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float r = length(p);
    if (r >= radius || r < 1e-5) return uv;
    float s = radius * pow(r / radius, k);
    p *= s / r;
    return vec2(p.x / aspect + 0.5, p.y + 0.5);
}

vec2 warp(vec2 uv, int e) {
    float aspect = width / height;
    vec2 p = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float r = length(p);
    if (e == 16) {                                  // pixel
        vec2 cells = vec2(width, height) / 14.0;
        return (floor(uv * cells) + 0.5) / cells;
    } else if (e == 17) {                           // kaleidoscope
        float seg = 6.2831853 / 6.0;
        float a = mod(atan(p.y, p.x), seg);
        a = abs(a - seg * 0.5);
        p = r * vec2(cos(a), sin(a));
        return vec2(p.x / aspect + 0.5, p.y + 0.5);
    } else if (e == 18) {                           // mirror
        return vec2(uv.x > 0.5 ? 1.0 - uv.x : uv.x, uv.y);
    } else if (e == 19) {                           // fisheye
        float s = min(0.55 + 1.8 * r * r, 1.0);
        p *= s;
        return vec2(p.x / aspect + 0.5, p.y + 0.5);
    } else if (e == 20) {                           // bulge
        return radial(uv, 1.8, 0.45, aspect);
    } else if (e == 21) {                           // pinch
        return radial(uv, 0.6, 0.45, aspect);
    } else if (e == 22) {                           // twirl
        float radius = 0.5;
        if (r < radius) {
            float t = 1.0 - r / radius;
            float a = t * t * 4.0;
            float cs = cos(a), sn = sin(a);
            p = vec2(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
        }
        return vec2(p.x / aspect + 0.5, p.y + 0.5);
    } else if (e == 23) {                           // ripple
        return uv + 0.004 * vec2(sin(uv.y * 45.0 + time * 4.0), cos(uv.x * 45.0 + time * 3.0));
    }
    return uv;
}

float edges(vec2 uv) {
    vec2 d = vec2(1.0 / width, 1.0 / height);
    float tl = luma(px(uv + d * vec2(-1.0, -1.0)).rgb);
    float t  = luma(px(uv + d * vec2( 0.0, -1.0)).rgb);
    float tr = luma(px(uv + d * vec2( 1.0, -1.0)).rgb);
    float l  = luma(px(uv + d * vec2(-1.0,  0.0)).rgb);
    float r  = luma(px(uv + d * vec2( 1.0,  0.0)).rgb);
    float bl = luma(px(uv + d * vec2(-1.0,  1.0)).rgb);
    float b  = luma(px(uv + d * vec2( 0.0,  1.0)).rgb);
    float br = luma(px(uv + d * vec2( 1.0,  1.0)).rgb);
    float gx = -tl - 2.0 * l - bl + tr + 2.0 * r + br;
    float gy = -tl - 2.0 * t - tr + bl + 2.0 * b + br;
    return length(vec2(gx, gy));
}

vec3 heat(float t) {
    if (t < 0.25) return mix(vec3(0.0, 0.0, 0.25), vec3(0.45, 0.0, 0.6), t / 0.25);
    if (t < 0.5) return mix(vec3(0.45, 0.0, 0.6), vec3(0.95, 0.1, 0.15), (t - 0.25) / 0.25);
    if (t < 0.75) return mix(vec3(0.95, 0.1, 0.15), vec3(1.0, 0.65, 0.0), (t - 0.5) / 0.25);
    return mix(vec3(1.0, 0.65, 0.0), vec3(1.0, 1.0, 0.85), (t - 0.75) / 0.25);
}

vec3 sepia(vec3 c) {
    return clamp(vec3(dot(c, vec3(0.393, 0.769, 0.189)),
                      dot(c, vec3(0.349, 0.686, 0.168)),
                      dot(c, vec3(0.272, 0.534, 0.131))), 0.0, 1.0);
}

void main() {
    int e = int(effect + 0.5);
    vec2 uv = (v_texcoord - 0.5) / zoom + 0.5;
    uv = warp(uv, e);
    vec3 c = zoom > 1.001 ? bicubic(uv).rgb : px(uv).rgb;
    vec2 q = vec2((v_texcoord.x - 0.5) * width / height, v_texcoord.y - 0.5);
    float vig = smoothstep(1.0, 0.35, length(q));

    if (e == 1) c = vec3(luma(c));
    else if (e == 2) c = vec3(smoothstep(0.1, 0.85, luma(c)));
    else if (e == 3) c = clamp((saturate3(c, 1.6) - 0.5) * 1.08 + 0.5, 0.0, 1.0);
    else if (e == 4) c = mix(c, vec3(luma(c)), 0.45) * 0.8 + 0.12;
    else if (e == 5) c = clamp(c * vec3(1.1, 1.0, 0.82) + vec3(0.03, 0.01, 0.0), 0.0, 1.0);
    else if (e == 6) c = clamp(c * vec3(0.86, 1.0, 1.12) + vec3(0.0, 0.01, 0.03), 0.0, 1.0);
    else if (e == 7) c = sepia(c);
    else if (e == 8) {
        c = mix(c, sepia(c), 0.75) * 0.9 + 0.06;
        c += (hash(floor(v_texcoord * vec2(width, height) * 0.5) + fract(time)) - 0.5) * 0.09;
        float line = step(0.996, hash(vec2(floor(v_texcoord.x * 300.0), floor(time * 12.0))));
        c = mix(c, vec3(0.85, 0.8, 0.7), line * 0.5);
        c *= mix(0.55, 1.0, vig);
    }
    else if (e == 9) c = clamp(vec3(smoothstep(0.05, 0.95, c.r), c.g * 1.05, c.b * 0.75 + 0.12), 0.0, 1.0);
    else if (e == 10) c = heat(luma(c));
    else if (e == 11) { float l = 1.0 - luma(c); c = clamp(vec3(l * 0.75, l * 0.9, l * 1.15), 0.0, 1.0); }
    else if (e == 12) c = 1.0 - c;
    else if (e == 13) { float g = edges(uv); c = vec3(1.0 - smoothstep(0.08, 0.5, g)) * vec3(0.98, 0.97, 0.94); }
    else if (e == 14) {
        vec3 poster = floor(saturate3(c, 1.4) * 5.0 + 0.5) / 5.0;
        c = mix(poster, vec3(0.05), smoothstep(0.25, 0.6, edges(uv)));
    }
    else if (e == 15) c = mix(c, 1.0 - c, step(0.55, c));
    else if (e == 24) c *= mix(0.25, 1.0, vig);
    else if (e == 25) {
        float l = clamp(luma(c) * 1.8, 0.0, 1.0);
        l += (hash(v_texcoord * vec2(width, height) + fract(time) * 100.0) - 0.5) * 0.18;
        l *= 0.85 + 0.15 * sin(v_texcoord.y * height * 1.5);
        c = vec3(0.1, 1.0, 0.25) * l * mix(0.4, 1.0, vig);
    }
    gl_FragColor = vec4(clamp(c, 0.0, 1.0), 1.0);
}
"#;

/// One rendered effect preview (tightly packed RGBA).
pub struct Preview {
    pub id: &'static str,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub pixels: Vec<u8>,
}

/// Blocking: run every effect of the real shader over one small RGBA frame.
///
/// Uses its own tiny GL pipeline; frames are pushed one at a time and each
/// result is pulled before the uniforms change, so every preview is rendered
/// with the right effect.
pub fn render_previews(sample: &gst::Sample) -> anyhow::Result<Vec<Preview>> {
    use anyhow::anyhow;
    use gst::prelude::*;

    let caps = sample.caps().ok_or_else(|| anyhow!("no caps"))?.to_owned();
    let pipeline = gst::parse::launch(
        "appsrc name=src format=time ! glupload ! glcolorconvert \
         ! video/x-raw(memory:GLMemory),format=RGBA ! glshader name=fx ! gldownload \
         ! video/x-raw,format=RGBA ! appsink name=sink sync=false",
    )?
    .downcast::<gst::Pipeline>()
    .map_err(|_| anyhow!("not a pipeline"))?;
    let src = pipeline
        .by_name("src")
        .unwrap()
        .downcast::<gst_app::AppSrc>()
        .unwrap();
    src.set_caps(Some(&caps));
    let sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    let fx = pipeline.by_name("fx").unwrap();
    fx.set_property("fragment", SHADER);
    let buffer = sample.buffer_owned().ok_or_else(|| anyhow!("no buffer"))?;

    let result = (|| -> anyhow::Result<Vec<Preview>> {
        pipeline.set_state(gst::State::Playing)?;
        let mut out = Vec::with_capacity(EFFECTS.len());
        for (i, effect) in EFFECTS.iter().enumerate() {
            fx.set_property(
                "uniforms",
                gst::Structure::builder("uniforms")
                    .field("zoom", 1.0f32)
                    .field("effect", effect.code as f32)
                    .build(),
            );
            let mut buf = buffer.copy();
            {
                let b = buf.make_mut();
                b.set_pts(gst::ClockTime::from_mseconds(i as u64 * 40));
                b.set_dts(gst::ClockTime::NONE);
            }
            src.push_buffer(buf)?;
            let rendered = sink
                .try_pull_sample(gst::ClockTime::from_seconds(3))
                .ok_or_else(|| anyhow!("render timeout"))?;
            let info = gst_video::VideoInfo::from_caps(rendered.caps().unwrap())?;
            let map = rendered.buffer().unwrap().map_readable()?;
            out.push(Preview {
                id: effect.id,
                width: info.width() as i32,
                height: info.height() as i32,
                stride: info.stride()[0] as usize,
                pixels: map.as_slice().to_vec(),
            });
        }
        Ok(out)
    })();
    let _ = src.end_of_stream();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_codes_unique_and_handled() {
        let mut codes: Vec<u32> = EFFECTS.iter().map(|e| e.code).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), EFFECTS.len());
        for e in EFFECTS.iter().filter(|e| e.code != 0) {
            assert!(
                SHADER.contains(&format!("e == {}", e.code)),
                "{} not in shader",
                e.id
            );
        }
        assert_eq!(get("missing").id, "none");
    }

    #[test]
    fn previews_render_every_effect() {
        use std::str::FromStr;
        gst::init().unwrap();
        let (w, h) = (64usize, 36usize);
        // Left half red, right half blue: warps and colour effects all change it.
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                px[o..o + 4].copy_from_slice(if x < w / 2 {
                    &[220, 40, 30, 255]
                } else {
                    &[30, 60, 210, 255]
                });
            }
        }
        let caps = gst::Caps::from_str(&format!(
            "video/x-raw,format=RGBA,width={w},height={h},framerate=30/1"
        ))
        .unwrap();
        let sample = gst::Sample::builder()
            .buffer(&gst::Buffer::from_mut_slice(px))
            .caps(&caps)
            .build();
        let previews = render_previews(&sample).expect("render");
        assert_eq!(previews.len(), EFFECTS.len());
        let original = &previews[0].pixels;
        let unchanged: Vec<_> = previews
            .iter()
            .skip(1)
            .filter(|p| &p.pixels == original)
            .map(|p| p.id)
            .collect();
        // Purely geometric effects may leave this symmetric two-colour frame unchanged.
        assert!(
            unchanged.iter().all(|id| [
                "mirror", "kaleido", "pixel", "fisheye", "bulge", "pinch", "twirl", "ripple"
            ]
            .contains(id)),
            "effects that did nothing: {unchanged:?}"
        );
    }
}
