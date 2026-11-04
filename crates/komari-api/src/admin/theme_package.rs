//! Theme ZIP inspection and installation.
//!
//! Mirrors Go's `extractAndValidateTheme` / `peekThemeFromZip`, with three
//! deliberate hardening additions:
//!
//! * entry count, config size and uncompressed-size limits (zip bombs);
//! * extraction into a staging directory and an atomic rename, so a failed
//!   upload never leaves a half-written theme and never destroys the previous
//!   copy of a same-`short` theme;
//! * path traversal entries are skipped (Go skips them too) and the staging
//!   root is re-checked before every write.
//!
//! Theme metadata parsing and `short` validation come from `komari-web`, the
//! same code the static router uses.

use std::fs;
use std::io::{self, Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use komari_web::{
    THEME_CONFIG_FILE, THEMES_DIR, ThemeError, ThemeInfo, is_valid_theme_short, sanitize_relative,
};
use zip::ZipArchive;

/// Maximum number of entries in a theme archive.
pub(crate) const MAX_THEME_ENTRIES: usize = 20_000;
/// Maximum total uncompressed size extracted from one theme archive.
pub(crate) const MAX_THEME_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum size of `komari-theme.json` itself.
const MAX_THEME_CONFIG_BYTES: u64 = 1024 * 1024;

/// Read and validate only `komari-theme.json` (Go `peekThemeFromZip`).
pub(crate) fn peek_theme(data: &[u8]) -> Result<ThemeInfo, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|error| format!("无法打开ZIP文件: {error}"))?;
    read_theme_config(
        &mut archive,
        "主题配置文件 komari-theme.json 不存在，不是合法的主题包",
    )
}

/// Validate and install a theme package below `<data_dir>/theme/<short>`.
///
/// The previous directory with the same `short` is replaced only after the new
/// package has been extracted successfully.
pub(crate) fn install_theme(data: Vec<u8>, data_dir: &Path) -> Result<ThemeInfo, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|error| format!("无法打开ZIP文件: {error}"))?;
    let info = read_theme_config(&mut archive, "主题配置文件 komari-theme.json 不存在")?;
    extract_and_install(&mut archive, &info, data_dir)?;
    Ok(info)
}

fn read_theme_config<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    missing: &str,
) -> Result<ThemeInfo, String> {
    let file = match archive.by_name(THEME_CONFIG_FILE) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Err(missing.to_owned()),
        Err(error) => return Err(format!("无法读取主题配置文件: {error}")),
    };

    let mut data = Vec::new();
    file.take(MAX_THEME_CONFIG_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|error| format!("读取主题配置失败: {error}"))?;
    if data.len() as u64 > MAX_THEME_CONFIG_BYTES {
        return Err("主题配置文件 komari-theme.json 过大".to_owned());
    }

    let info = match ThemeInfo::from_slice(&data) {
        Ok(info) => info,
        Err(ThemeError::Json(error)) => {
            return Err(format!("主题配置格式错误: {error}"));
        }
        Err(ThemeError::MissingFields(_)) => {
            return Err("主题配置缺少必填字段（name、short）".to_owned());
        }
    };

    if !is_valid_theme_short(&info.short) {
        return Err("主题short字段格式无效，只允许字母、数字、下划线和连字符".to_owned());
    }

    Ok(info)
}

fn extract_and_install<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    info: &ThemeInfo,
    data_dir: &Path,
) -> Result<(), String> {
    if archive.len() > MAX_THEME_ENTRIES {
        return Err(format!("主题包文件数量过多（超过 {MAX_THEME_ENTRIES} 个）"));
    }

    let themes_dir = data_dir.join(THEMES_DIR);
    let staging_root = data_dir.join(".theme-staging");
    fs::create_dir_all(&themes_dir).map_err(|error| format!("创建主题目录失败: {error}"))?;
    fs::create_dir_all(&staging_root).map_err(|error| format!("创建主题目录失败: {error}"))?;

    let staging = unique_path(&staging_root, &info.short, "tmp");
    fs::create_dir(&staging).map_err(|error| format!("创建主题目录失败: {error}"))?;

    if let Err(error) = write_entries(archive, &staging) {
        let _ = fs::remove_dir_all(&staging);
        cleanup_staging_root(&staging_root);
        return Err(error);
    }

    let target = themes_dir.join(&info.short);
    let mut backup = None;
    if target.exists() {
        let old = unique_path(&staging_root, &info.short, "old");
        if let Err(error) = fs::rename(&target, &old) {
            let _ = fs::remove_dir_all(&staging);
            cleanup_staging_root(&staging_root);
            return Err(format!("删除原有主题失败: {error}"));
        }
        backup = Some(old);
    }

    match fs::rename(&staging, &target) {
        Ok(()) => {
            if let Some(old) = backup {
                let _ = fs::remove_dir_all(old);
            }
            cleanup_staging_root(&staging_root);
            Ok(())
        }
        Err(error) => {
            if let Some(old) = backup {
                let _ = fs::rename(old, &target);
            }
            let _ = fs::remove_dir_all(&staging);
            cleanup_staging_root(&staging_root);
            Err(format!("安装主题失败: {error}"))
        }
    }
}

fn write_entries<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    staging: &Path,
) -> Result<(), String> {
    let mut budget = MAX_THEME_UNCOMPRESSED_BYTES;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("打开压缩文件失败: {error}"))?;

        // `sanitize_relative` rejects `..`, backslashes and NUL bytes. Go skips
        // escaping entries instead of failing the whole package; do the same.
        let Some(relative) = sanitize_relative(entry.name()) else {
            continue;
        };
        let path = staging.join(&relative);
        if !path.starts_with(staging) {
            continue;
        }

        if entry.is_dir() {
            fs::create_dir_all(&path).map_err(|error| format!("创建目录失败: {error}"))?;
            continue;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建目录失败: {error}"))?;
        }

        if entry.size() > budget {
            return Err(format!(
                "主题包解压后过大（超过 {} MiB）",
                MAX_THEME_UNCOMPRESSED_BYTES / (1024 * 1024)
            ));
        }

        let mut output =
            fs::File::create(&path).map_err(|error| format!("创建文件失败: {error}"))?;
        // Never trust the declared size: stop one byte past the remaining
        // budget so a lying header cannot inflate without bound.
        let copied = io::copy(&mut (&mut entry).take(budget + 1), &mut output)
            .map_err(|error| format!("解压文件失败: {error}"))?;
        if copied > budget {
            return Err(format!(
                "主题包解压后过大（超过 {} MiB）",
                MAX_THEME_UNCOMPRESSED_BYTES / (1024 * 1024)
            ));
        }
        budget -= copied;
        output
            .flush()
            .map_err(|error| format!("解压文件失败: {error}"))?;
    }

    Ok(())
}

fn unique_path(root: &Path, short: &str, kind: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut candidate = root.join(format!("{short}.{kind}-{}-{nanos}", std::process::id()));
    let mut suffix = 0u32;
    while candidate.exists() {
        suffix += 1;
        candidate = root.join(format!(
            "{short}.{kind}-{}-{nanos}-{suffix}",
            std::process::id()
        ));
    }
    candidate
}

fn cleanup_staging_root(root: &Path) {
    // Only remove the directory when it is empty; a concurrent install may be
    // using it.
    let _ = fs::remove_dir(root);
}
