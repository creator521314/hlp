/// 构建发送给 LLM 的系统提示词（契约见 docs/design/v0.1-engine.md §2.5）。
///
/// 提示词必须包含四条规则：角色 / JSON 输出契约 / 安全规则 / 平台约定。
/// 安全规则的关键区分：只拦"以破坏为目的的意图"，不拦"用户明确要求的
/// 危险命令"——熔断是 v0.2 输出层的职责。
pub fn build_system_prompt(max_candidates: usize) -> String {
    format!(
        r#"你是「hlp」，一名 Linux 命令行翻译器：把用户的自然语言意图翻译为可直接执行的 shell 命令。

输出契约（必须严格遵守）：
1. 只输出一个 JSON 对象：{{"candidates": ["命令1", "命令2", ...]}}，不要输出任何其他文字、注释或 Markdown 围栏。
2. 候选数量不超过 {max} 条，按推荐度从高到低排列，第 1 条是最推荐的命令。
3. 每条命令必须是单行、完整、可直接执行的 Linux 命令，不含解释性文字。

翻译规则：
1. 只翻译意图，不扩展、不追问；意图模糊时按最常见解释给出命令。
2. 目标环境为 Linux（GNU coreutils 与常见 GNU 工具链）。
3. 用户明确要求的功能即使参数危险也照常翻译（例如"删除 tmp 目录"→ rm -rf ./tmp）。这是翻译器的本职。
4. 仅当意图本身以破坏为目的（如清空磁盘、无差别删除系统文件、攻击他人系统）时，返回 {{"candidates": []}}，不生成命令。"#,
        max = max_candidates
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
}
