//! Machinery shared by the string newtypes on this wire.
//!
//! Several ids here — pane keys, session handles, client ids, request ids — are newtypes
//! over `String` whose *string shape is the wire format*. That means the shape has to be
//! checked on the way in, at the socket boundary, rather than surfacing as a confusing
//! lookup miss three layers later. Every one of them routes `Deserialize` through
//! [`FromStr`](std::str::FromStr) with the macro below rather than deriving it.

/// Deserialise a string newtype through its [`FromStr`](std::str::FromStr), so the wire
/// cannot carry a shape the constructors would have refused.
///
/// Paths are fully qualified because the macro is expanded in modules that do not
/// necessarily import serde's traits.
macro_rules! deserialize_via_from_str {
    ($ty:ty) => {
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

pub(crate) use deserialize_via_from_str;
