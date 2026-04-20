//! Shared helpers for building multimodal content parts.
//!
//! Both the Responses API (`OxideClient`) and Chat Completions (`OxideChatClient`,
//! `OpenAIClient`) accept text + inline images, but the JSON shape differs:
//!
//! - Chat: `[{type:"text"}, {type:"image_url", image_url:{url, detail}}]`
//! - Responses: `[{type:"input_text"}, {type:"input_image", image_url, detail}]`
//!
//! These helpers use the typed enums from `openai-oxide` so every backend
//! produces the same wire shape without duplicating the JSON per file.

use crate::types::ImagePart;

use openai_oxide::types::chat::{ContentPart, ImageDetail as ChatImageDetail, ImageUrl};
use openai_oxide::types::responses::common::ImageDetail as ResponsesImageDetail;
use openai_oxide::types::responses::{InputContent, InputImageContent, InputTextContent};

/// Build a Chat Completions content-parts vector:
/// `[{type:"text", text}, {type:"image_url", image_url:{url, detail}}, ...]`.
///
/// Caller wraps this in `UserContent::Parts(...)` (typed) or serializes via
/// `serde_json::to_value(...)` (legacy JSON path).
pub fn chat_parts(text: &str, images: &[ImagePart]) -> Vec<ContentPart> {
    let mut parts: Vec<ContentPart> = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(ContentPart::Text { text: text.into() });
    }
    for img in images {
        parts.push(ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: img.data_url(),
                detail: Some(ChatImageDetail::Auto),
            },
        });
    }
    parts
}

/// Build a Responses API content-parts vector:
/// `[{type:"input_text", text}, {type:"input_image", image_url, detail}, ...]`.
///
/// Caller either assigns to `ResponseInputItem.content` via
/// `serde_json::to_value(...)` or injects into items-format requests the same
/// way.
pub fn responses_parts(text: &str, images: &[ImagePart]) -> Vec<InputContent> {
    let mut parts: Vec<InputContent> = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(InputContent::InputText(InputTextContent {
            text: text.into(),
        }));
    }
    for img in images {
        parts.push(InputContent::InputImage(InputImageContent {
            detail: ResponsesImageDetail::Auto,
            file_id: None,
            image_url: Some(img.data_url()),
        }));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_img() -> ImagePart {
        ImagePart {
            data: "AAAA".into(),
            mime_type: "image/jpeg".into(),
        }
    }

    #[test]
    fn chat_parts_contains_text_and_image() {
        let parts = chat_parts("hello", &[sample_img()]);
        let v = serde_json::to_value(&parts).unwrap();
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"type\":\"text\""), "missing text type: {s}");
        assert!(
            s.contains("\"type\":\"image_url\""),
            "missing image_url type: {s}"
        );
        assert!(
            s.contains("data:image/jpeg;base64,AAAA"),
            "missing data URL: {s}"
        );
    }

    #[test]
    fn responses_parts_contains_input_text_and_input_image() {
        let parts = responses_parts("hello", &[sample_img()]);
        let v = serde_json::to_value(&parts).unwrap();
        let s = serde_json::to_string(&v).unwrap();
        assert!(
            s.contains("\"type\":\"input_text\""),
            "missing input_text: {s}"
        );
        assert!(
            s.contains("\"type\":\"input_image\""),
            "missing input_image: {s}"
        );
        assert!(
            s.contains("data:image/jpeg;base64,AAAA"),
            "missing data URL: {s}"
        );
    }

    #[test]
    fn empty_text_omits_text_part() {
        let parts = chat_parts("", &[sample_img()]);
        assert_eq!(parts.len(), 1, "empty text ⇒ only image part");
        let rparts = responses_parts("", &[sample_img()]);
        assert_eq!(rparts.len(), 1);
    }
}
