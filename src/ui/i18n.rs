use chrono::Local;
use chrono::TimeZone;
use once_cell::sync::Lazy;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

static EN_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/en.json"), "English"));
static CS_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/cs.json"), "Czech"));
static DE_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/de.json"), "German"));
static ES_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/es.json"), "Spanish"));
static FR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/fr.json"), "French"));
static JA_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ja.json"), "Japanese"));
static PL_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/pl.json"), "Polish"));
static PT_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/pt.json"), "Portuguese"));
static PT_BR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/pt-BR.json"), "Brazilian Portuguese"));
static RU_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ru.json"), "Russian"));
static UK_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/uk.json"), "Ukrainian"));
static ZH_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/zh.json"), "Chinese"));
static AR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ar.json"), "Arabic"));
static BN_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/bn.json"), "Bengali"));
static HI_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/hi.json"), "Hindi"));
static ID_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/id.json"), "Indonesian"));
static IT_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/it.json"), "Italian"));
static KO_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ko.json"), "Korean"));
static TH_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/th.json"), "Thai"));
static TR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/tr.json"), "Turkish"));
static VI_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/vi.json"), "Vietnamese"));
static UR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ur.json"), "Urdu"));
static FA_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/fa.json"), "Persian"));
static NL_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/nl.json"), "Dutch"));
static TL_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/tl.json"), "Tagalog"));
static HE_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/he.json"), "Hebrew"));
static SV_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/sv.json"), "Swedish"));
static NB_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/nb.json"), "Norwegian"));
static DA_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/da.json"), "Danish"));
static FI_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/fi.json"), "Finnish"));
static EL_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/el.json"), "Greek"));
static HU_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/hu.json"), "Hungarian"));
static RO_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/ro.json"), "Romanian"));
static BG_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/bg.json"), "Bulgarian"));
static SR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/sr.json"), "Serbian"));
static HR_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/hr.json"), "Croatian"));
static SL_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/sl.json"), "Slovenian"));
static SK_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/sk.json"), "Slovak"));
static LT_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/lt.json"), "Lithuanian"));
static LV_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/lv.json"), "Latvian"));
static ET_BUNDLE: Lazy<HashMap<String, String>> =
    Lazy::new(|| parse_bundle(include_str!("locales/et.json"), "Estonian"));

static CURRENT_LANGUAGE: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new("en".to_string()));

// ---------------------------------------------------------------------------
// Locale formatting configuration
// ---------------------------------------------------------------------------

/// Text direction for a locale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

/// Duration unit abbreviations per locale.
#[derive(Clone, Debug)]
pub struct DurationAbbrevs {
    pub days: &'static str,
    pub hours: &'static str,
    pub minutes: &'static str,
    pub seconds: &'static str,
    pub milliseconds: &'static str,
}

/// Per-locale formatting rules.
#[derive(Clone, Debug)]
pub struct LocaleFormat {
    pub decimal_separator: char,
    pub thousands_separator: char,
    pub date_format: &'static str,
    pub size_units: [&'static str; 5],
    pub duration: DurationAbbrevs,
    pub text_direction: TextDirection,
}

static EN_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "m",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static CS_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d. %m. %Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "m",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static DE_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "T",
        hours: "Std",
        minutes: "Min",
        seconds: "Sek",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static ES_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "m",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static FR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["o", "Ko", "Mo", "Go", "To"],
    duration: DurationAbbrevs {
        days: "j",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static JA_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%Y/%m/%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "日",
        hours: "時",
        minutes: "分",
        seconds: "秒",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static PL_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "godz",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static PT_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static PT_BR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static RU_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    duration: DurationAbbrevs {
        days: "д",
        hours: "ч",
        minutes: "мин",
        seconds: "с",
        milliseconds: "мс",
    },
    text_direction: TextDirection::Ltr,
};

static UK_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    duration: DurationAbbrevs {
        days: "д",
        hours: "год",
        minutes: "хв",
        seconds: "с",
        milliseconds: "мс",
    },
    text_direction: TextDirection::Ltr,
};

static ZH_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "天",
        hours: "时",
        minutes: "分",
        seconds: "秒",
        milliseconds: "毫秒",
    },
    text_direction: TextDirection::Ltr,
};

