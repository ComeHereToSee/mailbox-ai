//! Prompt templates. The output language follows `AiSettings.language` (rewrites keep the
//! language of the text); prompts that deal with dates carry today's date, weekday and
//! time zone. Every prompt asks for the result only, without preambles.

use super::model::{AiLoop, AiSettings};
use super::{MailText, fmt_time, now_context};

pub const BASE: &str = "你是 Mailbox AI，内置在邮件客户端里的写作与阅读助手。直接给出结果本身：不要寒暄，不要解释你做了什么，不要写「以下是……」「好的」之类的开场白或结束语。";

pub const ASSISTANT: &str = "你是 Mailbox AI，内置在邮件客户端里的助手，帮助用户阅读、整理和撰写邮件。回答简洁、准确，不知道就直说。";

const JSON_ONLY: &str = "只输出一个 JSON 对象，不要输出任何其他文字，不要使用 ``` 代码块。";

pub fn lang_name(code: &str) -> String {
    let name = match code.trim() {
        "zh-CN" | "zh" | "zh-Hans" | "zh_CN" => "简体中文",
        "zh-TW" | "zh-HK" | "zh-Hant" => "繁體中文",
        "en" | "en-US" | "en-GB" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        "fr" => "Français",
        "de" => "Deutsch",
        "es" => "Español",
        "ru" => "Русский",
        "pt" => "Português",
        "it" => "Italiano",
        other => return other.to_string(),
    };
    name.to_string()
}

fn is_english(s: &AiSettings) -> bool {
    s.language.starts_with("en")
}

/// Output language for reading features (summaries, analysis, answers).
pub fn output_language(s: &AiSettings) -> String {
    match s.language.trim() {
        "" | "auto" => "使用与邮件原文相同的语言输出。".to_string(),
        "zh-CN" | "zh-Hans" | "zh" => "请用简体中文输出。".to_string(),
        "en" | "en-US" => "Write the output in English.".to_string(),
        other => format!("Write the output in {}.", lang_name(other)),
    }
}

/// Output language for emails written for the user (replies follow the incoming mail on "auto").
fn writing_language(s: &AiSettings, reply: bool) -> String {
    match s.language.trim() {
        "" | "auto" if reply => "使用与对方来信相同的语言。".to_string(),
        "" | "auto" => "使用与用户的要求相同的语言。".to_string(),
        "zh-CN" | "zh-Hans" | "zh" => "用简体中文写。".to_string(),
        "en" | "en-US" => "Write in English.".to_string(),
        other => format!("Write in {}.", lang_name(other)),
    }
}

pub fn tone_text(tone: &str) -> &'static str {
    match tone {
        "friendly" => "友好、亲切",
        "concise" => "简洁、直接",
        _ => "专业、得体",
    }
}

fn signature(s: &AiSettings) -> String {
    let name = s.signature_name.trim();
    if name.is_empty() { "不要写落款署名（邮件里已有签名）".to_string() } else { format!("落款署名用「{name}」") }
}

/// Headers and body of one message.
pub fn mail_block(m: &MailText) -> String {
    format!("发件人：{}\n时间：{}\n主题：{}\n\n{}", m.from, fmt_time(m.date), m.subject, m.text)
}

// ---- writing ----------------------------------------------------------------------------

/// Instruction of a rewrite op (`polish` … `translate`, `custom`).
fn rewrite_instruction(op: &str, instruction: &str, target_lang: &str) -> Option<String> {
    Some(match op {
        "polish" => "润色下面的邮件正文：让表达更通顺、自然、得体，修正语病和不当用词。".to_string(),
        "formal" => "把下面的邮件正文改写得更正式、更礼貌，适合商务场合。".to_string(),
        "friendly" => "把下面的邮件正文改写得更友好、更亲切，但仍然得体。".to_string(),
        "concise" => "把下面的邮件正文改写得更简洁：删去冗余和客套，保留全部关键信息。".to_string(),
        "expand" => "把下面的邮件正文扩写得更完整、更有条理：可以补充自然的过渡和礼貌用语，但不要编造新的事实。".to_string(),
        "proofread" => "校对下面的邮件正文：只修正错别字、语法、标点和用词错误，不要改变语气、结构和内容。".to_string(),
        "translate" => format!("把下面的邮件正文翻译成{}，保持原文的语气和格式。", lang_name(target_lang)),
        "custom" => format!("按照这个要求修改下面的邮件正文：{instruction}"),
        _ => return None,
    })
}

