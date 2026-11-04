use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfPageImage {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::PdfPageImage;
    use core_foundation::base::TCFType;
    use core_foundation::url::CFURL;
    use core_graphics::color::CGColor;
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    use core_graphics::geometry::{CGAffineTransform, CGPoint, CGRect, CGSize};
    use core_graphics::image::CGImageAlphaInfo;
    use foreign_types::ForeignType;
    use std::ffi::c_void;
    use std::hash::{Hash, Hasher};
    use std::os::raw::c_int;
    use std::path::{Path, PathBuf};

    type CGPDFDocumentRef = *mut c_void;
    type CGPDFPageRef = *mut c_void;
    type CGContextRef = *mut c_void;

    const KCG_PDF_MEDIA_BOX: c_int = 0;
    const PDF_RENDER_SCALE: f32 = 2.0;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPDFDocumentCreateWithURL(url: *const c_void) -> CGPDFDocumentRef;
        fn CGPDFDocumentRelease(document: CGPDFDocumentRef);
        fn CGPDFDocumentGetNumberOfPages(document: CGPDFDocumentRef) -> usize;
        fn CGPDFDocumentGetPage(document: CGPDFDocumentRef, page_number: usize) -> CGPDFPageRef;
        fn CGPDFPageGetBoxRect(page: CGPDFPageRef, box_type: c_int) -> CGRect;
        fn CGPDFPageGetRotationAngle(page: CGPDFPageRef) -> c_int;
        fn CGPDFPageGetDrawingTransform(
            page: CGPDFPageRef,
            box_type: c_int,
            rect: CGRect,
            rotate: c_int,
            preserve_aspect_ratio: bool,
        ) -> CGAffineTransform;
        fn CGContextDrawPDFPage(context: CGContextRef, page: CGPDFPageRef);
    }

    struct PdfDocument(CGPDFDocumentRef);

    impl PdfDocument {
        fn open(path: &Path) -> Result<Self, String> {
            let url = CFURL::from_path(path, false)
                .ok_or_else(|| format!("Could not build a file URL for '{}'", path.display()))?;
            let document =
                unsafe { CGPDFDocumentCreateWithURL(url.as_concrete_TypeRef() as *const c_void) };
            if document.is_null() {
                return Err(format!("Could not open PDF '{}'", path.display()));
            }
            Ok(Self(document))
        }

        fn page(&self, index: usize) -> Result<CGPDFPageRef, String> {
            let page = unsafe { CGPDFDocumentGetPage(self.0, index + 1) };
            if page.is_null() {
                Err(format!("PDF page {} does not exist", index + 1))
            } else {
                Ok(page)
            }
        }
    }

    impl Drop for PdfDocument {
        fn drop(&mut self) {
            unsafe { CGPDFDocumentRelease(self.0) }
        }
    }

    pub fn page_count(path: &Path) -> Result<usize, String> {
        let document = PdfDocument::open(path)?;
        let count = unsafe { CGPDFDocumentGetNumberOfPages(document.0) };
        if count == 0 {
            Err(format!("PDF '{}' has no pages", path.display()))
        } else {
            Ok(count)
        }
    }

    pub fn render_page(path: &Path, page_index: usize, scale: f32) -> Result<PdfPageImage, String> {
        let cache = cache_path(path, page_index, scale);
        if let Ok((width, height)) = image::image_dimensions(&cache) {
            return Ok(PdfPageImage {
                path: cache,
                width,
                height,
            });
        }

        let document = PdfDocument::open(path)?;
        let page = document.page(page_index)?;
        let media_box = unsafe { CGPDFPageGetBoxRect(page, KCG_PDF_MEDIA_BOX) };
        let rotation = unsafe { CGPDFPageGetRotationAngle(page) };
        let (page_width, page_height) = if rotation.rem_euclid(180) == 90 {
            (media_box.size.height, media_box.size.width)
        } else {
            (media_box.size.width, media_box.size.height)
        };
        let scale = scale.max(0.5);
        let width = (page_width * scale as f64).round().max(1.0) as usize;
        let height = (page_height * scale as f64).round().max(1.0) as usize;

        let color_space = CGColorSpace::create_device_rgb();
        let bitmap_info = CGImageAlphaInfo::CGImageAlphaPremultipliedLast as u32;
        let mut context =
            CGContext::create_bitmap_context(None, width, height, 8, 0, &color_space, bitmap_info);
        let white = CGColor::rgb(1.0, 1.0, 1.0, 1.0);
        context.set_fill_color(&white);
        context.fill_rect(CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(width as f64, height as f64),
        ));
        let target = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(width as f64, height as f64),
        );
        let transform =
            unsafe { CGPDFPageGetDrawingTransform(page, KCG_PDF_MEDIA_BOX, target, 0, true) };
        context.concat_ctm(transform);
        unsafe { CGContextDrawPDFPage(context.as_ptr() as CGContextRef, page) };

        let bytes_per_row = context.bytes_per_row();
        let data = context.data();
        let mut rgba = vec![0u8; width * height * 4];
        for row in 0..height {
            let source_start = row * bytes_per_row;
            let source_end = source_start + width * 4;
            let target_start = row * width * 4;
            rgba[target_start..target_start + width * 4]
                .copy_from_slice(&data[source_start..source_end]);
        }
        let image = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
            .ok_or_else(|| "Could not build the PDF page image".to_string())?;
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create the PDF cache: {error}"))?;
        }
        image
            .save(&cache)
            .map_err(|error| format!("Could not save the PDF preview: {error}"))?;
        Ok(PdfPageImage {
            path: cache,
            width: width as u32,
            height: height as u32,
        })
    }

    fn cache_path(path: &Path, page_index: usize, scale: f32) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.to_string_lossy().hash(&mut hasher);
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                modified.hash(&mut hasher);
            }
        }
        page_index.hash(&mut hasher);
        scale.to_bits().hash(&mut hasher);
        std::env::temp_dir()
            .join("averroes-artifacts")
            .join(format!("pdf-{:016x}.png", hasher.finish()))
    }

    pub fn default_scale() -> f32 {
        PDF_RENDER_SCALE
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::PdfPageImage;
    use std::path::Path;

    pub fn page_count(_path: &Path) -> Result<usize, String> {
        Err("PDF preview is only available on macOS".into())
    }

    pub fn render_page(
        _path: &Path,
        _page_index: usize,
        _scale: f32,
    ) -> Result<PdfPageImage, String> {
        Err("PDF preview is only available on macOS".into())
    }

    pub fn default_scale() -> f32 {
        2.0
    }
}

