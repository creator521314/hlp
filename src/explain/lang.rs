//! 解释语言判定（v0.4 设计 §2.6 / 决策 5）。
//!
//! 优先级：[explain] language 显式配置（zh/en）> auto 判定（输入含 CJK → Zh；
//! 否则 LC_ALL/LANG 以 zh 开头 → Zh；其余 → En）。

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }

    pub fn instruction(self) -> &'static str {
        match self {
            Lang::Zh => "请用简体中文解释，每个段的说明不超过 30 字。",
            Lang::En => "Explain in English, at most 30 words per segment.",
        }
    }
}

/// cfg_lang: "auto" | "zh" | "en"；env_lang 为 LC_ALL/LANG 的值。
pub fn detect(cfg_lang: &str, intent_or_command: &str, env_lang: Option<&str>) -> Lang {
    match cfg_lang.trim().to_ascii_lowercase().as_str() {
        "zh" => return Lang::Zh,
        "en" => return Lang::En,
        _ => {}
    }
    if intent_or_command
        .chars()
        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    {
        return Lang::Zh;
    }
    if env_lang
        .unwrap_or("")
        .to_ascii_lowercase()
        .starts_with("zh")
    {
        return Lang::Zh;
    }
    Lang::En
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-54：locale 回退 + 显式配置优先
    #[test]
    fn tu54_detect_lang_priority() {
        assert_eq!(detect("auto", "ls", Some("zh_CN.UTF-8")), Lang::Zh);
        assert_eq!(detect("auto", "列出大文件", Some("en_US.UTF-8")), Lang::Zh);
        assert_eq!(detect("auto", "ls", Some("en_US.UTF-8")), Lang::En);
        assert_eq!(detect("en", "列出", Some("zh_CN.UTF-8")), Lang::En);
        assert_eq!(detect("zh", "ls", None), Lang::Zh);
    }
}
