//! The fixed identities of a Store this prototype creates.
/// Catalog binding of a Store this prototype creates. A product SDK reads it
/// from its discovery record; the prototype fixes it.
pub const BINDING: &[u8] = b"layerfs-projfs-prototype";
/// History cursor key of a Store this prototype creates, fixed likewise.
pub const CURSOR_KEY: [u8; 32] = [0x5a; 32];
