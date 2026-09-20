//! Interface text, in Turkish and English.
//!
//! Only the chrome is translated. File names and paths are shown exactly as
//! the disk has them. Failures reach the window as `code` or `code:detail`
//! and become words here, so switching language rewords a message that is
//! already on screen.
//!
//! Plain strings live in a struct and the ones that take an argument are
//! methods — a missing `{}` in one language's format string is the kind of
//! mistake that only shows when somebody switches to it.

use burrow_tree::Kind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Lang {
    Tr,
    En,
}

impl Default for Lang {
    /// Turkish when Windows is Turkish, English otherwise.
    fn default() -> Self {
        if windows_is_turkish() {
            Lang::Tr
        } else {
            Lang::En
        }
    }
}

/// Every string that needs no argument.
pub struct Strings {
    pub drive_tooltip: &'static str,
    pub rescan_tooltip: &'static str,
    pub theme_tooltip: &'static str,
    pub language_tooltip: &'static str,
    pub metric_on_disk: &'static str,
    pub metric_size: &'static str,
    pub metric_tooltip: &'static str,

    pub tab_folders: &'static str,
    pub tab_largest: &'static str,
    pub tab_kinds: &'static str,
    pub tab_duplicates: &'static str,
    pub dupes_intro: &'static str,
    pub dupes_min: &'static str,
    pub dupes_start: &'static str,
    pub dupes_stop: &'static str,
    pub dupes_none: &'static str,
    pub dupes_cancelled: &'static str,

    pub col_name: &'static str,
    pub col_share: &'static str,
    pub col_size: &'static str,
    pub col_files: &'static str,
    pub col_modified: &'static str,
    pub col_folder: &'static str,
    pub col_kind: &'static str,
    pub col_extension: &'static str,
    pub no_extension: &'static str,

    pub menu_open: &'static str,
    pub menu_reveal: &'static str,
    pub menu_copy: &'static str,
    pub menu_zoom: &'static str,

    pub map_up: &'static str,
    pub map_hint: &'static str,
    pub legend_other: &'static str,
    pub hardlink_note: &'static str,
    pub cloud_note: &'static str,

    pub used: &'static str,
    pub free: &'static str,
    pub unexplained_tooltip: &'static str,

    pub preparing: &'static str,
    pub scanning_detail: &'static str,
    pub scan_failed: &'static str,
    pub retry: &'static str,
    pub elevation_title: &'static str,
    pub elevation_detail: &'static str,
    pub elevation_action: &'static str,
    pub no_volume_title: &'static str,
    pub no_volume_detail: &'static str,
    pub not_ntfs: &'static str,
    pub copied: &'static str,
    pub live_tooltip: &'static str,
    pub not_live: &'static str,
    pub stale: &'static str,
}

