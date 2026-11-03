//! Drawing with Direct2D + DirectWrite.
//!
//! DirectWrite is the same text engine Windows Terminal uses: it handles font
//! fallback (emoji, symbols, Hindi) and joins letters correctly for us.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Result, HSTRING};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows_numerics::Vector2;

use crate::layout::Rect;
use crate::win::wstr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Rgb {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, o.0), m(self.1, o.1), m(self.2, o.2))
    }
    fn f(self, a: f32) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0 as f32 / 255.0, g: self.1 as f32 / 255.0, b: self.2 as f32 / 255.0, a }
    }
    /// 0x00BBGGRR for DWM.
    pub fn bgr(self) -> u32 {
        (self.2 as u32) << 16 | (self.1 as u32) << 8 | self.0 as u32
    }
}

pub fn d2r(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h }
}

pub struct Gfx {
    pub dw: IDWriteFactory,
    d2d: ID2D1Factory,
    rt: Option<ID2D1HwndRenderTarget>,
    brush: Option<ID2D1SolidColorBrush>,
    coll: IDWriteFontCollection,
    /// 0 regular, 1 bold, 2 italic, 3 bold italic.
    pub fmts: Vec<IDWriteTextFormat>,
    pub ui: IDWriteTextFormat,
    pub ui_bold: IDWriteTextFormat,
    pub ui_small: IDWriteTextFormat,
    pub ui_center: IDWriteTextFormat,
    /// The typing box: terminal font, wraps long lines.
    pub boxf: IDWriteTextFormat,
    face: IDWriteFontFace,
    pub cell_w: f32,
    pub cell_h: f32,
    pub baseline: f32,
    pub underline_y: f32,
    pub line_th: f32,
    pub scale: f32,
    pub font_pt: f32,
    pub family: String,
    coverage: RefCell<HashMap<char, bool>>,
    /// Shaped text pieces, shared by all panes (two generations, ~4000 pieces max).
    seg_cache: RefCell<(HashMap<(Vec<u16>, u8), IDWriteTextLayout>, HashMap<(Vec<u16>, u8), IDWriteTextLayout>)>,
    /// Symbols and emoji drawn once into small pictures (colour glyphs are slow to draw every frame).
    bmp_cache: RefCell<(HashMap<BmpKey, Pic>, HashMap<BmpKey, Pic>)>,
    /// Picture previews as GPU bitmaps: key -> (bitmap, original width, height). None = could not load.
    pics: RefCell<HashMap<String, Option<(ID2D1Bitmap, u32, u32)>>>,
    wic: RefCell<Option<windows::Win32::Graphics::Imaging::IWICImagingFactory>>,
}

/// Where a preview picture comes from.
pub enum PicSource {
    Bytes(Vec<u8>),
    File(String),
}

type BmpKey = (Vec<u16>, u8, u32);
type Pic = (ID2D1Bitmap, f32, f32);