static AR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '\u{066B}',   // Arabic decimal separator ٫
    thousands_separator: '\u{066C}', // Arabic thousands separator ٬
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["ب", "ك.ب", "م.ب", "ج.ب", "ت.ب"],
    duration: DurationAbbrevs {
        days: "ي",
        hours: "س",
        minutes: "د",
        seconds: "ث",
        milliseconds: "مل.ث",
    },
    text_direction: TextDirection::Rtl,
};

static BN_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "দি",
        hours: "ঘ",
        minutes: "মি",
        seconds: "সে",
        milliseconds: "মি.সে",
    },
    text_direction: TextDirection::Ltr,
};

static HI_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "दि",
        hours: "घं",
        minutes: "मि",
        seconds: "से",
        milliseconds: "मि.से",
    },
    text_direction: TextDirection::Ltr,
};

static ID_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "h",
        hours: "j",
        minutes: "m",
        seconds: "d",
        milliseconds: "md",
    },
    text_direction: TextDirection::Ltr,
};

static IT_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "g",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static KO_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "일",
        hours: "시",
        minutes: "분",
        seconds: "초",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static TH_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "ว",
        hours: "ชม.",
        minutes: "น.",
        seconds: "วิ",
        milliseconds: "มล.วิ",
    },
    text_direction: TextDirection::Ltr,
};

static TR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "g",
        hours: "sa",
        minutes: "dk",
        seconds: "sn",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static VI_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "ng",
        hours: "g",
        minutes: "ph",
        seconds: "gi",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static UR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "د",
        hours: "گھ",
        minutes: "م",
        seconds: "س",
        milliseconds: "ملی.س",
    },
    text_direction: TextDirection::Rtl,
};

static FA_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '\u{066B}',   // Arabic decimal separator ٫
    thousands_separator: '\u{066C}', // Arabic thousands separator ٬
    date_format: "%Y/%m/%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "ر",
        hours: "سا",
        minutes: "دق",
        seconds: "ث",
        milliseconds: "میلی‌ث",
    },
    text_direction: TextDirection::Rtl,
};

static NL_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d-%m-%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "u",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static TL_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%m/%d/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "ar",
        hours: "or",
        minutes: "min",
        seconds: "seg",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static HE_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: '.',
    thousands_separator: ',',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "י",
        hours: "שע",
        minutes: "דק",
        seconds: "שנ",
        milliseconds: "מ\"ש",
    },
    text_direction: TextDirection::Rtl,
};

static SV_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static NB_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "t",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static DA_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "t",
        minutes: "min",
        seconds: "sek",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static FI_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GT", "TT"],
    duration: DurationAbbrevs {
        days: "pv",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static EL_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d/%m/%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "η",
        hours: "ω",
        minutes: "λ",
        seconds: "δ",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static HU_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%Y. %m. %d. %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "n",
        hours: "ó",
        minutes: "p",
        seconds: "mp",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static RO_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "z",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static BG_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    duration: DurationAbbrevs {
        days: "д",
        hours: "ч",
        minutes: "мин",
        seconds: "с",
        milliseconds: "мс",
    },
    text_direction: TextDirection::Ltr,
};

static SR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["Б", "КБ", "МБ", "ГБ", "ТБ"],
    duration: DurationAbbrevs {
        days: "д",
        hours: "ч",
        minutes: "мин",
        seconds: "с",
        milliseconds: "мс",
    },
    text_direction: TextDirection::Ltr,
};

static HR_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static SL_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '.',
    date_format: "%d. %m. %Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static SK_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d. %m. %Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static LT_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%Y-%m-%d %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "val",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static LV_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "d",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

static ET_FORMAT: LocaleFormat = LocaleFormat {
    decimal_separator: ',',
    thousands_separator: '\u{a0}', // non-breaking space
    date_format: "%d.%m.%Y %H:%M",
    size_units: ["B", "KB", "MB", "GB", "TB"],
    duration: DurationAbbrevs {
        days: "p",
        hours: "h",
        minutes: "min",
        seconds: "s",
        milliseconds: "ms",
    },
    text_direction: TextDirection::Ltr,
};