const TR: Strings = Strings {
    drive_tooltip: "Sürücü",
    rescan_tooltip: "Yeniden tara (F5)",
    theme_tooltip: "Tema değiştir",
    language_tooltip: "Dil",
    metric_on_disk: "Diskte",
    metric_size: "Boyut",
    metric_tooltip: concat!(
        "Diskte: dosyaların gerçekten kapladığı alan. Diskin neden dolu olduğunu bu gösterir.\n",
        "Boyut: Explorer'ın gösterdiği mantıksal boyut."
    ),

    tab_folders: "Klasörler",
    tab_largest: "En büyük dosyalar",
    tab_kinds: "Türler",
    tab_duplicates: "Kopyalar",
    dupes_intro: concat!(
        "İçeriği birebir aynı dosyaları bulur. Önce boyutlar karşılaştırılır (diski okumadan), ",
        "sonra yalnızca eşleşenlerin başı ve sonu, en son da hâlâ eşleşenlerin tamamı okunur. ",
        "Yalnızca bulutta duran dosyalar okunmaz, yani indirilmez."
    ),
    dupes_min: "En az",
    dupes_start: "Kopyaları bul",
    dupes_stop: "Durdur",
    dupes_none: "Kopya dosya bulunamadı.",
    dupes_cancelled: "Arama durduruldu; liste eksik olabilir.",

    col_name: "Ad",
    col_share: "Pay",
    col_size: "Boyut",
    col_files: "Dosya",
    col_modified: "Değişiklik",
    col_folder: "Konum",
    col_kind: "Tür",
    col_extension: "Uzantı",
    no_extension: "(uzantısız)",

    menu_open: "Aç",
    menu_reveal: "Klasörde göster",
    menu_copy: "Yolu kopyala",
    menu_zoom: "Haritada buraya odaklan",

    map_up: "⬆ Üst klasör",
    map_hint: "Tık: seç  ·  Çift tık: içine gir  ·  Sağ tık: menü  ·  Geri: üst klasör",
    legend_other: "Diğer",
    hardlink_note: "Başka adlarla da bağlı (hard link); alanı bir kez sayıldı.",
    cloud_note: "Yalnızca bulutta; diskte yer kaplamıyor.",

    used: "dolu",
    free: "boş",
    unexplained_tooltip: concat!(
        "Hiçbir dosyaya ait olmayan dolu alan: gölge kopyalar (sistem geri yükleme noktaları), ",
        "NTFS'in kendi kayıtları ve taramadan bu yana değişenler."
    ),

    preparing: "Hazırlanıyor…",
    scanning_detail: "Ana dosya tablosu okunuyor. Bu, diskin tamamı için birkaç saniye sürer.",
    scan_failed: "Taranamadı",
    retry: "Tekrar dene",
    elevation_title: "Yönetici izni gerekiyor",
    elevation_detail: concat!(
        "Burrow, diski doğrudan okuyarak saniyeler içinde tarıyor; Windows bunun için ",
        "yönetici izni istiyor. Diske yalnızca okuma yapılır, hiçbir şey yazılmaz."
    ),
    elevation_action: "Yönetici olarak yeniden başlat",
    no_volume_title: "NTFS sürücüsü bulunamadı",
    no_volume_detail: "Burrow şimdilik yalnızca NTFS birimlerini okuyabiliyor.",
    not_ntfs: "NTFS değil",
    copied: "Yol kopyalandı.",
    live_tooltip: "Disk izleniyor: dosyalar eklendikçe, silindikçe ve büyüdükçe harita kendini günceller. Yeniden taramaya gerek yok.",
    not_live: "anlık görüntü",
    stale: "Disk, izlenemeyecek kadar çok değişti. Güncel görmek için F5 ile yeniden tarayın.",
};