impl Gfx {
    pub fn new(family: &str, font_pt: f32, scale: f32) -> Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut coll: Option<IDWriteFontCollection> = None;
            dw.GetSystemFontCollection(&mut coll, false)?;
            let coll = coll.unwrap();
            let dummy = dw.CreateTextFormat(
                w!("Segoe UI"),
                &coll,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                12.0,
                w!("en-us"),
            )?;
            let face = Self::find_face(&coll, family)?;
            let mut g = Gfx {
                dw,
                d2d,
                rt: None,
                brush: None,
                coll,
                fmts: vec![dummy.clone(), dummy.clone(), dummy.clone(), dummy.clone()],
                ui: dummy.clone(),
                ui_bold: dummy.clone(),
                ui_small: dummy.clone(),
                ui_center: dummy.clone(),
                boxf: dummy,
                face,
                cell_w: 8.0,
                cell_h: 16.0,
                baseline: 12.0,
                underline_y: 14.0,
                line_th: 1.0,
                scale,
                font_pt,
                family: family.to_string(),
                coverage: RefCell::new(HashMap::new()),
                seg_cache: RefCell::new(Default::default()),
                bmp_cache: RefCell::new(Default::default()),
                pics: RefCell::new(HashMap::new()),
                wic: RefCell::new(None),
            };
            g.rebuild_fonts()?;
            Ok(g)
        }
    }

    fn find_face(coll: &IDWriteFontCollection, family: &str) -> Result<IDWriteFontFace> {
        unsafe {
            for name in [family, "Cascadia Mono", "Consolas"] {
                let mut idx = 0u32;
                let mut exists = windows::core::BOOL(0);
                coll.FindFamilyName(&HSTRING::from(name), &mut idx, &mut exists)?;
                if exists.as_bool() {
                    let fam = coll.GetFontFamily(idx)?;
                    let font = fam.GetFirstMatchingFont(
                        DWRITE_FONT_WEIGHT_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        DWRITE_FONT_STYLE_NORMAL,
                    )?;
                    return font.CreateFontFace();
                }
            }
            Err(windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32)))
        }
    }

    pub fn set_font(&mut self, family: &str, pt: f32, scale: f32) -> Result<()> {
        if family != self.family {
            self.face = Self::find_face(&self.coll, family)?;
            self.family = family.to_string();
            self.coverage.borrow_mut().clear();
        }
        self.font_pt = pt;
        self.scale = scale;
        self.rebuild_fonts()
    }

    fn rebuild_fonts(&mut self) -> Result<()> {
        *self.seg_cache.borrow_mut() = Default::default();
        *self.bmp_cache.borrow_mut() = Default::default();
        unsafe {
            let px = self.font_pt * 96.0 / 72.0 * self.scale;
            let mut m = DWRITE_FONT_METRICS::default();
            self.face.GetMetrics(&mut m);
            let upem = m.designUnitsPerEm as f32;
            let cp = ['0' as u32];
            let mut gi = [0u16];
            self.face.GetGlyphIndices(cp.as_ptr(), 1, gi.as_mut_ptr())?;
            let mut gm = [DWRITE_GLYPH_METRICS::default()];
            self.face.GetDesignGlyphMetrics(gi.as_ptr(), 1, gm.as_mut_ptr(), false)?;
            let asc = m.ascent as f32 * px / upem;
            let desc = m.descent as f32 * px / upem;
            let gap = (m.lineGap as f32 * px / upem).max(0.0);
            self.cell_w = gm[0].advanceWidth as f32 * px / upem;
            self.cell_h = (asc + desc + gap).ceil();
            self.baseline = (asc + (self.cell_h - asc - desc) / 2.0).round();
            self.underline_y = self.baseline - m.underlinePosition as f32 * px / upem;
            self.line_th = (m.underlineThickness as f32 * px / upem).round().max(1.0);

            let fam = HSTRING::from(self.family.as_str());
            let mut fmts = Vec::new();
            for (weight, style) in [
                (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL),
                (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_NORMAL),
                (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_ITALIC),
                (DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_STYLE_ITALIC),
            ] {
                let f = self.dw.CreateTextFormat(&fam, &self.coll, weight, style, DWRITE_FONT_STRETCH_NORMAL, px, w!("en-us"))?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, self.cell_h, self.baseline)?;
                fmts.push(f);
            }
            self.fmts = fmts;
            let bf = self.dw.CreateTextFormat(&fam, &self.coll, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, px, w!("en-us"))?;
            bf.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            bf.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, self.cell_h, self.baseline)?;
            self.boxf = bf;

            let ui_px = 12.0 * self.scale;
            self.ui = self.ui_format(ui_px, DWRITE_FONT_WEIGHT_NORMAL, false)?;
            self.ui_bold = self.ui_format(ui_px, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?;
            self.ui_small = self.ui_format(10.5 * self.scale, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?;
            self.ui_center = self.ui_format(ui_px, DWRITE_FONT_WEIGHT_NORMAL, true)?;
            Ok(())
        }
    }

    fn ui_format(&self, px: f32, weight: DWRITE_FONT_WEIGHT, center: bool) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dw.CreateTextFormat(
                w!("Segoe UI"),
                &self.coll,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                px,
                w!("en-us"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            if center {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            }
            let sign = self.dw.CreateEllipsisTrimmingSign(&f)?;
            let trim = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            f.SetTrimming(&trim, &sign)?;
            Ok(f)
        }
    }

    /// Does the main font have this character? If not, DirectWrite falls back
    /// to another font with a different width, so we place it cell by cell.
    pub fn has_glyph(&self, c: char) -> bool {
        if c.is_ascii() {
            return true;
        }
        if let Some(v) = self.coverage.borrow().get(&c) {
            return *v;
        }
        let cp = [c as u32];
        let mut gi = [0u16];
        let ok = unsafe { self.face.GetGlyphIndices(cp.as_ptr(), 1, gi.as_mut_ptr()).is_ok() } && gi[0] != 0;
        self.coverage.borrow_mut().insert(c, ok);
        ok
    }

    // ---------- render target ----------

    pub fn ensure_target(&mut self, hwnd: HWND) -> Result<()> {
        if self.rt.is_some() {
            return Ok(());
        }
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hprops)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            let brush = rt.CreateSolidColorBrush(&Rgb(255, 255, 255).f(1.0), None)?;
            self.rt = Some(rt);
            self.brush = Some(brush);
        }
        Ok(())
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if let Some(rt) = &self.rt {
            unsafe {
                let _ = rt.Resize(&D2D_SIZE_U { width: w.max(1), height: h.max(1) });
            }
        }
    }

    pub fn drop_target(&mut self) {
        *self.bmp_cache.borrow_mut() = Default::default();
        self.pics.borrow_mut().clear();
        self.rt = None;
        self.brush = None;
    }

    pub fn begin(&self, clear: Rgb) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.BeginDraw();
                rt.Clear(Some(&clear.f(1.0)));
            }
        }
    }

    /// Returns false when the GPU target was lost and must be made again.
    pub fn end(&self) -> bool {
        match &self.rt {
            Some(rt) => match unsafe { rt.EndDraw(None, None) } {
                Ok(()) => true,
                Err(e) => {
                    crate::log(&format!("EndDraw failed: {e:?}"));
                    false
                }
            },
            None => true,
        }
    }

    fn brush(&self, c: Rgb, a: f32) -> &ID2D1SolidColorBrush {
        let b = self.brush.as_ref().unwrap();
        unsafe { b.SetColor(&c.f(a)) };
        b
    }

    pub fn fill(&self, r: Rect, c: Rgb) {
        self.fill_a(r, c, 1.0)
    }

    pub fn fill_a(&self, r: Rect, c: Rgb, a: f32) {
        if let Some(rt) = &self.rt {
            unsafe { rt.FillRectangle(&d2r(r), self.brush(c, a)) };
        }
    }

    pub fn round(&self, r: Rect, radius: f32, c: Rgb, a: f32) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let rr = D2D1_ROUNDED_RECT { rect: d2r(r), radiusX: radius, radiusY: radius };
                rt.FillRoundedRectangle(&rr, self.brush(c, a));
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            }
        }
    }

    pub fn outline(&self, r: Rect, radius: f32, c: Rgb, width: f32) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let rr = D2D1_ROUNDED_RECT { rect: d2r(r.inset(width / 2.0)), radiusX: radius, radiusY: radius };
                rt.DrawRoundedRectangle(&rr, self.brush(c, 1.0), width, None);
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            }
        }
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: Rgb) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
                rt.FillEllipse(&e, self.brush(c, 1.0));
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            }
        }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgb) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, self.brush(c, 1.0), width, None);
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
            }
        }
    }

    /// A rounded box corner: a line from `a` to `b`, a quarter circle to `c`, a line to `d`.
    pub fn corner(&self, pts: [(f32, f32); 4], radius: f32, clockwise: bool, width: f32, col: Rgb) {
        let (Some(rt), Ok(geo)) = (&self.rt, unsafe { self.d2d.CreatePathGeometry() }) else { return };
        unsafe {
            let Ok(sink) = geo.Open() else { return };
            let v = |p: (f32, f32)| Vector2 { X: p.0, Y: p.1 };
            sink.BeginFigure(v(pts[0]), windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddLine(v(pts[1]));
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: v(pts[2]),
                size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.AddLine(v(pts[3]));
            sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_OPEN);
            if sink.Close().is_err() {
                return;
            }
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            rt.DrawGeometry(&geo, self.brush(col, 1.0), width, None);
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        }
    }

    /// Text for the typing box, wrapped to `width` (not cached: it changes on every key).
    pub fn wrapped(&self, s: &[u16], width: f32) -> Option<IDWriteTextLayout> {
        unsafe { self.dw.CreateTextLayout(s, &self.boxf, width.max(10.0), 100_000.0).ok() }
    }

    /// A small microphone: a rounded body, a U-shaped holder, a stem and a foot.
    pub fn mic(&self, cx: f32, cy: f32, h: f32, c: Rgb) {
        let bw = h * 0.36;
        let bh = h * 0.56;
        let top = cy - h / 2.0;
        self.round(Rect::new(cx - bw / 2.0, top, bw, bh), bw / 2.0, c, 1.0);
        let th = (h * 0.09).max(1.2);
        let r = h * 0.32;
        let y = top + bh * 0.55;
        let (Some(rt), Ok(geo)) = (&self.rt, unsafe { self.d2d.CreatePathGeometry() }) else { return };
        unsafe {
            let Ok(sink) = geo.Open() else { return };
            sink.BeginFigure(Vector2 { X: cx - r, Y: y }, windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: Vector2 { X: cx + r, Y: y },
                size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(windows::Win32::Graphics::Direct2D::Common::D2D1_FIGURE_END_OPEN);
            if sink.Close().is_err() {
                return;
            }
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            rt.DrawGeometry(&geo, self.brush(c, 1.0), th, None);
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        }
        let bottom = cy + h / 2.0;
        self.line(cx, y + r, cx, bottom - th / 2.0, th, c);
        self.line(cx - bw * 0.7, bottom - th / 2.0, cx + bw * 0.7, bottom - th / 2.0, th, c);
    }

    fn wic(&self) -> Option<windows::Win32::Graphics::Imaging::IWICImagingFactory> {
        use windows::Win32::Graphics::Imaging::{IWICImagingFactory, CLSID_WICImagingFactory};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        if let Some(f) = self.wic.borrow().as_ref() {
            return Some(f.clone());
        }
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            *self.wic.borrow_mut() = Some(f.clone());
            Some(f)
        }
    }

    /// A picture for a preview, made once and kept. `load` is only called the first time.
    /// The picture is shrunk to at most `max` pixels on its long side (saves GPU memory).
    pub fn picture(&self, key: &str, max: u32, load: impl FnOnce() -> Option<PicSource>) -> Option<(ID2D1Bitmap, u32, u32)> {
        if let Some(v) = self.pics.borrow().get(key) {
            return v.clone();
        }
        let made = load().and_then(|src| self.decode(src, max));
        // Only keep successes: a chat picture that is not in the log yet may appear a moment later.
        if made.is_some() {
            let mut m = self.pics.borrow_mut();
            if m.len() > 64 {
                m.clear();
            }
            m.insert(key.to_string(), made.clone());
        }
        made
    }

    fn decode(&self, src: PicSource, max: u32) -> Option<(ID2D1Bitmap, u32, u32)> {
        use windows::core::Interface;
        use windows::Win32::Graphics::Imaging::*;
        let rt = self.rt.as_ref()?;
        let wic = self.wic()?;
        unsafe {
            // The bytes must live until the bitmap is made: the decoder reads them lazily.
            let bytes;
            let dec = match src {
                PicSource::Bytes(b) => {
                    bytes = b;
                    let s = wic.CreateStream().ok()?;
                    s.InitializeFromMemory(&bytes).ok()?;
                    wic.CreateDecoderFromStream(&s, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?
                }
                PicSource::File(p) => wic
                    .CreateDecoderFromFilename(&HSTRING::from(p.as_str()), None, windows::Win32::Foundation::GENERIC_READ, WICDecodeMetadataCacheOnDemand)
                    .ok()?,
            };
            let frame = dec.GetFrame(0).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h).ok()?;
            if w == 0 || h == 0 {
                return None;
            }
            let scale = (max as f32 / w.max(h) as f32).min(1.0);
            let source: IWICBitmapSource = if scale < 1.0 {
                let sw = ((w as f32 * scale).round() as u32).max(1);
                let sh = ((h as f32 * scale).round() as u32).max(1);
                let sc = wic.CreateBitmapScaler().ok()?;
                sc.Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant).ok()?;
                sc.cast().ok()?
            } else {
                frame.cast().ok()?
            };
            let conv = wic.CreateFormatConverter().ok()?;
            conv.Initialize(&source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None::<&IWICPalette>, 0.0, WICBitmapPaletteTypeMedianCut)
                .ok()?;
            let bmp = rt.CreateBitmapFromWicBitmap(&conv, None).ok()?;
            Some((bmp, w, h))
        }
    }

    /// Draw a picture stretched into `r` (smooth scaling).
    pub fn draw_picture(&self, b: &ID2D1Bitmap, r: Rect) {
        if let Some(rt) = &self.rt {
            unsafe { rt.DrawBitmap(b, Some(&d2r(r)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
        }
    }

    pub fn clip(&self, r: Rect) {
        if let Some(rt) = &self.rt {
            unsafe { rt.PushAxisAlignedClip(&d2r(r), D2D1_ANTIALIAS_MODE_ALIASED) };
        }
    }

    pub fn unclip(&self) {
        if let Some(rt) = &self.rt {
            unsafe { rt.PopAxisAlignedClip() };
        }
    }

    /// Draw UI text inside a rect (vertically centred, cut with "…" if too long).
    pub fn text(&self, s: &str, fmt: &IDWriteTextFormat, r: Rect, c: Rgb) {
        if let Some(rt) = &self.rt {
            let ws = wstr(s);
            unsafe {
                rt.DrawText(
                    &ws,
                    fmt,
                    &d2r(r),
                    self.brush(c, 1.0),
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                )
            };
        }
    }

    pub fn measure(&self, s: &str, fmt: &IDWriteTextFormat) -> f32 {
        unsafe {
            let ws = wstr(s);
            match self.dw.CreateTextLayout(&ws, fmt, 10_000.0, 100.0) {
                Ok(l) => {
                    let mut m = DWRITE_TEXT_METRICS::default();
                    let _ = l.GetMetrics(&mut m);
                    m.widthIncludingTrailingWhitespace
                }
                Err(_) => 0.0,
            }
        }
    }

    pub fn make_layout(&self, s: &[u16], fmt: usize, underline: bool, strike: bool) -> Option<IDWriteTextLayout> {
        let key = (s.to_vec(), fmt as u8 | (underline as u8) << 2 | (strike as u8) << 3);
        {
            let mut c = self.seg_cache.borrow_mut();
            if let Some(l) = c.0.get(&key) {
                return Some(l.clone());
            }
            if let Some(l) = c.1.remove(&key) {
                c.0.insert(key, l.clone());
                return Some(l);
            }
        }
        let l = self.shape(s, fmt, underline, strike)?;
        let mut c = self.seg_cache.borrow_mut();
        if c.0.len() > 4000 {
            c.1 = std::mem::take(&mut c.0);
        }
        c.0.insert(key, l.clone());
        Some(l)
    }

    fn shape(&self, s: &[u16], fmt: usize, underline: bool, strike: bool) -> Option<IDWriteTextLayout> {
        unsafe {
            let l = self.dw.CreateTextLayout(s, &self.fmts[fmt], 100_000.0, self.cell_h).ok()?;
            let range = DWRITE_TEXT_RANGE { startPosition: 0, length: s.len() as u32 };
            if underline {
                let _ = l.SetUnderline(true, range);
            }
            if strike {
                let _ = l.SetStrikethrough(true, range);
            }
            Some(l)
        }
    }

    /// Draw a symbol/emoji piece from its cached picture (made on first use).
    pub fn draw_special(&self, s: &[u16], fmt: usize, ul: bool, strike: bool, x: f32, y: f32, c: Rgb) {
        let Some(rt) = &self.rt else { return };
        let key: BmpKey = (
            s.to_vec(),
            fmt as u8 | (ul as u8) << 2 | (strike as u8) << 3,
            (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32,
        );
        let hit = {
            let mut m = self.bmp_cache.borrow_mut();
            if let Some(v) = m.0.get(&key) {
                Some(v.clone())
            } else if let Some(v) = m.1.remove(&key) {
                m.0.insert(key.clone(), v.clone());
                Some(v)
            } else {
                None
            }
        };
        let pic = match hit {
            Some(v) => v,
            None => {
                let Some(l) = self.make_layout(s, fmt, ul, strike) else { return };
                let mut tm = DWRITE_TEXT_METRICS::default();
                unsafe {
                    let _ = l.GetMetrics(&mut tm);
                }
                let w = (tm.widthIncludingTrailingWhitespace + 2.0).ceil().max(1.0);
                let h = self.cell_h.ceil();
                let made = unsafe {
                    let pf = D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
                    };
                    let size = windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: w, height: h };
                    (|| -> Option<Pic> {
                        let crt = rt
                            .CreateCompatibleRenderTarget(Some(&size), None, Some(&pf), D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE)
                            .ok()?;
                        crt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
                        let b = crt.CreateSolidColorBrush(&c.f(1.0), None).ok()?;
                        crt.BeginDraw();
                        crt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                        crt.DrawTextLayout(Vector2 { X: 0.0, Y: 0.0 }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        crt.EndDraw(None, None).ok()?;
                        Some((crt.GetBitmap().ok()?, w, h))
                    })()
                };
                match made {
                    Some(v) => {
                        let mut m = self.bmp_cache.borrow_mut();
                        if m.0.len() > 1500 {
                            m.1 = std::mem::take(&mut m.0);
                        }
                        m.0.insert(key, v.clone());
                        v
                    }
                    None => {
                        self.draw_layout(&l, x, y, c);
                        return;
                    }
                }
            }
        };
        let (x, y) = (x.round(), y.round());
        let dest = D2D_RECT_F { left: x, top: y, right: x + pic.1, bottom: y + pic.2 };
        unsafe { rt.DrawBitmap(&pic.0, Some(&dest), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None) };
    }

    pub fn draw_layout(&self, l: &IDWriteTextLayout, x: f32, y: f32, c: Rgb) {
        if let Some(rt) = &self.rt {
            unsafe {
                rt.DrawTextLayout(Vector2 { X: x, Y: y }, l, self.brush(c, 1.0), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT)
            };
        }
    }
}