/// (system, user) for `/compose` rewrite ops. None for an unknown op.
pub fn rewrite(op: &str, text: &str, instruction: &str, target_lang: &str) -> Option<(String, String)> {
    let what = rewrite_instruction(op, instruction, target_lang)?;
    let lang_rule = if op == "translate" { "人名、产品名、链接、邮箱地址保持原样。" } else { "使用与原文相同的语言。" };
    let system = format!("{BASE}\n你是用户的邮件写作助手。");
    let user = format!(
        "{what}\n\n要求：\n- 保留原意和所有事实、数字、日期、人名、链接。\n- {lang_rule}\n- 只输出改写后的正文，不加解释，不加「以下是……」，不要用 Markdown 标题，段落之间空一行。\n\n原文：\n\"\"\"\n{text}\n\"\"\""
    );
    Some((system, user))
}

/// (system, user) for `/compose` op `write`.
pub fn write(s: &AiSettings, instruction: &str, draft: &str) -> (String, String) {
    let system = format!("{BASE}\n你是用户的邮件写作助手，替用户写邮件。{}", now_context());
    let mut user = format!(
        "请根据下面的要求写一封邮件的正文。\n要求：{instruction}\n语气：{}\n{}\n{}",
        tone_text(&s.tone),
        signature(s),
        writing_language(s, false)
    );
    if !draft.trim().is_empty() {
        user.push_str(&format!("\n\n用户已经写好的草稿（可以参考和沿用）：\n\"\"\"\n{draft}\n\"\"\""));
    }
    user.push_str(
        "\n\n规则：\n- 只输出邮件正文，以合适的称呼开头，段落之间空一行，不要用 Markdown 标题或加粗。\n- 不要编造用户没有提供的事实、数字、日期或承诺；需要用户补充的信息用「[待补充]」标出。\n- 如果要求里明确包含邮件主题，可以在最后单独一行写「主题：……」；否则不要写主题。",
    );
    (system, user)
}

/// (system, user) for a reply to the last message of `conversation` (`/compose` op `reply`,
/// auto replies, batch draft replies).
pub fn reply(s: &AiSettings, conversation: &str, instruction: &str) -> (String, String) {
    let system = format!("{BASE}\n你是用户（邮箱主人）的邮件助手，替用户写回复邮件。{}", now_context());
    let wish = if instruction.trim().is_empty() { "根据来信内容给出恰当的回复".to_string() } else { instruction.trim().to_string() };
    let user = format!(
        "下面是邮件会话（按时间顺序，最后一封是要回复的邮件）：\n\n{conversation}\n\n---\n请以我的身份写一封回复邮件的正文。\n我的要求：{wish}\n语气：{}\n{}\n{}\n\n规则：\n- 只输出回复正文，不要写主题行，不要引用或复述原邮件。\n- 以合适的称呼开头，段落之间空一行，不要用 Markdown 标题或加粗。\n- 不要编造原邮件和我的要求里都没有的事实、数字、日期或承诺；需要我补充的信息用「[待补充]」标出。",
        tone_text(&s.tone),
        signature(s),
        writing_language(s, true)
    );
    (system, user)
}

/// Suggested replies as JSON `{"suggestions":[{"label","text"}]}`.
pub fn replies(s: &AiSettings, conversation: &str) -> (String, String) {
    let system = format!("{BASE}\n你帮用户（邮箱主人）快速回复邮件。{JSON_ONLY}");
    let user = format!(
        "{conversation}\n\n---\n请为最后一封邮件给出 3 个不同方向的回复建议（例如同意、需要更多信息、婉拒或改期，按邮件内容选最合适的三种）。\n每条建议包含：\n- label：按钮上显示的短语，不超过 12 个字，例如「好的，周五前确认」。\n- text：完整的回复正文。语气{}，{}，以合适的称呼开头，不要写主题行，不要引用原邮件。{}\n\n输出格式：{{\"suggestions\":[{{\"label\":\"…\",\"text\":\"…\"}},{{\"label\":\"…\",\"text\":\"…\"}},{{\"label\":\"…\",\"text\":\"…\"}}]}}",
        tone_text(&s.tone),
        signature(s),
        writing_language(s, true)
    );
    (system, user)
}

// ---- reading ----------------------------------------------------------------------------

