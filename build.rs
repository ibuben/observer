use std::path::{Path, PathBuf};

use image::ImageEncoder;

fn main() {
    println!("cargo:rerun-if-changed=Observer.png");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out.join("observer.ico");
    std::fs::write(&ico_path, make_ico(&manifest.join("Observer.png"))).unwrap();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let rc_path = out.join("observer.rc");
        let icon = ico_path.to_string_lossy().replace('\\', "/");
        let rc = format!("1 ICON \"{icon}\"\n{VERSION_INFO}\n");
        let mut encoded = vec![0xFF, 0xFE];
        for unit in rc.encode_utf16() {
            encoded.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&rc_path, encoded).unwrap();
        embed_resource::compile(&rc_path, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}

const VERSION_INFO: &str = r#"
1 VERSIONINFO
FILEVERSION 0,9,1,0
PRODUCTVERSION 0,9,1,0
FILEFLAGSMASK 0x3F
FILEFLAGS 0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "041904B0"
        BEGIN
            VALUE "CompanyName", "eXUnity LAB"
            VALUE "FileDescription", "eXUnity Observer"
            VALUE "FileVersion", "0.9.1"
            VALUE "InternalName", "Observer"
            VALUE "LegalCopyright", "© eXUnity LAB"
            VALUE "OriginalFilename", "xm-view.exe"
            VALUE "ProductName", "eXUnity Observer"
            VALUE "ProductVersion", "0.9.1"
            VALUE "Comments", "Программа видеонаблюдения"
        END
        BLOCK "040904B0"
        BEGIN
            VALUE "CompanyName", "eXUnity LAB"
            VALUE "FileDescription", "eXUnity Observer"
            VALUE "FileVersion", "0.9.1"
            VALUE "InternalName", "Observer"
            VALUE "LegalCopyright", "© eXUnity LAB"
            VALUE "OriginalFilename", "xm-view.exe"
            VALUE "ProductName", "eXUnity Observer"
            VALUE "ProductVersion", "0.9.1"
            VALUE "Comments", "Программа видеонаблюдения"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x419, 1200
        VALUE "Translation", 0x409, 1200
    END
END
"#;

fn make_ico(path: &Path) -> Vec<u8> {
    let image = image::open(path).expect("Observer.png").to_rgba8();
    let trimmed = trim_transparent(&image);
    let mut pngs = Vec::new();
    for side in [16u32, 24, 32, 48, 64, 128, 256] {
        let square = fit_square(&trimmed, side);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                square.as_raw(),
                side,
                side,
                image::ExtendedColorType::Rgba8,
            )
            .expect("png");
        pngs.push((side, png));
    }
    pack_ico(&pngs)
}

fn trim_transparent(image: &image::RgbaImage) -> image::RgbaImage {
    let (width, height) = image.dimensions();
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    for (x, y, pixel) in image.enumerate_pixels() {
        if pixel[3] > 8 {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    if min_x > max_x || min_y > max_y {
        return image.clone();
    }
    image::imageops::crop_imm(image, min_x, min_y, max_x - min_x + 1, max_y - min_y + 1).to_image()
}

fn fit_square(image: &image::RgbaImage, side: u32) -> image::RgbaImage {
    let (width, height) = image.dimensions();
    let scale = (side as f32 * 0.88) / width.max(height).max(1) as f32;
    let fitted_w = ((width as f32) * scale).round().max(1.0) as u32;
    let fitted_h = ((height as f32) * scale).round().max(1.0) as u32;
    let fitted = image::imageops::resize(
        image,
        fitted_w,
        fitted_h,
        image::imageops::FilterType::Lanczos3,
    );
    let mut canvas = image::RgbaImage::from_pixel(side, side, image::Rgba([0, 0, 0, 0]));
    image::imageops::overlay(
        &mut canvas,
        &fitted,
        ((side - fitted_w) / 2) as i64,
        ((side - fitted_h) / 2) as i64,
    );
    canvas
}

fn pack_ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    let mut body = Vec::new();
    for (side, png) in images {
        let dim = if *side >= 256 { 0 } else { *side as u8 };
        file.push(dim);
        file.push(dim);
        file.push(0);
        file.push(0);
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&(png.len() as u32).to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
        body.extend_from_slice(png);
    }
    file.extend(body);
    file
}
