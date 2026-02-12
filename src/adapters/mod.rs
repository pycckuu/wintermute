//! Adapters — in-process async tasks for messaging platforms (spec 6.9).
//!
//! Each adapter maintains protocol connections, authenticates inbound
//! messages, extracts verified principal identity, and normalizes
//! messages into InboundEvent format.

pub mod cli;
pub mod telegram;

// Sub-modules will be added as implementation progresses:
// - slack:     Slack Socket Mode WebSocket (spec 6.9)
// - whatsapp:  WhatsApp Web via Baileys subprocess (spec 6.9)
// - webhook:   HTTPS POST with HMAC verification (spec 6.9)