pub fn summary(s: &AiSettings, conversation: &str, thread: bool) -> (String, String) {
    let system = format!("{BASE}\n你负责为用户总结邮件。{}", output_language(s));
    let (lead, todo) = if is_english(s) { ("**In short**", "**To-do**") } else { ("**一句话**", "**待办**") };
    let what = if thread {
        "下面是一个邮件会话（按时间顺序）。请总结整个会话：讨论了什么、结论是什么、还有什么悬而未决。"
    } else {
        "请总结下面这封邮件。"
    };
    let user = format!(
        "{what}\n\n格式（Markdown）：\n1. 第一行写「{lead}：……」，用一句话说清核心内容。\n2. 然后用 2–5 条以「- 」开头的要点列出关键信息（人物、事实、数字、结论）。\n3. 如果有需要用户做的事或截止日期，最后加一个「{todo}」小节逐条列出（写明负责人和截止时间）。\n4. 不要复述签名、免责声明和引用的历史邮件，不要编造原文没有的信息。\n\n{conversation}"
    );
    (system, user)
}

pub fn translate(target_lang: &str, mail: &MailText) -> (String, String) {
    let lang = lang_name(target_lang);
    let system = format!("{BASE}\n你是专业的邮件翻译。");
    let user = format!(
        "把下面这封邮件的正文翻译成{lang}。\n要求：\n- 保持原文的语气、段落和列表结构，输出纯文本段落，不要用 Markdown 标题。\n- 人名、产品名、代码、链接、邮箱地址保持原样。\n- 只输出译文。\n\n主题：{}\n\n正文：\n{}",
        mail.subject, mail.text
    );
    (system, user)
}

/// Smart analysis of a new message (JSON).
pub fn insight(s: &AiSettings, mail: &MailText) -> (String, String) {
    let system = format!("{BASE}\n你负责分析新邮件，帮用户判断是否需要回复、有没有截止时间和待办事项。{}\n{JSON_ONLY}", now_context());
    let user = format!(
        "分析下面这封邮件（收件人是用户本人），输出一个 JSON 对象，字段如下：\n\
- needs_reply：布尔值。发件人是否在等用户本人回复或确认；通知、营销、系统自动发送的邮件为 false。\n\
- priority：\"high\"、\"normal\" 或 \"low\"。\n\
- deadline：用户需要在什么时间之前处理，格式 \"YYYY-MM-DD HH:MM\"（本地时间），没有就写 null。只说了日期没说时间的用 18:00；「周五前」指本周五 18:00（本周五已过则是下周五）；「明天」「下周一」等按上面给出的当前日期换算。\n\
- deadline_text：原文里关于截止时间的说法（如「周五前」），没有就写 null。\n\
- todos：字符串数组，用户需要做的事，每条一句话；没有就写 []。\n\
- one_line：不超过 40 个字的一句话摘要。\n\
todos 和 one_line：{}\n\n\
示例：{{\"needs_reply\":true,\"priority\":\"high\",\"deadline\":\"2026-10-23 18:00\",\"deadline_text\":\"周五前\",\"todos\":[\"确认负责的事项\"],\"one_line\":\"评审会纪要，需周五前确认分工\"}}\n\n\
邮件：\n{}",
        output_language(s),
        mail_block(mail)
    );
    (system, user)
}

/// Classification for batch `classify` (JSON).
pub fn classify(s: &AiSettings, mail: &MailText) -> (String, String) {
    let system = format!("{BASE}\n你负责给邮件分类并给出整理建议。{JSON_ONLY}");
    let user = format!(
        "为下面这封邮件给出整理建议，输出 JSON：{{\"label\":\"…\",\"category\":\"…\",\"action\":\"…\",\"reason\":\"…\"}}\n\
- label：一个简短的标签（2–6 个字，如「工作」「账单」「旅行」「通知」），没有合适的就写空字符串。\n\
- category：primary（个人和工作往来）、social（社交网络）、promotions（营销推广）、updates（通知、账单、订单）、forums（论坛、邮件列表）之一。\n\
- action：keep（留在收件箱）、archive（看过即可归档）、trash（明显无用的垃圾或营销）之一，拿不准时用 keep。\n\
- reason：一句话说明理由。\n\
label 和 reason：{}\n\n邮件：\n{}",
        output_language(s),
        mail_block(mail)
    );
    (system, user)
}

/// One message of a batch job (`summarize`, `todos`, `custom`).
pub fn batch_item(op: &str, instruction: &str, s: &AiSettings, mail: &MailText) -> (String, String) {
    let system = format!("{BASE}\n{}", output_language(s));
    let task = match op {
        "summarize" => "用一句话（不超过 50 个字）概括下面这封邮件的核心内容，只输出这句话。".to_string(),
        "todos" => "列出下面这封邮件里需要我（收件人）去做的事，每条以「- 」开头，有截止时间就写上。没有任何待办时只输出「无」。".to_string(),
        _ => format!("{instruction}\n\n只针对下面这封邮件输出结果。"),
    };
    (system, format!("{task}\n\n邮件：\n{}", mail_block(mail)))
}

