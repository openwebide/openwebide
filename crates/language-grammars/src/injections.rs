//! Injection selection is provider policy, independent of browser/workspace adapters.
#[cfg(feature = "html")]
mod html;
#[cfg(feature = "html")]
pub(crate) use html::html_injection;
#[cfg(feature = "markdown")]
mod markdown;
#[cfg(feature = "markdown")]
pub(crate) use markdown::{markdown_injection, markdown_language_injection};