// ---------------------------------------------------------------------------
// Pluralization
// ---------------------------------------------------------------------------

/// CLDR plural categories supported by our locales.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluralCategory {
    One,
    Few,
    Other,
}

/// Returns the CLDR plural category for the given language and count.
pub fn plural_category(language: &str, count: u64) -> PluralCategory {
    match language {
        // Slavic: one / few (2-4) / other
        "bg" | "cs" | "hr" | "sk" | "sl" | "sr" | "uk" => match count {
            1 => PluralCategory::One,
            2..=4 => PluralCategory::Few,
            _ => PluralCategory::Other,
        },
        // Polish: one / few (2-4 but not 12-14, 112-114, etc.) / other
        "pl" => {
            if count == 1 {
                PluralCategory::One
            } else {
                let rem100 = count % 100;
                let rem10 = count % 10;
                if (2..=4).contains(&rem10) && !(12..=14).contains(&rem100) {
                    PluralCategory::Few
                } else {
                    PluralCategory::Other
                }
            }
        }
        // Russian: one (1, 21, 31…) / few (2-4, 22-24…) / other
        "ru" => {
            let rem100 = count % 100;
            let rem10 = count % 10;
            if rem10 == 1 && rem100 != 11 {
                PluralCategory::One
            } else if (2..=4).contains(&rem10) && !(12..=14).contains(&rem100) {
                PluralCategory::Few
            } else {
                PluralCategory::Other
            }
        }
        // Arabic: one (1) / few (3-10, 103-110, …) / other
        "ar" => {
            if count == 1 {
                PluralCategory::One
            } else {
                let rem100 = count % 100;
                if (3..=10).contains(&rem100) {
                    PluralCategory::Few
                } else {
                    PluralCategory::Other
                }
            }
        }
        // Indo-Aryan and Persian: one (0-1) / other
        "bn" | "fa" | "hi" | "ur" => {
            if count <= 1 {
                PluralCategory::One
            } else {
                PluralCategory::Other
            }
        }
        // Lithuanian: one (n%10=1 && n%100!=11) / few (n%10=2..9 && n%100!=12..19) / other
        "lt" => {
            let rem100 = count % 100;
            let rem10 = count % 10;
            if rem10 == 1 && rem100 != 11 {
                PluralCategory::One
            } else if (2..=9).contains(&rem10) && !(12..=19).contains(&rem100) {
                PluralCategory::Few
            } else {
                PluralCategory::Other
            }
        }
        // Latvian: one (n%10=1 && n%100!=11, or n=0) / other
        "lv" => {
            let rem10 = count % 10;
            let rem100 = count % 100;
            if rem10 == 1 && rem100 != 11 {
                PluralCategory::One
            } else {
                PluralCategory::Other
            }
        }
        // CJK, Thai, Indonesian, Vietnamese, Korean, Tagalog: no grammatical plural
        "id" | "ja" | "ko" | "tl" | "th" | "vi" | "zh" => PluralCategory::Other,
        // Romance and Germanic: one vs other
        _ => {
            if count == 1 {
                PluralCategory::One
            } else {
                PluralCategory::Other
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Collation
// ---------------------------------------------------------------------------

use icu_collator::options::CollatorOptions;
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use icu_locale_core::locale;

macro_rules! define_collator {
    ($name:ident, $loc:expr, $label:expr) => {
        static $name: Lazy<CollatorBorrowed<'static>> = Lazy::new(|| {
            CollatorBorrowed::try_new(
                CollatorPreferences::from(&locale!($loc)),
                CollatorOptions::default(),
            )
            .expect(concat!(
                "ICU collation data for ",
                $label,
                " must be available"
            ))
        });
    };
}

define_collator!(EN_COLLATOR, "en", "English");
define_collator!(CS_COLLATOR, "cs", "Czech");
define_collator!(DE_COLLATOR, "de", "German");
define_collator!(ES_COLLATOR, "es", "Spanish");
define_collator!(FR_COLLATOR, "fr", "French");
define_collator!(JA_COLLATOR, "ja", "Japanese");
define_collator!(PL_COLLATOR, "pl", "Polish");
define_collator!(PT_COLLATOR, "pt", "Portuguese");
define_collator!(RU_COLLATOR, "ru", "Russian");
define_collator!(UK_COLLATOR, "uk", "Ukrainian");
define_collator!(ZH_COLLATOR, "zh", "Chinese");
define_collator!(AR_COLLATOR, "ar", "Arabic");
define_collator!(BN_COLLATOR, "bn", "Bengali");
define_collator!(HI_COLLATOR, "hi", "Hindi");
define_collator!(ID_COLLATOR, "id", "Indonesian");
define_collator!(IT_COLLATOR, "it", "Italian");
define_collator!(KO_COLLATOR, "ko", "Korean");
define_collator!(TH_COLLATOR, "th", "Thai");
define_collator!(TR_COLLATOR, "tr", "Turkish");
define_collator!(VI_COLLATOR, "vi", "Vietnamese");
define_collator!(UR_COLLATOR, "ur", "Urdu");
define_collator!(FA_COLLATOR, "fa", "Persian");
define_collator!(NL_COLLATOR, "nl", "Dutch");
define_collator!(TL_COLLATOR, "fil", "Tagalog");
define_collator!(HE_COLLATOR, "he", "Hebrew");
define_collator!(SV_COLLATOR, "sv", "Swedish");
define_collator!(NB_COLLATOR, "nb", "Norwegian");
define_collator!(DA_COLLATOR, "da", "Danish");
define_collator!(FI_COLLATOR, "fi", "Finnish");
define_collator!(EL_COLLATOR, "el", "Greek");
define_collator!(HU_COLLATOR, "hu", "Hungarian");
define_collator!(RO_COLLATOR, "ro", "Romanian");
define_collator!(BG_COLLATOR, "bg", "Bulgarian");
define_collator!(SR_COLLATOR, "sr", "Serbian");
define_collator!(HR_COLLATOR, "hr", "Croatian");
define_collator!(SK_COLLATOR, "sk", "Slovak");
define_collator!(SL_COLLATOR, "sl", "Slovenian");
define_collator!(LT_COLLATOR, "lt", "Lithuanian");
define_collator!(LV_COLLATOR, "lv", "Latvian");
define_collator!(ET_COLLATOR, "et", "Estonian");

fn resolve_collator(language: &str) -> &'static CollatorBorrowed<'static> {
    match language {
        "ar" => &AR_COLLATOR,
        "bg" => &BG_COLLATOR,
        "bn" => &BN_COLLATOR,
        "cs" => &CS_COLLATOR,
        "de" => &DE_COLLATOR,
        "es" => &ES_COLLATOR,
        "fa" => &FA_COLLATOR,
        "fr" => &FR_COLLATOR,
        "da" => &DA_COLLATOR,
        "el" => &EL_COLLATOR,
        "et" => &ET_COLLATOR,
        "fi" => &FI_COLLATOR,
        "he" => &HE_COLLATOR,
        "hi" => &HI_COLLATOR,
        "hr" => &HR_COLLATOR,
        "hu" => &HU_COLLATOR,
        "id" => &ID_COLLATOR,
        "it" => &IT_COLLATOR,
        "ja" => &JA_COLLATOR,
        "ko" => &KO_COLLATOR,
        "lt" => &LT_COLLATOR,
        "lv" => &LV_COLLATOR,
        "nb" => &NB_COLLATOR,
        "nl" => &NL_COLLATOR,
        "pl" => &PL_COLLATOR,
        "pt" | "pt-BR" => &PT_COLLATOR,
        "ro" => &RO_COLLATOR,
        "ru" => &RU_COLLATOR,
        "sk" => &SK_COLLATOR,
        "sl" => &SL_COLLATOR,
        "sr" => &SR_COLLATOR,
        "th" => &TH_COLLATOR,
        "tl" => &TL_COLLATOR,
        "tr" => &TR_COLLATOR,
        "sv" => &SV_COLLATOR,
        "uk" => &UK_COLLATOR,
        "ur" => &UR_COLLATOR,
        "vi" => &VI_COLLATOR,
        "zh" => &ZH_COLLATOR,
        _ => &EN_COLLATOR,
    }
}

// ---------------------------------------------------------------------------
// I18n struct
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct I18n {
    language: String,
}

impl I18n {
    pub fn new(language: impl AsRef<str>) -> Self {
        let mut i18n = Self {
            language: "en".to_string(),
        };
        i18n.set_language(language.as_ref());
        i18n
    }

    pub fn set_language(&mut self, language: &str) {
        self.language = normalize_language(language);
        if let Ok(mut current) = CURRENT_LANGUAGE.write() {
            *current = self.language.clone();
        }
    }

    // -- translation --------------------------------------------------------

    pub fn tr(&self, key: &str) -> String {
        lookup_translation(&self.language, key)
    }

    pub fn tr_fmt(&self, key: &str, replacements: &[(&str, String)]) -> String {
        let mut text = self.tr(key);
        for (name, value) in replacements {
            let placeholder = format!("{{{}}}", name);
            text = text.replace(&placeholder, value);
        }
        text
    }

    /// Look up the plural-specific variant of `key` for `count`, then replace
    /// `{count}` in the result.
    pub fn tr_plural(&self, key: &str, count: u64) -> String {
        let category = plural_category(&self.language, count);
        let suffixed_key = plural_suffixed_key(key, category);
        let mut text = lookup_translation_or_base(&self.language, &suffixed_key, key);
        text = text.replace("{count}", &count.to_string());
        text
    }

    /// Like `tr_plural` but with additional named replacements.
    pub fn tr_plural_fmt(&self, key: &str, count: u64, replacements: &[(&str, String)]) -> String {
        let mut text = self.tr_plural(key, count);
        for (name, value) in replacements {
            let placeholder = format!("{{{}}}", name);
            text = text.replace(&placeholder, value);
        }
        text
    }
}

// ---------------------------------------------------------------------------
// RTL-aware layout helpers
// ---------------------------------------------------------------------------
//
// RTL: known limitation - egui (as of 0.33.x) does not provide built-in
// bidirectional text rendering. Mixed RTL/LTR text within a single label will
// not reorder correctly, and TextEdit does not support RTL cursor movement.
// Full RTL support would require upstream egui changes or a Bidi shaping
// layer. The helpers below cover *layout direction* (widget placement and
// positional anchoring) so that adding an RTL locale in the future only
// requires setting `TextDirection::Rtl` in `LocaleFormat`.

/// Returns true if the current global locale is RTL.
pub fn is_rtl() -> bool {
    let lang = current_language();
    resolve_locale_format(&lang).text_direction == TextDirection::Rtl
}

// ---------------------------------------------------------------------------
// Locale format resolution
// ---------------------------------------------------------------------------

fn resolve_locale_format(language: &str) -> &'static LocaleFormat {
    match language {
        "ar" => &AR_FORMAT,
        "bg" => &BG_FORMAT,
        "bn" => &BN_FORMAT,
        "cs" => &CS_FORMAT,
        "da" => &DA_FORMAT,
        "de" => &DE_FORMAT,
        "el" => &EL_FORMAT,
        "es" => &ES_FORMAT,
        "et" => &ET_FORMAT,
        "fa" => &FA_FORMAT,
        "fi" => &FI_FORMAT,
        "fr" => &FR_FORMAT,
        "he" => &HE_FORMAT,
        "hi" => &HI_FORMAT,
        "hr" => &HR_FORMAT,
        "hu" => &HU_FORMAT,
        "id" => &ID_FORMAT,
        "it" => &IT_FORMAT,
        "ja" => &JA_FORMAT,
        "ko" => &KO_FORMAT,
        "lt" => &LT_FORMAT,
        "lv" => &LV_FORMAT,
        "nb" => &NB_FORMAT,
        "nl" => &NL_FORMAT,
        "pl" => &PL_FORMAT,
        "pt" => &PT_FORMAT,
        "ro" => &RO_FORMAT,
        "pt-BR" => &PT_BR_FORMAT,
        "ru" => &RU_FORMAT,
        "sk" => &SK_FORMAT,
        "sl" => &SL_FORMAT,
        "sr" => &SR_FORMAT,
        "sv" => &SV_FORMAT,
        "th" => &TH_FORMAT,
        "tl" => &TL_FORMAT,
        "tr" => &TR_FORMAT,
        "uk" => &UK_FORMAT,
        "ur" => &UR_FORMAT,
        "vi" => &VI_FORMAT,
        "zh" => &ZH_FORMAT,
        _ => &EN_FORMAT,
    }
}

fn plural_suffixed_key(key: &str, category: PluralCategory) -> String {
    let suffix = match category {
        PluralCategory::One => ".one",
        PluralCategory::Few => ".few",
        PluralCategory::Other => ".other",
    };
    format!("{key}{suffix}")
}

// ---------------------------------------------------------------------------
// Formatting implementation
// ---------------------------------------------------------------------------

fn fmt_number_with_locale(value: f64, decimals: usize, lf: &LocaleFormat) -> String {
    let raw = format!("{:.prec$}", value, prec = decimals);
    // Replace the decimal point with the locale separator.
    let raw = raw.replace('.', &lf.decimal_separator.to_string());

    // Insert thousands separators into the integer part.
    if let Some(dot_pos) = raw.find(lf.decimal_separator) {
        let (int_part, frac_part) = raw.split_at(dot_pos);
        let grouped = insert_thousands(int_part, lf.thousands_separator);
        format!("{grouped}{frac_part}")
    } else {
        insert_thousands(&raw, lf.thousands_separator)
    }
}

fn insert_thousands(int_part: &str, sep: char) -> String {
    let negative = int_part.starts_with('-');
    let digits: &str = if negative { &int_part[1..] } else { int_part };
    if digits.len() <= 3 {
        return int_part.to_string();
    }
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(sep);
        }
        result.push(ch);
    }
    if negative {
        result.push('-');
    }
    result.chars().rev().collect()
}

