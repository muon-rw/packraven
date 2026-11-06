use std::io::Cursor;

use anyhow::{Context, Result};
use base64::Engine;
use image::imageops::FilterType;
use image::ImageFormat;

use crate::pack::{Pack, SERVER_ICON};

// Vanilla ignores a server icon that isn't exactly 64x64.
const SIZE: u32 = 64;

pub fn data_url(pack: &Pack) -> Option<String> {
    let png = std::fs::read(pack.dir.join(SERVER_ICON)).ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

pub fn save(pack: &Pack, image: &[u8]) -> Result<()> {
    let image = image::load_from_memory(image).context("packraven can't read that image. Choose a PNG, JPEG, GIF or WebP file")?;
    let side = image.width().min(image.height());
    let square = image.crop_imm((image.width() - side) / 2, (image.height() - side) / 2, side, side);
    // Smoothing would blur a small pixel-art icon, so we only smooth when scaling down.
    let filter = if side < SIZE { FilterType::Nearest } else { FilterType::Lanczos3 };
    let mut png = Vec::new();
    square.resize_exact(SIZE, SIZE, filter).write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
    std::fs::write(pack.dir.join(SERVER_ICON), png).context("writing server-icon.png")
}

pub async fn project_logo(project_id: u64) -> Result<Vec<u8>> {
    if project_id == 0 {
        anyhow::bail!("Set the CurseForge project ID in Pack settings first");
    }
    let project = crate::curseforge::mods(&[project_id]).await?.remove(&project_id).context("CurseForge has no project with this pack's ID")?;
    let logo = project.logo.as_ref().and_then(crate::curseforge::CfLogo::url).context("The CurseForge project has no logo yet")?;
    Ok(crate::http::CLIENT.get(logo).send().await?.error_for_status()?.bytes().await?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{Lockfile, PackConfig};

    fn pack_in(dir: &std::path::Path) -> Pack {
        Pack { dir: dir.to_path_buf(), config: PackConfig::default(), lock: Lockfile::default() }
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::new(width, height).write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn saves_any_image_as_a_64px_square() {
        let dir = std::env::temp_dir().join(format!("packraven-icon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = pack_in(&dir);
        for (width, height) in [(300, 200), (16, 16), (64, 64)] {
            save(&pack, &png(width, height)).unwrap();
            let icon = image::open(dir.join(SERVER_ICON)).unwrap();
            assert_eq!((icon.width(), icon.height()), (SIZE, SIZE));
        }
        assert!(data_url(&pack).unwrap().starts_with("data:image/png;base64,"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
