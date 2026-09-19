//! The words of cleaning up: what each rule found, why it is safe or not,
//! and what removing it will do. Kept apart from the rest of the interface
//! text because it is most of it.

use ferret_tree::cleanup::Safety;

use crate::i18n::Lang;

/// Every cleanup string that needs no argument.
pub struct Words {
    pub tab_cleanup: &'static str,
    pub cleanup_intro: &'static str,
    pub computing: &'static str,
    pub nothing_found: &'static str,
    pub recycle_selected: &'static str,

    pub menu_recycle: &'static str,
    pub protected: &'static str,

    pub confirm_title: &'static str,
    pub confirm_detail: &'static str,
    pub confirm_action: &'static str,
    pub cancel: &'static str,
    pub recycling: &'static str,

    pub keep_one: &'static str,
    pub clear_selection: &'static str,

    pub open_recycle_bin: &'static str,
    pub open_disk_cleanup: &'static str,
    pub copy_command: &'static str,
    pub command_copied: &'static str,
    pub show_items: &'static str,

    pub tab_changes: &'static str,
    pub changes_intro: &'static str,
    pub first_snapshot: &'static str,
    pub compare_with: &'static str,
    pub used_space_change: &'static str,
    pub no_changes: &'static str,
    pub col_change: &'static str,
    pub col_then_now: &'static str,
}

const TR: Words = Words {
    tab_cleanup: "Temizlik",
    cleanup_intro: concat!(
        "Windows disklerini en sık dolduran yerler: kendini yeniden oluşturan önbellekler, geçici klasörler, ",
        "derleme çıktıları, eski kurulum dosyaları. Silinen her şey Geri Dönüşüm Kutusu'na gider."
    ),
    computing: "Öneriler hazırlanıyor…",
    nothing_found: "Temizlenecek bilinen bir yer bulunamadı.",
    recycle_selected: "Seçilenleri Geri Dönüşüm Kutusu'na taşı",

    menu_recycle: "Geri Dönüşüm Kutusu'na taşı",
    protected: "Windows'un veya kurulu programların parçası; buradan silinmez.",

    confirm_title: "Geri Dönüşüm Kutusu'na taşınsın mı?",
    confirm_detail: concat!(
        "Kalıcı olarak silinmez; Geri Dönüşüm Kutusu'ndan geri yüklenebilir. Geri Dönüşüm Kutusu aynı diskte ",
        "durduğu için yer, kutuyu boşalttığınızda açılır. Kullanımdaki dosyalar olduğu yerde kalır. ",
        "Kutuya sığmayacak kadar büyük bir dosya varsa Windows önce sorar."
    ),
    confirm_action: "Taşı",
    cancel: "Vazgeç",
    recycling: "Taşınıyor…",

    keep_one: "Her grupta birini bırak, gerisini seç",
    clear_selection: "Seçimi temizle",

    open_recycle_bin: "Geri Dönüşüm Kutusu'nu aç",
    open_disk_cleanup: "Disk Temizleme'yi aç",
    copy_command: "Komutu kopyala",
    command_copied: "Komut kopyalandı. Yönetici olarak açılmış bir komut isteminde çalıştırın.",
    show_items: "Neler var?",

    tab_changes: "Değişenler",
    changes_intro: concat!(
        "Her taramadan sonra klasör boyutları kaydedilir. Eski bir kayıtla bugünü karşılaştırıp neyin büyüdüğünü ",
        "görün. Liste, büyümenin gerçekten olduğu klasöre iner: C:\\Users değil, içinde şişen klasör."
    ),
    first_snapshot: "Bu disk için ilk kayıt şimdi alındı. Bir sonraki taramada neyin büyüdüğünü burada göreceksiniz.",
    compare_with: "Karşılaştır:",
    used_space_change: "kullanılan alan",
    no_changes: "10 MB'tan büyük bir değişiklik yok.",
    col_change: "Değişim",
    col_then_now: "Önce → şimdi",
};

