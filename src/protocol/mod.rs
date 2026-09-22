//! Typy wire-protokołów oraz translacja między nimi.
//!
//! UWAGA: te typy są używane WYŁĄCZNIE na ścieżce `/v1/messages` (Anthropic ↔ OpenAI).
//! `/v1/chat/completions` jest czystym passthrough na `serde_json::Value` i nie
//! przechodzi przez żaden z poniższych structów.

pub mod anthropic;
pub mod openai;
pub mod translate;
