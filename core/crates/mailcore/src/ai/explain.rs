//! Explaining attachments (`POST /v1/ai/messages/{id}/attachments/{aid}/explain`): images go
//! inline to a vision model, PDFs come with the text the app extracted (PDFKit), text
//! files are decoded here. The message's account policy and the encrypted-mail rule apply.

use super::model::{ChatImage, ChatMessage, ChatRequest, ModelRef};
use super::{DeltaTx, StreamDone, clean_output, client, prompts, truncate_chars};
use crate::Engine;
use crate::error::{Error, Result};
use crate::model::MessageId;
use base64::Engine as _;
use serde_json::json;

pub const VISION_NEEDED: &str = "需要支持看图的模型：请在 AI 功能中设置看图模型";
pub const UNSUPPORTED: &str = "暂不支持解读这种附件";
/// Images larger than this are not sent.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Pdf,
    Text,
    Other,
}

/// What an attachment is, from its MIME type and file name.
pub fn attachment_kind(filename: &str, mime: &str) -> AttachmentKind {
    let m = mime.trim().to_lowercase();
    let f = filename.trim().to_lowercase();
    let ext = |list: &[&str]| list.iter().any(|e| f.ends_with(e));
    if m.starts_with("image/") || ext(&[".png", ".jpg", ".jpeg", ".gif", ".webp"]) {
        AttachmentKind::Image
    } else if m == "application/pdf" || ext(&[".pdf"]) {
        AttachmentKind::Pdf
    } else if m.starts_with("text/") || m == "application/json" || ext(&[".txt", ".csv", ".md", ".markdown", ".json", ".log"]) {
        AttachmentKind::Text
    } else {
        AttachmentKind::Other
    }
}

/// Text of a file: UTF-8 (BOM dropped), otherwise GB18030.
pub fn decode_text(data: &[u8]) -> String {
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(data);
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::GB18030.decode_without_bom_handling(data).0.into_owned(),
    }
}

/// Everything resolved before the stream starts.
#[derive(Debug, Clone)]
pub struct ExplainPlan {
    pub model: ModelRef,
    pub system: String,
    pub user: String,
    pub images: Vec<ChatImage>,
    pub filename: String,
}

impl Engine {
    /// Whether the model is marked as able to see images.
    pub fn ai_model_has(&self, m: &ModelRef, capability: &str) -> bool {
        self.ai_provider(&m.provider)
            .is_some_and(|p| p.models.iter().any(|x| x.id == m.model && x.capabilities.iter().any(|c| c == capability)))
    }

    /// The model for images: `vision_model`, or `default_model` when it can see images.
    pub fn ai_vision_model(&self) -> Result<ModelRef> {
        let s = self.ai_settings();
        if let Some(m) = s.vision_model.clone().filter(|m| !m.provider.is_empty() && !m.model.is_empty()) {
            if let Some(p) = self.ai_provider(&m.provider) {
                if p.enabled && !p.kind.is_cli() && (p.has_key || !client::needs_key(&p)) {
                    return Ok(m);
                }
            }
        }
        if let Ok(m) = self.ai_model(false) {
            let cli = self.ai_provider(&m.provider).is_some_and(|p| p.kind.is_cli());
            if !cli && self.ai_model_has(&m, "vision") {
                return Ok(m);
            }
        }
        Err(Error::AiNotConfigured(VISION_NEEDED.into()))
    }

    pub async fn ai_explain_plan(&self, id: MessageId, aid: i64, text: Option<String>, question: Option<String>) -> Result<ExplainPlan> {
        let s = self.ai_settings();
        let (filename, mime, data) = self.attachment(id, aid).await?;
        let question = question.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
        let text = text.filter(|t| !t.trim().is_empty());
        let (model, content, images) = match attachment_kind(&filename, &mime) {
            AttachmentKind::Other => return Err(Error::Invalid(UNSUPPORTED.into())),
            AttachmentKind::Image => {
                if data.len() > MAX_IMAGE_BYTES {
                    return Err(Error::Invalid("图片超过 10 MB，无法交给 AI 解读".into()));
                }
                let media_type = if mime.to_lowercase().starts_with("image/") {
                    mime.to_lowercase()
                } else {
                    crate::mime::build::guess_mime(&filename).to_string()
                };
                let image = ChatImage { media_type, data: base64::engine::general_purpose::STANDARD.encode(&data) };
                (self.ai_vision_model()?, None, vec![image])
            }
            AttachmentKind::Pdf => {
                let t = text.ok_or_else(|| Error::Invalid("解读 PDF 需要先提取出其中的文字（请求体的 text）".into()))?;
                (self.ai_model(false)?, Some(truncate_chars(&t, s.max_body_chars)), Vec::new())
            }
            AttachmentKind::Text => {
                let t = decode_text(&data);
                if t.trim().is_empty() {
                    return Err(Error::Invalid("这个附件是空的".into()));
                }
                (self.ai_model(false)?, Some(truncate_chars(&t, s.max_body_chars)), Vec::new())
            }
        };
        let local = self.ai_model_is_local(&model);
        self.ai_check_message(id, local).await?;
        let (system, user) = prompts::explain(&s, &filename, content.as_deref(), question.as_deref());
        Ok(ExplainPlan { model, system, user, images, filename })
    }

    /// Streams the explanation; `done` carries `filename`.
    pub async fn ai_explain_run(&self, plan: ExplainPlan, tx: DeltaTx) -> Result<StreamDone> {
        let label = plan.model.label();
        let req = ChatRequest {
            model: plan.model,
            system: Some(plan.system),
            messages: vec![ChatMessage::user(plan.user)],
            feature: "explain".into(),
            images: plan.images,
            ..Default::default()
        };
        let raw = self.ai_chat(req, tx).await?;
        Ok(StreamDone::new(clean_output(&raw), label).with("filename", json!(plan.filename)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_decoding() {
        assert_eq!(attachment_kind("a.png", "image/png"), AttachmentKind::Image);
        assert_eq!(attachment_kind("scan.JPG", "application/octet-stream"), AttachmentKind::Image);
        assert_eq!(attachment_kind("会议纪要.pdf", "application/pdf"), AttachmentKind::Pdf);
        assert_eq!(attachment_kind("data.csv", "application/octet-stream"), AttachmentKind::Text);
        assert_eq!(attachment_kind("notes.md", ""), AttachmentKind::Text);
        assert_eq!(attachment_kind("a.json", "application/json"), AttachmentKind::Text);
        assert_eq!(attachment_kind("readme", "text/plain"), AttachmentKind::Text);
        assert_eq!(attachment_kind("a.zip", "application/zip"), AttachmentKind::Other);
        assert_eq!(decode_text("\u{feff}你好".as_bytes()), "你好");
        let (gbk, _, _) = encoding_rs::GB18030.encode("账单");
        assert_eq!(decode_text(&gbk), "账单");
    }
}