const EN: Words = Words {
    tab_cleanup: "Clean up",
    cleanup_intro: concat!(
        "The places that fill Windows disks most often: caches that rebuild themselves, temporary folders, ",
        "build output, old installers. Everything removed goes to the Recycle Bin."
    ),
    computing: "Preparing suggestions…",
    nothing_found: "No known place to clean up was found.",
    recycle_selected: "Move selected to the Recycle Bin",

    menu_recycle: "Move to the Recycle Bin",
    protected: "Part of Windows or an installed program; not deleted from here.",

    confirm_title: "Move to the Recycle Bin?",
    confirm_detail: concat!(
        "Nothing is deleted permanently; it can be restored from the Recycle Bin. The Recycle Bin lives on ",
        "the same disk, so the space comes back when you empty it. Files in use stay where they are. ",
        "If a file is too large for the Recycle Bin, Windows asks first."
    ),
    confirm_action: "Move",
    cancel: "Cancel",
    recycling: "Moving…",

    keep_one: "Keep one in each group, select the rest",
    clear_selection: "Clear selection",

    open_recycle_bin: "Open the Recycle Bin",
    open_disk_cleanup: "Open Disk Cleanup",
    copy_command: "Copy the command",
    command_copied: "Command copied. Run it in a command prompt opened as administrator.",
    show_items: "What's in it?",

    tab_changes: "Changes",
    changes_intro: concat!(
        "Folder sizes are recorded after every scan. Compare an earlier record with today to see what grew. ",
        "The list goes down to where the growth actually happened: not C:\\Users, but the folder inside it that swelled."
    ),
    first_snapshot: "The first record for this disk has just been taken. Next time you scan, what grew will show here.",
    compare_with: "Compare with:",
    used_space_change: "used space",
    no_changes: "No change larger than 10 MB.",
    col_change: "Change",
    col_then_now: "Then → now",
};

