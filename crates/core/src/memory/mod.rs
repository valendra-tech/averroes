//! Conversation indexing primitives kept independent from providers and UI.

mod global;

use crate::work::{WorkMessage, WorkMessageRole};
use sha2::{Digest, Sha256};

pub use global::{compile_global_memory_prompt, GlobalMemory, GlobalMemoryPrompt};

pub const MAX_FRAGMENT_CHARS: usize = 1_800;
/// Sentinel position used by the semantic index for the compact understood
/// context. It is never exposed as a normal transcript position.
pub const CONTEXT_FRAGMENT_MESSAGE_POSITION: usize = usize::MAX;
const FRAGMENT_OVERLAP_CHARS: usize = 180;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationFragment {
    pub message_position: usize,
    pub chunk_index: usize,
    pub text: String,
    pub content_hash: String,
}

/// Compile only user-visible text. Reasoning is intentionally not persisted
/// as memory: it is implementation detail and can be very large.
pub fn compile_fragments(messages: &[WorkMessage]) -> Vec<ConversationFragment> {
    let mut fragments = Vec::new();
    for (message_position, message) in messages.iter().enumerate() {
        let body = message.text.trim();
        if body.is_empty() {
            continue;
        }
        let role = match message.role {
            WorkMessageRole::User => "User",
            WorkMessageRole::Assistant => "Assistant",
            WorkMessageRole::Error => "Error",
        };
        let source = format!("{role}: {body}");
        for (chunk_index, text) in split_text(&source).into_iter().enumerate() {
            fragments.push(ConversationFragment {
                content_hash: content_hash(&text),
                message_position,
                chunk_index,
                text,
            });
        }
    }
    fragments
}

/// Compiles the visible transcript plus the latest compact understanding of
/// the conversation. Keeping the context as its own fragment lets semantic
/// retrieval find decisions and the current objective even after the original
/// messages have been compacted.
pub fn compile_fragments_with_context(
    messages: &[WorkMessage],
    context_summary: Option<&str>,
) -> Vec<ConversationFragment> {
    let mut fragments = compile_fragments(messages);
    let Some(context_summary) = context_summary
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return fragments;
    };
    let source = format!("Understood conversation context: {context_summary}");
    fragments.extend(
        split_text(&source)
            .into_iter()
            .enumerate()
            .map(|(chunk_index, text)| ConversationFragment {
                content_hash: content_hash(&text),
                message_position: CONTEXT_FRAGMENT_MESSAGE_POSITION,
                chunk_index,
                text,
            }),
    );
    fragments
}

fn split_text(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= MAX_FRAGMENT_CHARS {
        return vec![text.to_owned()];
    }
    let step = MAX_FRAGMENT_CHARS
        .saturating_sub(FRAGMENT_OVERLAP_CHARS)
        .max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + MAX_FRAGMENT_CHARS).min(chars.len());
        let chunk = chars[start..end].iter().collect::<String>();
        if !chunk.trim().is_empty() {
            chunks.push(chunk);
        }
        if end == chars.len() {
            break;
        }
        start += step;
    }
    chunks
}

pub fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * std::mem::size_of::<f32>());
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub fn decode_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    let chunks = bytes.chunks_exact(std::mem::size_of::<f32>());
    if !chunks.remainder().is_empty() {
        return None;
    }
    Some(
        chunks
            .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("exact f32 chunk")))
            .collect(),
    )
}

pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    let (mut dot, mut left_norm, mut right_norm) = (0.0, 0.0, 0.0);
    for (&left, &right) in left.iter().zip(right) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_are_bounded_and_exclude_reasoning() {
        let fragments = compile_fragments(&[WorkMessage {
            role: WorkMessageRole::User,
            text: "a".repeat(2_000),
            reasoning: "private reasoning".into(),
        }]);
        assert!(fragments.len() > 1);
        assert!(fragments[0].text.starts_with("User:"));
        assert!(fragments.iter().all(|fragment| {
            fragment.text.chars().count() <= MAX_FRAGMENT_CHARS
                && !fragment.text.contains("private reasoning")
        }));
    }

    #[test]
    fn understood_context_is_indexed_as_a_bounded_fragment() {
        let fragments = compile_fragments_with_context(
            &[],
            Some("Objective: ship the release.\nNext action: run the checks."),
        );
        assert_eq!(fragments.len(), 1);
        assert_eq!(
            fragments[0].message_position,
            CONTEXT_FRAGMENT_MESSAGE_POSITION
        );
        assert!(fragments[0]
            .text
            .contains("Understood conversation context"));
    }

    #[test]
    fn embeddings_round_trip() {
        let encoded = encode_embedding(&[0.25, -1.5, 3.0]);
        assert_eq!(decode_embedding(&encoded), Some(vec![0.25, -1.5, 3.0]));
    }

    #[test]
    fn cosine_similarity_is_one_for_the_same_vector() {
        assert!((cosine_similarity(&[1.0, 2.0], &[1.0, 2.0]) - 1.0).abs() < f32::EPSILON);
    }
}