fn fmt_bytes_with_locale(bytes: u64, lf: &LocaleFormat) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;

    let bytes_f64 = bytes as f64;
    let (value, decimals, unit_idx) = if bytes_f64 >= TB {
        (bytes_f64 / TB, 2, 4)
    } else if bytes_f64 >= GB {
        (bytes_f64 / GB, 2, 3)
    } else if bytes_f64 >= MB {
        (bytes_f64 / MB, 1, 2)
    } else if bytes_f64 >= KB {
        (bytes_f64 / KB, 1, 1)
    } else {
        return format!("{} {}", bytes, lf.size_units[0]);
    };
    let num = fmt_number_with_locale(value, decimals, lf);
    format!("{} {}", num, lf.size_units[unit_idx])
}

fn fmt_date_with_locale(unix_secs: u64, lf: &LocaleFormat) -> String {
    Local
        .timestamp_opt(unix_secs as i64, 0)
        .single()
        .map(|time| time.format(lf.date_format).to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_duration_with_locale(duration: Duration, with_ms: bool, lf: &LocaleFormat) -> String {
    let total_secs = duration.as_secs();
    let days = total_secs / 86_400;
    let hours = (total_secs % 86_400) / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    let d = &lf.duration;

    if with_ms && total_secs == 0 {
        let ms = duration.as_millis();
        return format!("{}{}", ms, d.milliseconds);
    }

    if days > 0 {
        format!(
            "{}{} {}{} {}{} {}{}",
            days, d.days, hours, d.hours, minutes, d.minutes, seconds, d.seconds
        )
    } else if hours > 0 {
        format!(
            "{}{} {}{} {}{}",
            hours, d.hours, minutes, d.minutes, seconds, d.seconds
        )
    } else if minutes > 0 {
        format!("{}{} {}{}", minutes, d.minutes, seconds, d.seconds)
    } else if with_ms {
        let ms = duration.subsec_millis();
        format!("{}{} {}{}", seconds, d.seconds, ms, d.milliseconds)
    } else {
        format!("{}{}", seconds, d.seconds)
    }
}

// ---------------------------------------------------------------------------
// Free functions (use CURRENT_LANGUAGE)
// ---------------------------------------------------------------------------

fn current_locale_format() -> &'static LocaleFormat {
    let lang = current_language();
    resolve_locale_format(&lang)
}

pub fn fmt_bytes(bytes: u64) -> String {
    fmt_bytes_with_locale(bytes, current_locale_format())
}

pub fn fmt_date(unix_secs: u64) -> String {
    fmt_date_with_locale(unix_secs, current_locale_format())
}

pub fn fmt_duration(duration: Duration) -> String {
    fmt_duration_with_locale(duration, false, current_locale_format())
}

pub fn fmt_duration_ms(duration: Duration) -> String {
    fmt_duration_with_locale(duration, true, current_locale_format())
}

pub fn fmt_speed_mbps(bytes_per_sec: f64) -> String {
    let lf = current_locale_format();
    let unit = tr("Mbps");
    if bytes_per_sec <= 0.0 {
        format!("0 {unit}")
    } else {
        let mbps = (bytes_per_sec * 8.0) / 1_000_000.0;
        let num = fmt_number_with_locale(mbps, 2, lf);
        format!("{num} {unit}")
    }
}

pub fn locale_compare(a: &str, b: &str) -> Ordering {
    let lang = current_language();
    resolve_collator(&lang).compare(a, b)
}
// ---------------------------------------------------------------------------
// Language resolution
// ---------------------------------------------------------------------------

/// All language codes that have a bundled locale file.
const SUPPORTED_LANGUAGES: &[&str] = &[
    "ar", "bg", "bn", "cs", "da", "de", "el", "en", "es", "et", "fa", "fi", "fr", "he", "hi", "hr",
    "hu", "id", "it", "ja", "ko", "lt", "lv", "nb", "nl", "pl", "pt", "pt-BR", "ro", "ru", "sk",
    "sl", "sr", "sv", "th", "tl", "tr", "uk", "ur", "vi", "zh",
];

pub fn normalize_language(language: &str) -> String {
    let normalized = language.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "system" => detect_system_language(),
        "pt-br" => "pt-BR".to_string(),
        code if SUPPORTED_LANGUAGES.contains(&code) => code.to_string(),
        "" => "en".to_string(),
        // Basic fallback support: unknown locales use English until a bundle exists.
        _ => "en".to_string(),
    }
}

