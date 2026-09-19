//! Broad kinds of file, by extension.
//!
//! The treemap colours by kind and the summary groups by it, so the buckets
//! are chosen for what a person deciding what to delete would want told
//! apart: a 40 GB disk image and a 40 GB video collection are very different
//! conversations.

/// A broad family of file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Video,
    Image,
    Audio,
    Archive,
    /// Virtual machine disks and optical images.
    DiskImage,
    Document,
    Code,
    /// Programs, libraries and installers.
    Executable,
    Database,
    /// Packed game assets.
    Game,
    /// Machine-learning model weights.
    Model,
    /// Page file, hibernation file, drivers, logs and caches of the OS.
    System,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 13] = [
        Kind::Video,
        Kind::Image,
        Kind::Audio,
        Kind::Archive,
        Kind::DiskImage,
        Kind::Document,
        Kind::Code,
        Kind::Executable,
        Kind::Database,
        Kind::Game,
        Kind::Model,
        Kind::System,
        Kind::Other,
    ];

    /// Classify a file by its name.
    pub fn of(name: &str) -> Kind {
        // The three biggest files on most Windows disks share an extension
        // with drivers, and are nothing like them.
        for special in ["pagefile.sys", "hiberfil.sys", "swapfile.sys"] {
            if name.eq_ignore_ascii_case(special) {
                return Kind::System;
            }
        }
        let ext = extension(name);
        if ext.is_empty() || ext.len() > 12 {
            return Kind::Other;
        }
        let mut buf = [0u8; 12];
        let lower = &mut buf[..ext.len()];
        lower.copy_from_slice(ext.as_bytes());
        lower.make_ascii_lowercase();
        let Ok(ext) = std::str::from_utf8(lower) else {
            return Kind::Other;
        };

        match ext {
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg"
            | "ts" | "m2ts" | "3gp" | "vob" => Kind::Video,
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff"
            | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" | "avif" => {
                Kind::Image
            }
            "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "wma" | "opus" | "aiff" => Kind::Audio,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" | "lz4" => {
                Kind::Archive
            }
            "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow2" | "avhdx" | "wim" | "esd"
            | "dmg" => Kind::DiskImage,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
            | "txt" | "rtf" | "md" | "epub" | "csv" => Kind::Document,
            "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "js" | "py" | "go" | "rb"
            | "php" | "html" | "css" | "json" | "xml" | "yml" | "yaml" | "toml" | "rlib"
            | "rmeta" | "o" | "obj" | "pdb" | "class" | "jar" | "pyc" | "map" => Kind::Code,
            "exe" | "dll" | "msi" | "msp" | "msix" | "appx" | "appxbundle" | "msixbundle"
            | "sys" | "ocx" | "so" | "node" | "bin" => Kind::Executable,
            "db" | "sqlite" | "sqlite3" | "mdb" | "accdb" | "ldf" | "mdf" | "edb" | "ndf"
            | "db-wal" | "pst" | "ost" => Kind::Database,
            "rpf" | "pak" | "vpk" | "bsa" | "ba2" | "forge" | "ucas" | "utoc" | "uasset"
            | "assets" | "resource" | "wad" | "gcf" => Kind::Game,
            "gguf" | "safetensors" | "ckpt" | "onnx" | "pth" | "pt" | "tflite" | "mlmodel" => {
                Kind::Model
            }
            "etl" | "log" | "dmp" | "tmp" | "evtx" | "cat" | "mum" | "manifest" | "blf"
            | "regtrans" | "dat" | "hve" | "nvph" => Kind::System,
            _ => Kind::Other,
        }
    }

    /// A stable key for translations and settings.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Archive => "archive",
            Kind::DiskImage => "disk_image",
            Kind::Document => "document",
            Kind::Code => "code",
            Kind::Executable => "executable",
            Kind::Database => "database",
            Kind::Game => "game",
            Kind::Model => "model",
            Kind::System => "system",
            Kind::Other => "other",
        }
    }
}

/// The part of `name` after its last dot, or `""` when there is none.
/// A leading dot (`.gitignore`) is a hidden name, not an extension.
pub fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(0) | None => "",
        Some(dot) => &name[dot + 1..],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_ignoring_case() {
        assert_eq!(Kind::of("Tatil.MP4"), Kind::Video);
        assert_eq!(Kind::of("ubuntu.iso"), Kind::DiskImage);
        assert_eq!(Kind::of("ext4.vhdx"), Kind::DiskImage);
        assert_eq!(Kind::of("setup.exe"), Kind::Executable);
        assert_eq!(Kind::of("pagefile"), Kind::Other);
        assert_eq!(Kind::of("pagefile.sys"), Kind::System);
        assert_eq!(Kind::of("HIBERFIL.SYS"), Kind::System);
        assert_eq!(Kind::of("nvlddmkm.sys"), Kind::Executable);
        assert_eq!(Kind::of("update.rpf"), Kind::Game);
        assert_eq!(Kind::of("Qwen3.5-9B-Q4_K_M.gguf"), Kind::Model);
        assert_eq!(Kind::of("a.verylongextension"), Kind::Other);
    }

    #[test]
    fn a_leading_dot_is_not_an_extension() {
        assert_eq!(extension(".gitignore"), "");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("README"), "");
    }

    #[test]
    fn non_ascii_extensions_do_not_panic() {
        assert_eq!(Kind::of("dosya.çğü"), Kind::Other);
    }
}