pub use imp::{default_scale, page_count, render_page};

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{page_count, render_page};

    const MINIMAL_PDF: &str = concat!(
        r#"%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>
endobj
4 0 obj
<< /Length 37 >>
stream
BT /F1 24 Tf 50 100 Td (Hello) Tj ET
endstream
endobj
5 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
endobj
xref
0 6
"#,
        "0000000000 65535 f \n",
        "0000000009 00000 n \n",
        "0000000058 00000 n \n",
        "0000000115 00000 n \n",
        "0000000241 00000 n \n",
        "0000000327 00000 n \n",
        r#"trailer
<< /Size 6 /Root 1 0 R >>
startxref
397
%%EOF
"#
    );

    #[test]
    fn renders_a_minimal_pdf_page() {
        let path =
            std::env::temp_dir().join(format!("averroes-pdf-test-{}.pdf", std::process::id()));
        std::fs::write(&path, MINIMAL_PDF.as_bytes()).unwrap();

        assert_eq!(page_count(&path).unwrap(), 1);
        let page = render_page(&path, 0, 1.0).unwrap();
        assert!(page.width > 0);
        assert!(page.height > 0);
        assert!(page.path.exists());
        let dimensions = image::image_dimensions(&page.path).unwrap();
        assert_eq!(dimensions, (page.width, page.height));
        let rendered = image::open(&page.path).unwrap().to_rgba8();
        assert!(
            rendered.pixels().any(|pixel| pixel[0] < 250),
            "the rendered page should contain non-white content"
        );

        let _ = std::fs::remove_file(&path);
    }
}