pub fn sanitize_locale_preference(locale: &str) -> String {
    let normalized = locale.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" => "system".to_string(),
        "system" => "system".to_string(),
        "pt-br" => "pt-BR".to_string(),
        code if SUPPORTED_LANGUAGES.contains(&code) => code.to_string(),
        _ => "en".to_string(),
    }
}

pub fn migrate_locale_preference(locale: &str, migrated: bool) -> (String, bool) {
    let sanitized = sanitize_locale_preference(locale);
    if migrated {
        return (sanitized, true);
    }

    let migrated_locale = match sanitized.as_str() {
        "en" => "system".to_string(),
        _ => sanitized,
    };
    (migrated_locale, true)
}

fn detect_system_language() -> String {
    let locale = sys_locale::get_locale().unwrap_or_default();
    let lower = locale.to_ascii_lowercase();
    // Check for pt-BR specifically before falling back to primary subtag.
    if lower.starts_with("pt-br") || lower.starts_with("pt_br") {
        return "pt-BR".to_string();
    }
    let primary = lower.split(['-', '_']).next().unwrap_or_default();
    match primary {
        "ar" | "bg" | "bn" | "cs" | "da" | "de" | "el" | "es" | "et" | "fa" | "fi" | "fr"
        | "he" | "hi" | "hr" | "hu" | "id" | "it" | "ja" | "ko" | "lt" | "lv" | "nb" | "nl"
        | "pl" | "pt" | "ro" | "ru" | "sk" | "sl" | "sr" | "sv" | "th" | "tl" | "tr" | "uk"
        | "ur" | "vi" | "zh" => primary.to_string(),
        // Filipino uses "fil" in some OS locale codes
        "fil" => "tl".to_string(),
        _ => "en".to_string(),
    }
}

