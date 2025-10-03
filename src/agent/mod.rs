//! Agent implementations and shared types used by Greentic flows.
//!
//! This module exposes concrete agent nodes (currently OpenAI and Ollama), the
//! schema they respond with, and helper utilities for serialising them in YAML
//! flows.

pub mod agent_reply_schema;
pub mod json_schema;
pub mod manager;
pub mod ollama;
pub mod openai;