const EN: Strings = Strings {
    drive_tooltip: "Drive",
    rescan_tooltip: "Rescan (F5)",
    theme_tooltip: "Switch theme",
    language_tooltip: "Language",
    metric_on_disk: "On disk",
    metric_size: "Size",
    metric_tooltip: concat!(
        "On disk: the space files actually occupy. This is what explains a full drive.\n",
        "Size: the logical size Explorer shows."
    ),

    tab_folders: "Folders",
    tab_largest: "Largest files",
    tab_kinds: "Kinds",
    tab_duplicates: "Duplicates",
    dupes_intro: concat!(
        "Finds files with byte-for-byte identical contents. Sizes are compared first (without reading the disk), ",
        "then only the matches have their first and last bytes read, and only files that still match are read in full. ",
        "Cloud-only files are never read, so never downloaded."
    ),
    dupes_min: "At least",
    dupes_start: "Find duplicates",
    dupes_stop: "Stop",
    dupes_none: "No duplicate files found.",
    dupes_cancelled: "Search stopped; the list may be incomplete.",

    col_name: "Name",
    col_share: "Share",
    col_size: "Size",
    col_files: "Files",
    col_modified: "Modified",
    col_folder: "Location",
    col_kind: "Kind",
    col_extension: "Extension",
    no_extension: "(none)",

    menu_open: "Open",
    menu_reveal: "Show in folder",
    menu_copy: "Copy path",
    menu_zoom: "Focus the map here",

    map_up: "⬆ Up",
    map_hint: "Click: select  ·  Double-click: go in  ·  Right-click: menu  ·  Backspace: up",
    legend_other: "Other",
    hardlink_note: "Also linked under other names (hard link); its space is counted once.",
    cloud_note: "Cloud-only; takes no space on this disk.",

    used: "used",
    free: "free",
    unexplained_tooltip: concat!(
        "Used space that belongs to no file: shadow copies (system restore points), ",
        "NTFS's own bookkeeping, and whatever changed since the scan."
    ),

    preparing: "Getting ready…",
    scanning_detail: "Reading the master file table. For a whole disk this takes a few seconds.",
    scan_failed: "Could not scan",
    retry: "Try again",
    elevation_title: "Administrator rights needed",
    elevation_detail: concat!(
        "Burrow reads the disk directly, which is what makes a scan take seconds; ",
        "Windows requires administrator rights for that. The disk is only read, never written."
    ),
    elevation_action: "Restart as administrator",
    no_volume_title: "No NTFS drive found",
    no_volume_detail: "Burrow can only read NTFS volumes for now.",
    not_ntfs: "not NTFS",
    copied: "Path copied.",
    live_tooltip: "Watching the disk: the map updates itself as files are added, deleted and grow. No rescan needed.",
    not_live: "snapshot",
    stale: "The disk changed too much to follow. Press F5 to rescan.",
};