fn current_language() -> String {
    CURRENT_LANGUAGE
        .read()
        .map(|lang| lang.clone())
        .unwrap_or_else(|_| "en".to_string())
}

pub fn tr(key: &str) -> String {
    let lang = current_language();
    lookup_translation(&lang, key)
}

pub fn tr_fmt(key: &str, replacements: &[(&str, String)]) -> String {
    let mut text = tr(key);
    for (name, value) in replacements {
        let placeholder = format!("{{{}}}", name);
        text = text.replace(&placeholder, value);
    }
    text
}

fn resolve_bundle(language: &str) -> &'static HashMap<String, String> {
    match language {
        "ar" => &AR_BUNDLE,
        "bg" => &BG_BUNDLE,
        "bn" => &BN_BUNDLE,
        "cs" => &CS_BUNDLE,
        "da" => &DA_BUNDLE,
        "de" => &DE_BUNDLE,
        "el" => &EL_BUNDLE,
        "es" => &ES_BUNDLE,
        "et" => &ET_BUNDLE,
        "fa" => &FA_BUNDLE,
        "fi" => &FI_BUNDLE,
        "fr" => &FR_BUNDLE,
        "he" => &HE_BUNDLE,
        "hi" => &HI_BUNDLE,
        "hr" => &HR_BUNDLE,
        "hu" => &HU_BUNDLE,
        "id" => &ID_BUNDLE,
        "it" => &IT_BUNDLE,
        "ja" => &JA_BUNDLE,
        "ko" => &KO_BUNDLE,
        "lt" => &LT_BUNDLE,
        "lv" => &LV_BUNDLE,
        "nb" => &NB_BUNDLE,
        "nl" => &NL_BUNDLE,
        "pl" => &PL_BUNDLE,
        "pt" => &PT_BUNDLE,
        "pt-BR" => &PT_BR_BUNDLE,
        "ro" => &RO_BUNDLE,
        "ru" => &RU_BUNDLE,
        "sk" => &SK_BUNDLE,
        "sl" => &SL_BUNDLE,
        "sr" => &SR_BUNDLE,
        "sv" => &SV_BUNDLE,
        "th" => &TH_BUNDLE,
        "tl" => &TL_BUNDLE,
        "tr" => &TR_BUNDLE,
        "uk" => &UK_BUNDLE,
        "ur" => &UR_BUNDLE,
        "vi" => &VI_BUNDLE,
        "zh" => &ZH_BUNDLE,
        _ => &EN_BUNDLE,
    }
}

