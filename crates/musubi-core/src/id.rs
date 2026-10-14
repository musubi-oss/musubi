use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}:{}", $prefix, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let s = s.strip_prefix(concat!($prefix, ":")).unwrap_or(s);
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

typed_id!(AccountId, "acc");
typed_id!(IdentityId, "idn");
typed_id!(SessionId, "ses");
typed_id!(NodeId, "node");
typed_id!(MatchId, "match");
typed_id!(PartyId, "party");
typed_id!(GroupId, "grp");
typed_id!(ChannelId, "chan");
typed_id!(ObjectId, "obj");

/// Address of an actor inside the cluster: a kind discriminator plus a 128-bit key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
pub struct ActorId {
    pub kind: u16,
    pub key: u128,
}

impl ActorId {
    pub fn new(kind: u16, key: u128) -> Self {
        Self { kind, key }
    }

    pub fn from_uuid(kind: u16, id: Uuid) -> Self {
        Self {
            kind,
            key: id.as_u128(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_and_without_prefix() {
        let id = AccountId::new();
        let plain = id.to_string();
        let prefixed = format!("{id:?}");
        assert_eq!(plain.parse::<AccountId>().unwrap(), id);
        assert_eq!(prefixed.parse::<AccountId>().unwrap(), id);
    }

    #[test]
    fn v7_ids_are_monotonic_enough() {
        let a = AccountId::new();
        let b = AccountId::new();
        assert!(a <= b);
    }
}