impl Lang {
    pub fn words(self) -> &'static Words {
        match self {
            Lang::Tr => &TR,
            Lang::En => &EN,
        }
    }

    pub fn safety(self, safety: Safety) -> (&'static str, &'static str) {
        match (self, safety) {
            (Lang::Tr, Safety::Safe) => (
                "Güvenli",
                "Kendiliğinden yeniden oluşur; hiçbir şey kaybolmaz.",
            ),
            (Lang::Tr, Safety::Likely) => (
                "Büyük olasılıkla güvenli",
                "Yeniden derleyerek veya kurarak geri gelir; zaman kaybettirir, veri değil.",
            ),
            (Lang::Tr, Safety::Careful) => (
                "Dikkat",
                "Sizin dosyalarınız; gidebilir mi, yalnızca siz bilirsiniz.",
            ),
            (Lang::En, Safety::Safe) => ("Safe", "Rebuilt automatically; nothing is lost."),
            (Lang::En, Safety::Likely) => (
                "Probably safe",
                "Comes back by rebuilding or reinstalling; costs time, not data.",
            ),
            (Lang::En, Safety::Careful) => (
                "Careful",
                "Your own files; only you can say whether they can go.",
            ),
        }
    }

    /// `(title, explanation)` for a cleanup rule.
    pub fn rule(self, id: &str) -> (&'static str, &'static str) {
        match (self, id) {
            (Lang::Tr, "user_temp") => ("Geçici dosyalar", "Programların işi bitince bırakıp gittiği dosyalar (%TEMP%)."),
            (Lang::Tr, "windows_temp") => ("Windows geçici dosyaları", "Windows'un ve kurulum programlarının geçici klasörü."),
            (Lang::Tr, "update_cache") => (
                "Windows Update indirmeleri",
                "Kurulmuş güncellemelerin indirme kopyaları. Gerekirse Windows yeniden indirir.",
            ),
            (Lang::Tr, "browser_cache") => ("Tarayıcı önbellekleri", "Chrome, Edge, Brave ve Firefox'un sayfa önbellekleri. Oturumlar ve şifreler etkilenmez."),
            (Lang::Tr, "shader_cache") => ("Gölgelendirici önbellekleri", "NVIDIA, AMD, DirectX ve Steam'in derlenmiş gölgelendiricileri. Oyunun ilk açılışı biraz yavaşlayabilir."),
            (Lang::Tr, "app_cache") => ("Uygulama önbellekleri", "VS Code, Discord, Slack ve Teams'in önbellekleri."),
            (Lang::Tr, "crash_dumps") => ("Çökme dökümleri", "Program ve sistem çökmelerinden kalan hata raporları."),
            (Lang::Tr, "dev_cache") => ("Paket yöneticisi önbellekleri", "npm, pip, yarn ve Gradle'ın indirme önbellekleri. Sonraki kurulum yeniden indirir."),
            (Lang::Tr, "node_modules") => ("node_modules klasörleri", "JavaScript projelerinin bağımlılıkları. Projede `npm install` ile geri gelir."),
            (Lang::Tr, "build_output") => ("Rust derleme çıktıları", "Cargo.toml'un yanındaki target klasörleri. `cargo build` ile yeniden oluşur."),
            (Lang::Tr, "python_cache") => ("Python önbellekleri", "__pycache__ klasörleri. Python bunları kendisi yeniden oluşturur."),
            (Lang::Tr, "recycle_bin") => ("Geri Dönüşüm Kutusu", "Silinmiş ama hâlâ yer kaplayan dosyalar. Kalıcı silme gerektirdiği için boşaltmayı size bırakıyoruz."),
            (Lang::Tr, "old_installers") => ("Eski kurulum dosyaları", "İndirilenler'de 3 aydır dokunulmamış .exe, .msi, .iso ve arşivler."),
            (Lang::Tr, "backups") => ("Yedek klasörleri", "Kullanıcı klasörlerinizdeki backup/backups klasörleri. İçlerine bakmadan silmeyin."),
            (Lang::Tr, "old_big_files") => ("Eski büyük dosyalar", "Kullanıcı klasörlerinizde 1 yıldır değişmemiş, 500 MB'tan büyük dosyalar."),
            (Lang::Tr, "hibernation") => (
                "Hazırda bekletme dosyası",
                "hiberfil.sys silinmez, kapatılır. Hazırda bekletmeyi kullanmıyorsanız `powercfg /h off` bu alanı geri verir.",
            ),
            (Lang::Tr, "windows_old") => ("Önceki Windows kurulumu", "Windows.old en doğru şekilde Disk Temizleme ile kaldırılır."),
            (Lang::En, "user_temp") => ("Temporary files", "Files programs leave behind when they are done (%TEMP%)."),
            (Lang::En, "windows_temp") => ("Windows temporary files", "The temporary folder of Windows and installers."),
            (Lang::En, "update_cache") => (
                "Windows Update downloads",
                "Download copies of updates already installed. Windows fetches them again if needed.",
            ),
            (Lang::En, "browser_cache") => ("Browser caches", "Page caches of Chrome, Edge, Brave and Firefox. Sign-ins and passwords are not touched."),
            (Lang::En, "shader_cache") => ("Shader caches", "Compiled shaders of NVIDIA, AMD, DirectX and Steam. A game's first start may be a little slower."),
            (Lang::En, "app_cache") => ("App caches", "Caches of VS Code, Discord, Slack and Teams."),
            (Lang::En, "crash_dumps") => ("Crash dumps", "Error reports left by program and system crashes."),
            (Lang::En, "dev_cache") => ("Package manager caches", "Download caches of npm, pip, yarn and Gradle. The next install downloads again."),
            (Lang::En, "node_modules") => ("node_modules folders", "Dependencies of JavaScript projects. `npm install` in the project brings them back."),
            (Lang::En, "build_output") => ("Rust build output", "target folders next to a Cargo.toml. `cargo build` recreates them."),
            (Lang::En, "python_cache") => ("Python caches", "__pycache__ folders. Python recreates them itself."),
            (Lang::En, "recycle_bin") => ("Recycle Bin", "Deleted files still taking space. Emptying it is permanent, so that is left to you."),
            (Lang::En, "old_installers") => ("Old installers", ".exe, .msi, .iso and archives in Downloads untouched for 3 months."),
            (Lang::En, "backups") => ("Backup folders", "backup/backups folders in your user folders. Look inside before removing."),
            (Lang::En, "old_big_files") => ("Old large files", "Files over 500 MB in your user folders unchanged for a year."),
            (Lang::En, "hibernation") => (
                "Hibernation file",
                "hiberfil.sys is not deleted, it is turned off. If you do not use hibernation, `powercfg /h off` gives the space back.",
            ),
            (Lang::En, "windows_old") => ("Previous Windows installation", "Windows.old is best removed with Disk Cleanup."),
            _ => ("?", ""),
        }
    }

    pub fn recycle_count(self, items: usize, bytes: &str) -> String {
        let items = self.number(items as u64);
        match self {
            Lang::Tr => format!("{items} öğe · {bytes}"),
            Lang::En => format!("{items} items · {bytes}"),
        }
    }

    /// After a recycle: what went, and that the space follows when the bin
    /// is emptied — moving to the recycle bin alone frees nothing.
    pub fn recycled(self, gone: usize, requested: usize, bytes: &str) -> String {
        let left = requested.saturating_sub(gone);
        let gone = self.number(gone as u64);
        match (self, left) {
            (Lang::Tr, 0) => format!(
                "{gone} öğe Geri Dönüşüm Kutusu'na taşındı. Kutuyu boşaltınca {bytes} yer açılır."
            ),
            (Lang::En, 0) => {
                format!("{gone} items moved to the Recycle Bin. Empty it to get {bytes} back.")
            }
            (Lang::Tr, _) => format!(
                "{gone} öğe taşındı, {left} öğe kullanımda olduğu için kaldı. Kutuyu boşaltınca yer açılır."
            ),
            (Lang::En, _) => format!(
                "{gone} items moved, {left} in use and left in place. Empty the Recycle Bin to get the space back."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_tree::cleanup::RULES;

    #[test]
    fn every_rule_has_words_in_both_languages() {
        for lang in [Lang::Tr, Lang::En] {
            for rule in RULES {
                let (title, note) = lang.rule(rule.id);
                assert_ne!(title, "?", "{lang:?} has no title for {}", rule.id);
                assert!(!note.is_empty());
            }
        }
    }

    #[test]
    fn the_result_line_says_what_stayed() {
        assert!(Lang::En.recycled(3, 3, "1 GB").contains("1 GB"));
        assert!(Lang::Tr.recycled(2, 5, "1 GB").contains('3'));
    }
}