fn lookup_translation(language: &str, key: &str) -> String {
    lookup_translation_from_bundles(resolve_bundle(language), &EN_BUNDLE, key)
}

fn lookup_translation_from_bundles(
    bundle: &HashMap<String, String>,
    fallback_bundle: &HashMap<String, String>,
    key: &str,
) -> String {
    bundle
        .get(key)
        .or_else(|| fallback_bundle.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

fn lookup_translation_or_base(language: &str, key: &str, base_key: &str) -> String {
    lookup_translation_or_base_from_bundles(resolve_bundle(language), &EN_BUNDLE, key, base_key)
}

fn lookup_translation_or_base_from_bundles(
    bundle: &HashMap<String, String>,
    fallback_bundle: &HashMap<String, String>,
    key: &str,
    base_key: &str,
) -> String {
    bundle
        .get(key)
        .or_else(|| bundle.get(base_key))
        .or_else(|| fallback_bundle.get(key))
        .or_else(|| fallback_bundle.get(base_key))
        .cloned()
        .unwrap_or_else(|| base_key.to_string())
}

fn parse_bundle(raw_json: &str, language_name: &str) -> HashMap<String, String> {
    serde_json::from_str(raw_json.trim_start_matches('\u{feff}'))
        .unwrap_or_else(|err| panic!("Failed to parse bundled {language_name} locale file: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_locale_key_falls_back_to_english_bundle() {
        let bundle = HashMap::new();
        let fallback_bundle =
            HashMap::from([("Only in English".to_string(), "Only in English".to_string())]);

        assert_eq!(
            lookup_translation_from_bundles(&bundle, &fallback_bundle, "Only in English"),
            "Only in English".to_string()
        );
    }

    #[test]
    fn missing_plural_locale_key_falls_back_to_english_base() {
        let bundle = HashMap::new();
        let fallback_bundle =
            HashMap::from([("Only in English".to_string(), "Only in English".to_string())]);

        assert_eq!(
            lookup_translation_or_base_from_bundles(
                &bundle,
                &fallback_bundle,
                "Only in English.other",
                "Only in English"
            ),
            "Only in English".to_string()
        );
    }
}
