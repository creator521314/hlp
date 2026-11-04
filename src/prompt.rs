/// 构建发送给 LLM 的系统提示词（契约见 docs/design/v0.1-engine.md §2.5）。
///
/// 提示词必须包含四条规则：角色 / JSON 输出契约 / 安全规则 / 平台约定。
/// 安全规则的关键区分：只拦"以破坏为目的的意图"，不拦"用户明确要求的
/// 危险命令"——熔断是 v0.2 输出层的职责。
pub fn build_system_prompt(max_candidates: usize) -> String {
    build_system_prompt_with(max_candidates, false)
}

/// v0.4：want_explanation=true 时扩展输出 schema（TUI 卡片逐段解释，决策 8）。
/// v0.1 的 `build_system_prompt` 保持逐字兼容（T-U-07 守门）。
pub fn build_system_prompt_with(max_candidates: usize, want_explanation: bool) -> String {
    let extension = if want_explanation {
        r#"
5. 同时输出逐段解释：JSON 中增加 "segments" 数组，按第 1 条候选命令的顶层管道
   顺序逐段给出 {"cmd": "该段原文", "note": "该段作用的一句话说明"}。
   没有管道时 segments 只有一个元素。"#
    } else {
        ""
    };
    format!(
        r#"你是「hlp」，一名 Linux 命令行翻译器：把用户的自然语言意图翻译为可直接执行的 shell 命令。

输出契约（必须严格遵守）：
1. 只输出一个 JSON 对象：{{"candidates": ["命令1", "命令2", ...]}}，不要输出任何其他文字、注释或 Markdown 围栏。
2. 候选数量不超过 {max} 条，按推荐度从高到低排列，第 1 条是最推荐的命令。
3. 每条命令必须是单行、完整、可直接执行的 Linux 命令，不含解释性文字。
4. 仅当意图本身以破坏为目的（如清空磁盘、无差别删除系统文件、攻击他人系统）时，返回 {{"candidates": []}}，不生成命令。{extension}

翻译规则：
1. 只翻译意图，不扩展、不追问；意图模糊时按最常见解释给出命令。
2. 目标环境为 Linux（GNU coreutils 与常见 GNU 工具链）。
3. 用户明确要求的功能即使参数危险也照常翻译（例如"删除 tmp 目录"→ rm -rf ./tmp）。这是翻译器的本职。"#,
        max = max_candidates
    )
}

/// `-x` / TUI 解释模式的系统提示词（决策 6：全部段 + kb 素材一次喂入）。
pub fn build_explain_system_prompt(
    segments: &[crate::explain::Segment],
    materials: &[String],
    lang: crate::explain::Lang,
) -> String {
    let skeleton = segments
        .iter()
        .enumerate()
        .map(|(i, s)| format!("[{}] {}", i + 1, s.cmd))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"你是 Linux 命令行解释器。把给出的命令按管道拆段，逐段用一句话说明该段的作用。

命令段骨架（必须按此顺序、此原文输出，不得增删或改写段）：
{skeleton}

本地知识库素材（仅供你参考，可纠正但不照抄）：
{materials}

输出契约：只输出一个 JSON 对象
{{"segments":[{{"cmd":"段原文","note":"该段说明"}}]}}
segments 的元素个数必须与上面的骨架段数一致，cmd 与骨架逐字相同。
{lang_rule}"#,
        materials = materials.join("\n"),
        lang_rule = lang.instruction(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T-U-07：四条规则全部写入提示词
    #[test]
    fn tu07_system_prompt_contains_all_rules() {
        let p = build_system_prompt(3);
        // ① JSON 输出契约
        assert!(p.contains("candidates"));
        assert!(p.contains("JSON"));
        assert!(p.contains("不超过 3 条"));
        // ② 候选数上限
        assert!(p.contains("第 1 条"));
        // ③ 安全规则：恶意意图返回空数组；明确要求的危险命令照常翻译
        assert!(p.contains("candidates\": []"));
        assert!(p.contains("rm -rf ./tmp"));
        // ④ Linux 平台约定
        assert!(p.contains("Linux"));
        assert!(p.contains("GNU coreutils"));
        // ⑤ 单行命令约束
        assert!(p.contains("单行"));
    }

    /// T-U-61：解释提示词含语言指令（zh）与各段 kb 素材
    #[test]
    fn tu61_explain_prompt_has_lang_and_material() {
        use crate::explain::{lang::Lang, Segment};
        let segs = vec![
            Segment { cmd: "find .".to_string(), note: String::new() },
            Segment { cmd: "sort -rh".to_string(), note: String::new() },
        ];
        let mats = vec!["[1] find .\n知识库页 find：查找文件".to_string()];
        let p = build_explain_system_prompt(&segs, &mats, Lang::Zh);
        assert!(p.contains("请用简体中文解释"), "{p}");
        assert!(p.contains("知识库页 find"), "{p}");
        assert!(p.contains("find ."), "{p}");
        assert!(p.contains("sort -rh"), "{p}");
        let en = build_explain_system_prompt(&segs, &mats, Lang::En);
        assert!(en.contains("Explain in English"));
    }

    /// want_explanation 扩展不破坏 v0.1 契约
    #[test]
    fn system_prompt_extension_keeps_v01_contract() {
        let base = build_system_prompt(3);
        let ext = build_system_prompt_with(3, true);
        for needle in ["candidates\": []", "rm -rf ./tmp", "GNU coreutils", "不超过 3 条"] {
            assert!(base.contains(needle) && ext.contains(needle), "{needle}");
        }
        assert!(ext.contains("segments"));
        assert!(!base.contains("segments"));
    }
}
