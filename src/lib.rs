//! Headless, LLM-free auto-continue for Claude sessions in Thurbox.
//!
//! `record` runs from Claude's `StopFailure` hook, proves a quota episode from
//! the transcript and schedules one `fire` for when the window has reset.
//! `fire` checks that nothing moved, looks at the screen, types the message,
//! verifies it and only then submits. `sweep` recovers an episode the hook
//! missed. None of them starts a model.

pub mod config;
pub mod engine;
pub mod episode;
pub mod lock;
pub mod log;
pub mod platform;
pub mod remote;
pub mod screen;
pub mod thurbox;
pub mod transcript;
