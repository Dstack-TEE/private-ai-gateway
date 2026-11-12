//! Typed operation selected while shaping a gateway request.

use crate::aggregator::service::{
    AUDIO_TRANSCRIPTIONS_PATH, CHAT_COMPLETIONS_PATH, COMPLETIONS_PATH, EMBEDDINGS_PATH,
    IMAGES_GENERATIONS_PATH, MESSAGES_PATH, RESPONSES_PATH, SYSTEMONE_PATH,
};

/// A request endpoint. Selects which per-format config table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    ChatComplete,
    Complete,
    Embed,
    Messages,
    CreateModelResponse,
    SystemOne,
    ImagesGenerations,
    AudioTranscriptions,
}

impl Endpoint {
    /// Resolve the HTTP boundary to the typed endpoint used inside the pipeline.
    pub(crate) fn from_path(path: &str) -> Self {
        match path {
            COMPLETIONS_PATH => Self::Complete,
            EMBEDDINGS_PATH => Self::Embed,
            MESSAGES_PATH => Self::Messages,
            RESPONSES_PATH => Self::CreateModelResponse,
            SYSTEMONE_PATH => Self::SystemOne,
            IMAGES_GENERATIONS_PATH => Self::ImagesGenerations,
            AUDIO_TRANSCRIPTIONS_PATH => Self::AudioTranscriptions,
            _ => Self::ChatComplete,
        }
    }

    /// The operation sent after shaping Chat/Messages into the route's format.
    pub(crate) fn wire(self, format: crate::middleware::types::ProviderFormat) -> Self {
        use crate::middleware::types::ProviderFormat;
        match (self, format) {
            (Self::ChatComplete, ProviderFormat::Anthropic) => Self::Messages,
            (Self::Messages, ProviderFormat::Openai) => Self::ChatComplete,
            _ => self,
        }
    }

    /// The path this endpoint is served on downstream and forwarded to upstream.
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::ChatComplete => CHAT_COMPLETIONS_PATH,
            Endpoint::Complete => COMPLETIONS_PATH,
            Endpoint::Embed => EMBEDDINGS_PATH,
            Endpoint::Messages => MESSAGES_PATH,
            Endpoint::CreateModelResponse => RESPONSES_PATH,
            Endpoint::SystemOne => SYSTEMONE_PATH,
            Endpoint::ImagesGenerations => IMAGES_GENERATIONS_PATH,
            Endpoint::AudioTranscriptions => AUDIO_TRANSCRIPTIONS_PATH,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Endpoint::ChatComplete => "chatComplete",
            Endpoint::Complete => "complete",
            Endpoint::Embed => "embed",
            Endpoint::Messages => "messages",
            Endpoint::CreateModelResponse => "createModelResponse",
            Endpoint::SystemOne => "systemOne",
            Endpoint::ImagesGenerations => "imagesGenerations",
            Endpoint::AudioTranscriptions => "audioTranscriptions",
        }
    }
}
