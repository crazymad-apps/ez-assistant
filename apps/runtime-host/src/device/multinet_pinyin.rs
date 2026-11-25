//! ESP-SR 中文 MultiNet7 (`mn7_cn`) 合法音节表与离线汉字转拼音工具。
//!
//! 音节表直接提取自固件使用的 `espressif__esp-sr/model/multinet_model/mn7_cn/vocab`（405 音节），
//! 保证桌面端保存前即可拦截 MultiNet7 无法解析的非法音节。

pub(super) const DEFAULT_WAKE_WORD: &str = "你好小元";
pub(super) const DEFAULT_WAKE_PINYIN: &str = "ni hao xiao yuan";

/// MultiNet7 (`mn7_cn`) 405 个已排序合法无声调拼音音节。
pub(super) const MULTINET7_CN_SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben",
    "beng", "bi", "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can",
    "cang", "cao", "ce", "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen",
    "cheng", "chi", "chong", "chou", "chu", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci",
    "cong", "cou", "cu", "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao",
    "de", "dei", "deng", "di", "dia", "dian", "diao", "die", "ding", "diu", "dong", "dou",
    "du", "duan", "dui", "dun", "duo", "e", "ei", "en", "eng", "er", "fa", "fan",
    "fang", "fei", "fen", "feng", "fo", "fou", "fu", "ga", "gai", "gan", "gang", "gao",
    "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan", "guang", "gui",
    "gun", "guo", "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng", "hong",
    "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo", "ji", "jia", "jian",
    "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun", "ka",
    "kai", "kan", "kang", "kao", "ke", "ken", "keng", "kong", "kou", "ku", "kua", "kuai",
    "kuan", "kuang", "kui", "kun", "kuo", "la", "lai", "lan", "lang", "lao", "le", "lei",
    "leng", "li", "lia", "lian", "liang", "liao", "lie", "lin", "ling", "liu", "long", "lou",
    "lu", "luan", "lun", "luo", "lv", "lve", "ma", "mai", "man", "mang", "mao", "me",
    "mei", "men", "meng", "mi", "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou",
    "mu", "n", "na", "nai", "nan", "nang", "nao", "ne", "nei", "nen", "neng", "ni",
    "nian", "niang", "niao", "nie", "nin", "ning", "niu", "nong", "nou", "nu", "nuan", "nuo",
    "nv", "nve", "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei", "pen", "peng",
    "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia", "qian",
    "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun", "ran",
    "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "ruan", "rui", "run",
    "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan",
    "shang", "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan",
    "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo",
    "ta", "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao", "tie", "ting",
    "tong", "tou", "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei",
    "wen", "weng", "wo", "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing",
    "xiong", "xiu", "xu", "xuan", "xue", "xun", "ya", "yan", "yang", "yao", "ye", "yi",
    "yin", "ying", "yo", "yong", "you", "yu", "yuan", "yue", "yun", "za", "zai", "zan",
    "zang", "zao", "ze", "zei", "zen", "zeng", "zha", "zhai", "zhan", "zhang", "zhao", "zhe",
    "zhen", "zheng", "zhi", "zhong", "zhou", "zhu", "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun",
    "zhuo", "zi", "zong", "zou", "zu", "zuan", "zui", "zun", "zuo",
];

/// 校验单个音节是否在 MultiNet7 中文模型词表内。
pub(super) fn is_valid_syllable(syllable: &str) -> bool {
    MULTINET7_CN_SYLLABLES.binary_search(&syllable).is_ok()
}

/// 规范化并校验 MultiNet7 唤醒词拼音（要求 2~8 个合法无声调小写音节，以单空格分隔）。
pub(super) fn normalize_and_validate_pinyin(raw: &str) -> Result<String, String> {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.len() < 2 || tokens.len() > 8 {
        return Err("唤醒词拼音需包含 2 到 8 个音节（例如 ni hao xiao yuan）。".to_owned());
    }
    for token in &tokens {
        if !is_valid_syllable(token.as_str()) {
            return Err(format!("音节 `{token}` 不在 MultiNet7 中文词表内，请修正拼音。"));
        }
    }
    Ok(tokens.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_multinet7_default_and_rejects_bad_syllables() {
        assert_eq!(
            normalize_and_validate_pinyin("  Ni   Hao Xiao Yuan ").expect("valid"),
            "ni hao xiao yuan"
        );
        assert!(normalize_and_validate_pinyin("lv cha").is_ok());
        assert!(normalize_and_validate_pinyin("ni").is_err());
        assert!(normalize_and_validate_pinyin("ni hao xyz").is_err());
    }
}