/// Overall summary of a batch job.
pub fn batch_summary(s: &AiSettings, op: &str, instruction: &str, list: &str) -> (String, String) {
    let system = format!("{BASE}\n{}\n{}", output_language(s), now_context());
    let task = match op {
        "summarize" => "下面是多封邮件各自的一句话摘要。请写一份整体汇总（Markdown）：先用一两句话概括整体情况，再分组列出要点，并标出需要我处理的事项。".to_string(),
        "todos" => "下面是从多封邮件中提取的待办事项。请合并重复项，按截止时间和重要程度整理成一份待办清单（Markdown，用「- [ ] 」列表），每条注明来自哪封邮件（用方括号编号）。".to_string(),
        _ => format!("我对每封邮件执行了这个指令：「{instruction}」。下面是每封邮件的结果，请写一份整体汇总（Markdown）。"),
    };
    (system, format!("{task}\n\n{list}"))
}

/// A loop run over a list of messages.
pub fn loop_run(s: &AiSettings, l: &AiLoop, list: &str, count: usize) -> (String, String) {
    let system = format!("{BASE}\n你是用户的邮件助理，定期按用户的指令处理一批邮件。{}\n{}", now_context(), output_language(s));
    let user = format!(
        "任务：{}\n指令：{}\n\n下面是符合条件的 {count} 封邮件（[编号] 发件人｜主题｜时间｜账户，以及摘要）：\n\n{list}\n请按指令输出结果（Markdown）。提到具体邮件时写明发件人和主题；不要编造邮件里没有的信息。",
        l.name, l.instruction
    );
    (system, user)
}

/// Auto reply rule with an AI condition (JSON `{"match": bool}`).
pub fn rule_match(condition: &str, mail: &MailText) -> (String, String) {
    let system = format!("{BASE}\n你负责判断邮件是否符合用户设定的条件。{JSON_ONLY}");
    let user = format!(
        "判断下面这封邮件是否属于这一类：「{condition}」。\n输出 JSON：{{\"match\": true}} 或 {{\"match\": false}}\n\n邮件：\n{}",
        mail_block(mail)
    );
    (system, user)
}

/// Turns a question into search queries (JSON `{"queries": [...]}`).
pub fn ask_queries(question: &str) -> (String, String) {
    let system = format!("{BASE}\n你负责把用户关于自己邮箱的问题改写成本地邮件搜索的关键词。{}\n{JSON_ONLY}", now_context());
    let user = format!(
        "用户的问题：{question}\n\n给出 1–3 组搜索词，输出 JSON：{{\"queries\":[\"…\"]}}\n规则：\n\
- 每组是用空格分隔的 1–3 个关键词，用邮件里可能出现的原词（中文用词语，不要整句）。\n\
- 可以使用搜索语法：from:发件人、subject:主题词、after:YYYY-MM-DD、before:YYYY-MM-DD、newer_than:7d、has:attachment、is:unread。\n\
- 同一组里的词必须同时出现，所以每组不要放太多词；不同的说法放到不同的组里。"
    );
    (system, user)
}

/// Answer from the retrieved messages, citing them as [1] [2].
pub fn ask_answer(s: &AiSettings, question: &str, context: &str) -> (String, String) {
    let system = format!("{BASE}\n你根据用户自己的邮件回答问题。{}\n{}", now_context(), output_language(s));
    let user = format!(
        "问题：{question}\n\n下面是从用户邮箱中检索到的邮件，每封前面有编号：\n\n{context}\n回答要求：\n- 只根据这些邮件回答，引用时在句末用 [1]、[2] 这样的编号标注来源。\n- 邮件里找不到答案时，直接说不知道或没有找到，不要编造。\n- 简洁明了，可以使用 Markdown 列表。"
    );
    (system, user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_ops() {
        let mut s = AiSettings::default();
        assert!(output_language(&s).contains("简体中文"));
        s.language = "en".into();
        assert!(output_language(&s).contains("English"));
        s.language = "auto".into();
        assert!(writing_language(&s, true).contains("来信"));
        assert!(rewrite("unknown", "x", "", "").is_none());
        let (_, user) = rewrite("translate", "你好", "", "ja").unwrap();
        assert!(user.contains("日本語") && user.contains("你好"));
        let (system, _) = insight(&s, &MailText::default());
        assert!(system.contains("时区"));
    }
}