impl Lang {
    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::Tr => &TR,
            Lang::En => &EN,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Tr => "TR",
            Lang::En => "EN",
        }
    }

    pub fn kind(self, kind: Kind) -> &'static str {
        match (self, kind) {
            (Lang::Tr, Kind::Video) => "Video",
            (Lang::Tr, Kind::Image) => "Görsel",
            (Lang::Tr, Kind::Audio) => "Ses",
            (Lang::Tr, Kind::Archive) => "Arşiv",
            (Lang::Tr, Kind::DiskImage) => "Disk kalıbı",
            (Lang::Tr, Kind::Document) => "Belge",
            (Lang::Tr, Kind::Code) => "Kod",
            (Lang::Tr, Kind::Executable) => "Program",
            (Lang::Tr, Kind::Database) => "Veritabanı",
            (Lang::Tr, Kind::Game) => "Oyun verisi",
            (Lang::Tr, Kind::Model) => "YZ modeli",
            (Lang::Tr, Kind::System) => "Sistem",
            (Lang::Tr, Kind::Other) => "Diğer",
            (Lang::En, Kind::Video) => "Video",
            (Lang::En, Kind::Image) => "Image",
            (Lang::En, Kind::Audio) => "Audio",
            (Lang::En, Kind::Archive) => "Archive",
            (Lang::En, Kind::DiskImage) => "Disk image",
            (Lang::En, Kind::Document) => "Document",
            (Lang::En, Kind::Code) => "Code",
            (Lang::En, Kind::Executable) => "Program",
            (Lang::En, Kind::Database) => "Database",
            (Lang::En, Kind::Game) => "Game data",
            (Lang::En, Kind::Model) => "AI model",
            (Lang::En, Kind::System) => "System",
            (Lang::En, Kind::Other) => "Other",
        }
    }

    pub fn scanning(self, letter: char) -> String {
        match self {
            Lang::Tr => format!("{letter}: taranıyor"),
            Lang::En => format!("Scanning {letter}:"),
        }
    }

    pub fn small_items(self, count: usize) -> String {
        let count = self.number(count as u64);
        match self {
            Lang::Tr => format!("{count} küçük öğe"),
            Lang::En => format!("{count} small items"),
        }
    }

    /// The live badge: how long ago the scan behind this picture was made.
    /// A count of changes says how busy the disk is, which is not what
    /// anybody came to find out; how old the picture is, is.
    pub fn since_scan(self, seconds: u64) -> String {
        let when = match (self, seconds) {
            (Lang::Tr, s) if s < 10 => "az önce".to_string(),
            (Lang::Tr, s) if s < 60 => format!("{s} sn önce"),
            (Lang::Tr, s) if s < 3600 => format!("{} dk önce", s / 60),
            (Lang::Tr, s) => format!("{} sa önce", s / 3600),
            (Lang::En, s) if s < 10 => "just now".to_string(),
            (Lang::En, s) if s < 60 => format!("{s} s ago"),
            (Lang::En, s) if s < 3600 => format!("{} min ago", s / 60),
            (Lang::En, s) => format!("{} h ago", s / 3600),
        };
        match self {
            Lang::Tr => format!("canlı · tarama {when}"),
            Lang::En => format!("live · scanned {when}"),
        }
    }

    /// What the live badge says on hover: the watching, and how much of it
    /// there has been.
    pub fn live_detail(self, changes: u64) -> String {
        let count = self.number(changes);
        let watching = self.strings().live_tooltip;
        match self {
            Lang::Tr => format!(
                "{watching}

Taramadan beri {count} değişiklik izlendi."
            ),
            Lang::En => format!(
                "{watching}

{count} changes followed since the scan."
            ),
        }
    }

    pub fn dupes_progress(self, done: &str, total: &str, groups: usize) -> String {
        let groups = self.number(groups as u64);
        match self {
            Lang::Tr => format!("{done} / {total} okundu · {groups} grup"),
            Lang::En => format!("{done} of {total} read · {groups} groups"),
        }
    }

    /// What the search found: how much of the waste is Burrow's to free.
    pub fn dupes_summary(self, groups: usize, reclaimable: &str, seconds: f64) -> String {
        let groups = self.number(groups as u64);
        match self {
            Lang::Tr => {
                format!("{groups} kopya grubu · {reclaimable} geri kazanılabilir · {seconds:.0} sn")
            }
            Lang::En => {
                format!(
                    "{groups} groups of duplicates · {reclaimable} reclaimable · {seconds:.0} s"
                )
            }
        }
    }

    /// The rest: real waste, in places nothing here may touch.
    pub fn dupes_locked(self, bytes: &str) -> String {
        match self {
            Lang::Tr => format!(
                "Kilitli {bytes} daha var: uygulamaların ve Windows'un kendi klasörlerinde, buradan silinemez."
            ),
            Lang::En => format!(
                "Another {bytes} is locked: it sits in applications' and Windows' own folders."
            ),
        }
    }

    /// 3 × 146 MB.
    pub fn dupes_group(self, count: usize, size: &str) -> String {
        match self {
            Lang::Tr => format!("{count} kopya × {size}"),
            Lang::En => format!("{count} copies × {size}"),
        }
    }

    pub fn files(self, count: u64) -> String {
        let count = self.number(count);
        match self {
            Lang::Tr => format!("{count} dosya"),
            Lang::En => format!("{count} files"),
        }
    }

    /// `412 GB dolu · 52 GB boş · 464 GB`.
    pub fn volume_usage(self, used: &str, free: &str, total: &str) -> String {
        let s = self.strings();
        format!("{used} {}  ·  {free} {}  ·  {total}", s.used, s.free)
    }

    /// How much of the used space the scan could attribute to files.
    pub fn unexplained(self, bytes: &str) -> String {
        match self {
            Lang::Tr => format!("{bytes} hiçbir dosyada değil"),
            Lang::En => format!("{bytes} not in any file"),
        }
    }

    pub fn status_line(
        self,
        letter: char,
        files: u64,
        dirs: u64,
        seconds: f64,
        memory: &str,
    ) -> String {
        let files = self.number(files);
        let dirs = self.number(dirs);
        match self {
            Lang::Tr => format!(
                "{letter}:  {files} dosya · {dirs} klasör · {} sn'de tarandı · {memory} bellek",
                format!("{seconds:.1}").replace('.', ",")
            ),
            Lang::En => format!(
                "{letter}:  {files} files · {dirs} folders · scanned in {seconds:.1} s · {memory} memory"
            ),
        }
    }

    pub fn title(self, letter: char, used: &str) -> String {
        match self {
            Lang::Tr => format!("Burrow — {letter}: {used} dolu"),
            Lang::En => format!("Burrow — {letter}: {used} used"),
        }
    }

    /// Turn a `code` or `code:detail` failure into a sentence. Anything
    /// unrecognised is shown as it arrived.
    pub fn error(self, raw: &str) -> String {
        let (code, detail) = raw.split_once(':').unwrap_or((raw, ""));
        match (self, code) {
            (Lang::Tr, "needs_elevation") => "Erişim reddedildi. Yönetici izni gerekiyor.".into(),
            (Lang::En, "needs_elevation") => {
                "Access denied. Administrator rights are needed.".into()
            }
            (Lang::Tr, "scan_failed") => format!("Taranamadı: {detail}"),
            (Lang::En, "scan_failed") => format!("Could not scan: {detail}"),
            (Lang::Tr, "open_failed") => format!("Açılamadı: {detail}"),
            (Lang::En, "open_failed") => format!("Could not open: {detail}"),
            (Lang::Tr, "restart_failed") => format!("Yeniden başlatılamadı: {detail}"),
            (Lang::En, "restart_failed") => format!("Could not restart: {detail}"),
            _ => raw.to_string(),
        }
    }

    /// 1.559.903 in Turkish, 1,559,903 in English.
    pub fn number(self, value: u64) -> String {
        let separator = match self {
            Lang::Tr => '.',
            Lang::En => ',',
        };
        let digits = value.to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (position, digit) in digits.chars().enumerate() {
            if position > 0 && (digits.len() - position).is_multiple_of(3) {
                out.push(separator);
            }
            out.push(digit);
        }
        out
    }
}

/// Whether the user's Windows display language is Turkish.
///
/// The `LANG` family is checked first so a developer can override it; then
/// the user's locale name as Windows reports it.
fn windows_is_turkish() -> bool {
    for key in ["LANG", "LC_ALL", "LANGUAGE"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return value.to_ascii_lowercase().starts_with("tr");
            }
        }
    }
    crate::shell::user_locale()
        .map(|tag| tag.to_ascii_lowercase().starts_with("tr"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_the_way_each_language_writes_them() {
        assert_eq!(Lang::Tr.number(1_559_903), "1.559.903");
        assert_eq!(Lang::En.number(1_559_903), "1,559,903");
        assert_eq!(Lang::En.number(999), "999");
    }

    #[test]
    fn every_kind_has_a_name_in_both_languages() {
        for lang in [Lang::Tr, Lang::En] {
            for kind in Kind::ALL {
                assert!(!lang.kind(kind).is_empty());
            }
        }
    }

    #[test]
    fn the_parameterised_strings_carry_their_argument() {
        for lang in [Lang::Tr, Lang::En] {
            assert!(lang.scanning('C').contains('C'));
            assert!(lang.small_items(1234).contains("1"));
            assert!(lang.unexplained("9 GB").contains("9 GB"));
            let usage = lang.volume_usage("400 GB", "64 GB", "464 GB");
            assert!(
                usage.contains("400 GB") && usage.contains("64 GB") && usage.contains("464 GB")
            );
            let line = lang.status_line('C', 1000, 20, 7.25, "180 MB");
            assert!(line.starts_with("C:") && line.contains("180 MB"), "{line}");
        }
    }

    #[test]
    fn codes_become_sentences_and_unknowns_pass_through() {
        assert!(Lang::Tr.error("needs_elevation").contains("Yönetici"));
        assert_eq!(Lang::En.error("scan_failed:boom"), "Could not scan: boom");
        assert_eq!(Lang::En.error("Os { code: 5 }"), "Os { code: 5 }");
    }
}
